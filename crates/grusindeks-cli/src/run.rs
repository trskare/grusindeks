//! End-to-end "compute Grusindeks for a location" pipeline.
//!
//! Wired into the CLI by `commands::score`. Lives in its own module so the
//! integration tests can call it directly with a test client.

use anyhow::Result;
use chrono::{DateTime, Duration, NaiveDate, Utc};
use grusindeks_core::daily::compute_day;
use grusindeks_core::drying::{drying_step, DryingParams, SurfaceState};
use grusindeks_core::geo::{sample_around, Point};
use grusindeks_core::lang::Language;
use grusindeks_core::score::score;
use grusindeks_core::types::{HourlyConditions, Resolution, RideWindow};
use grusindeks_met::client::MetClient;
use grusindeks_met::frost;
use grusindeks_met::locationforecast;

use crate::aggregate::{AggregateScore, DayAggregate, MultiDayForecast};

/// Callbacks the orchestration layer fires as it walks through the
/// fetch/score pipeline. The CLI binds these to an `indicatif`-backed
/// terminal renderer; tests bind them to a recording sink (or skip them
/// entirely with `NoopProgress`). All methods have no-op defaults so
/// callers only override the events they care about.
///
/// Lives in the CLI crate on purpose — `grusindeks-met` stays unaware of
/// presentation. Notifications are coarse-grained (start/finish per
/// stage, plus a per-point tick on the forecast fan-out) since that's
/// the granularity the user can perceive.
pub trait ProgressSink: Send + Sync {
    /// We're about to call Frost for historical precipitation. Only fires
    /// when Frost is actually configured for this location.
    fn ground_started(&self) {}
    /// Frost call done. `found = false` means it failed and we're
    /// falling back to dry-ground assumption.
    fn ground_finished(&self, _found: bool) {}
    /// We're about to fan out `total` parallel locationforecast fetches.
    fn forecast_started(&self, _total: usize) {}
    /// One forecast fetch completed (cache-hit or network).
    fn forecast_point_done(&self) {}
    /// All forecast fetches finished, scoring is starting.
    fn forecast_finished(&self) {}
}

pub struct ScoreInputs<'a> {
    pub center: Point,
    pub radius_km: f64,
    pub window: RideWindow,
    pub frost_source_id: Option<&'a str>,
    /// How many hours of past observations to feed into the drying model.
    pub history_hours: i64,
    /// Language for human-readable labels and penalty messages.
    pub lang: Language,
    pub progress: &'a dyn ProgressSink,
}

/// Fetch every input we need and compute one Grusindeks per sample point.
pub async fn run_score(client: &MetClient, inputs: ScoreInputs<'_>) -> Result<AggregateScore> {
    let points = sample_around(inputs.center, inputs.radius_km);

    // `None` is propagated all the way through scoring so the ground axis
    // can render as "ukjent" instead of silently masquerading as
    // "akkurat regnet" (which is what `SurfaceState::default()` would
    // imply).
    let surface = fetch_ground_state(
        client,
        inputs.frost_source_id,
        inputs.history_hours,
        inputs.progress,
    )
    .await;

    let per_point_hours = fetch_forecasts_parallel(client, &points, inputs.progress).await?;
    let scored: Vec<(Point, _)> = per_point_hours
        .into_iter()
        .map(|(p, hours)| (p, score(&hours, inputs.window, surface, inputs.lang)))
        .collect();
    Ok(AggregateScore::from_points(inputs.center, scored))
}

/// One day worth of forecast lookup: a local date label plus the UTC
/// window to score. The CLI builds these from local "ride hours" and
/// hands them down so this layer doesn't need timezone knowledge.
pub struct DayWindow {
    pub date: NaiveDate,
    pub window: RideWindow,
}

pub struct ForecastInputs<'a> {
    pub center: Point,
    pub radius_km: f64,
    pub days: Vec<DayWindow>,
    pub frost_source_id: Option<&'a str>,
    pub history_hours: i64,
    /// Language for human-readable labels and penalty messages.
    pub lang: Language,
    pub progress: &'a dyn ProgressSink,
}

/// Multi-day variant of [`run_score`]. Fetches the forecast once per
/// sample point (the response already covers the full 9-day horizon),
/// then computes a `DayAggregate` per requested day.
pub async fn run_forecast(
    client: &MetClient,
    inputs: ForecastInputs<'_>,
) -> Result<MultiDayForecast> {
    let points = sample_around(inputs.center, inputs.radius_km);

    let surface = fetch_ground_state(
        client,
        inputs.frost_source_id,
        inputs.history_hours,
        inputs.progress,
    )
    .await;

    let now = Utc::now();
    let per_point_hours = fetch_forecasts_parallel(client, &points, inputs.progress).await?;

    let mut days = Vec::with_capacity(inputs.days.len());
    for dw in inputs.days {
        let mut day_points = Vec::with_capacity(per_point_hours.len());
        for (p, hours) in &per_point_hours {
            let ds = compute_day(hours, dw.window, surface, now, inputs.lang);
            day_points.push((*p, ds));
        }
        days.push(DayAggregate::from_points(
            dw.date,
            dw.window,
            inputs.center,
            day_points,
        ));
    }
    Ok(MultiDayForecast { days })
}

/// Fan out one `locationforecast` request per sample point in parallel.
/// Each completion notifies `progress`; the bar fills as responses
/// arrive, regardless of completion order.
async fn fetch_forecasts_parallel(
    client: &MetClient,
    points: &[Point],
    progress: &dyn ProgressSink,
) -> Result<Vec<(Point, Vec<HourlyConditions>)>> {
    progress.forecast_started(points.len());
    let mut set = tokio::task::JoinSet::new();
    for &p in points {
        let client = client.clone();
        set.spawn(async move {
            let f = locationforecast::fetch(&client, p).await?;
            Ok::<(Point, Vec<HourlyConditions>), anyhow::Error>((p, f.hours))
        });
    }
    let mut out = Vec::with_capacity(points.len());
    while let Some(joined) = set.join_next().await {
        let (p, hours) = joined??;
        out.push((p, hours));
        progress.forecast_point_done();
    }
    progress.forecast_finished();
    Ok(out)
}

async fn fetch_ground_state(
    client: &MetClient,
    frost_source_id: Option<&str>,
    history_hours: i64,
    progress: &dyn ProgressSink,
) -> Option<SurfaceState> {
    let src = frost_source_id?;
    client.config().frost_client_id.as_ref()?;
    progress.ground_started();
    let to: DateTime<Utc> = Utc::now();
    let from = to - Duration::hours(history_hours);
    let result = frost::fetch_hourly_observations(client, src, from, to).await;
    let outcome = match &result {
        Ok(history) => Some(replay_into_state(history, &DryingParams::default())),
        Err(e) => {
            tracing::warn!("frost lookup failed, ground state will be reported as unknown: {e}",);
            None
        }
    };
    progress.ground_finished(outcome.is_some());
    outcome
}

/// Replay a list of Frost observations through the drying model. Uses
/// the actual observed temperature, wind, and humidity per hour when
/// they're present; falls back to the previous neutral defaults
/// (10 °C / 3 m/s / 70 % RH / 70 % cloud / no UV) only for the
/// individual fields that are missing on the station. This way a real
/// hot, sunny week dries the bucket faster than a cold, calm one — the
/// bug the auditors flagged about the ground estimate diverging from
/// reality on hetebølge / kjølig overskyet uke.
///
/// Gap-aware: when consecutive observations are more than ~1.5 hours
/// apart (Frost outage, sensor maintenance, station reboot) we
/// synthesise dry filler hours so the drought counter climbs by the real
/// elapsed time instead of treating the gap as if no time passed at all.
/// Filler hours use neutral conditions and zero precipitation.
fn replay_into_state(history: &[frost::HourlyObservation], p: &DryingParams) -> SurfaceState {
    let mut state = SurfaceState::default();
    let mut prev_time: Option<DateTime<Utc>> = None;
    for h in history {
        if let Some(prev) = prev_time {
            let elapsed = h.time - prev;
            // Allow some slop — Frost timestamps drift by a few minutes
            // around the hour, so anything under 90 minutes counts as
            // "the next hour" rather than a gap.
            let gap_hours = elapsed.num_minutes() as f64 / 60.0;
            if gap_hours > 1.5 {
                let filler_count = gap_hours.round() as i64 - 1;
                for i in 1..=filler_count {
                    let filler = HourlyConditions {
                        time: prev + Duration::hours(i),
                        temperature_c: 10.0,
                        wind_speed_ms: 3.0,
                        precipitation_mm: 0.0,
                        wind_gust_ms: None,
                        wind_from_deg: None,
                        probability_of_precip: None,
                        relative_humidity: Some(70.0),
                        cloud_area_fraction: Some(70.0),
                        uv_index_clear_sky: None,
                        resolution: Resolution::Hourly,
                    };
                    state = drying_step(state, &filler, p);
                }
            }
        }
        let synth = HourlyConditions {
            time: h.time,
            temperature_c: h.temp_c.unwrap_or(10.0),
            wind_speed_ms: h.wind_ms.unwrap_or(3.0),
            precipitation_mm: h.precip_mm.unwrap_or(0.0),
            wind_gust_ms: None,
            wind_from_deg: None,
            probability_of_precip: None,
            relative_humidity: h.humidity_pct.or(Some(70.0)),
            cloud_area_fraction: Some(70.0),
            uv_index_clear_sky: None,
            resolution: Resolution::Hourly,
        };
        state = drying_step(state, &synth, p);
        prev_time = Some(h.time);
    }
    state
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Records every event for assertion. Order matters because the CLI
    /// renderer depends on it (e.g. `forecast_started` before any tick).
    #[derive(Default)]
    pub(crate) struct RecordingProgress {
        pub(crate) events: Mutex<Vec<&'static str>>,
        pub(crate) forecast_total: Mutex<Option<usize>>,
    }

    impl ProgressSink for RecordingProgress {
        fn ground_started(&self) {
            self.events.lock().unwrap().push("ground_started");
        }
        fn ground_finished(&self, _: bool) {
            self.events.lock().unwrap().push("ground_finished");
        }
        fn forecast_started(&self, total: usize) {
            *self.forecast_total.lock().unwrap() = Some(total);
            self.events.lock().unwrap().push("forecast_started");
        }
        fn forecast_point_done(&self) {
            self.events.lock().unwrap().push("forecast_point_done");
        }
        fn forecast_finished(&self) {
            self.events.lock().unwrap().push("forecast_finished");
        }
    }

    #[tokio::test]
    async fn run_score_fires_progress_events_in_order() {
        use grusindeks_met::client::{MetClient, MetClientConfig, UserAgent};
        use wiremock::matchers::{method, path as path_m};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let fixture = include_str!("../../../fixtures/locationforecast_oslo.json");
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_m("/weatherapi/locationforecast/2.0/complete"))
            .respond_with(ResponseTemplate::new(200).set_body_string(fixture))
            .mount(&server)
            .await;

        let ua = UserAgent::new("grusindeks-test", "0.1.0", "dev@example.invalid").unwrap();
        let mut cfg = MetClientConfig::production(ua, None);
        cfg.api_base = format!("{}/", server.uri()).parse().unwrap();
        let client = MetClient::new(cfg).unwrap();

        let progress = RecordingProgress::default();
        let center = Point::new(59.9139, 10.7522);
        let win = RideWindow::from_hours(Utc::now(), 3);

        let _ = run_score(
            &client,
            ScoreInputs {
                center,
                radius_km: 20.0,
                window: win,
                frost_source_id: None, // Frost not configured → no ground events
                history_hours: 168,
                lang: Language::Norwegian,
                progress: &progress,
            },
        )
        .await
        .unwrap();

        let events = progress.events.lock().unwrap();
        assert!(
            !events.iter().any(|e| e.starts_with("ground_")),
            "ground events should not fire when Frost is not configured: {events:?}"
        );
        assert_eq!(events.first(), Some(&"forecast_started"));
        assert_eq!(events.last(), Some(&"forecast_finished"));
        let total = progress.forecast_total.lock().unwrap().unwrap();
        let ticks = events
            .iter()
            .filter(|e| **e == "forecast_point_done")
            .count();
        assert_eq!(ticks, total, "one tick per fanned-out point");
    }

    #[test]
    fn empty_history_yields_dry_ground() {
        let state = replay_into_state(&[], &DryingParams::default());
        assert_eq!(state, SurfaceState::default());
    }

    #[test]
    fn heavy_recent_history_leaves_water_on_ground() {
        let now = Utc::now();
        let history: Vec<frost::HourlyObservation> = (0..6)
            .map(|i| frost::HourlyObservation {
                time: now - Duration::hours(6 - i),
                precip_mm: Some(3.0),
                temp_c: None,
                wind_ms: None,
                humidity_pct: None,
            })
            .collect();
        let state = replay_into_state(&history, &DryingParams::default());
        assert!(
            state.accumulated_mm > 0.0,
            "expected some accumulation, got {:?}",
            state,
        );
    }

    #[test]
    fn replay_treats_gaps_in_history_as_dry_elapsed_time() {
        // Two readings 6 hours apart simulate a Frost outage. The
        // drought counter must climb by the elapsed time, not by 1.
        let now = Utc::now();
        let history = vec![
            frost::HourlyObservation {
                time: now - Duration::hours(6),
                precip_mm: Some(0.0),
                temp_c: Some(15.0),
                wind_ms: Some(3.0),
                humidity_pct: Some(50.0),
            },
            frost::HourlyObservation {
                time: now,
                precip_mm: Some(0.0),
                temp_c: Some(15.0),
                wind_ms: Some(3.0),
                humidity_pct: Some(50.0),
            },
        ];
        let state = replay_into_state(&history, &DryingParams::default());
        // 1 first reading + 5 filler hours + 1 last reading = 7 dry hours
        // since the start. (Counter is "since meaningful rain", and
        // there's been none, so it equals the total elapsed time.)
        assert!(
            state.hours_since_meaningful_rain >= 6.0,
            "drought counter must reflect elapsed gap; got {}",
            state.hours_since_meaningful_rain,
        );
    }

    #[test]
    fn replay_uses_observed_conditions_not_synthesised_neutral() {
        // Two histories with identical precipitation but very different
        // weather should NOT produce identical drying. Hot/sunny dries
        // faster than cold/cloudy (still, no UV in observations →
        // sunshine bonus is half-strength either way; cloud is the same).
        // The dominant effect is temperature and wind.
        let now = Utc::now();
        let dry_history: Vec<frost::HourlyObservation> = (0..6)
            .map(|i| frost::HourlyObservation {
                time: now - Duration::hours(6 - i),
                precip_mm: Some(1.0),
                temp_c: Some(25.0),
                wind_ms: Some(8.0),
                humidity_pct: Some(40.0),
            })
            .collect();
        let damp_history: Vec<frost::HourlyObservation> = (0..6)
            .map(|i| frost::HourlyObservation {
                time: now - Duration::hours(6 - i),
                precip_mm: Some(1.0),
                temp_c: Some(2.0),
                wind_ms: Some(0.5),
                humidity_pct: Some(95.0),
            })
            .collect();
        let p = DryingParams::default();
        let dry_state = replay_into_state(&dry_history, &p);
        let damp_state = replay_into_state(&damp_history, &p);
        assert!(
            dry_state.accumulated_mm < damp_state.accumulated_mm,
            "hot/windy/dry-air history should leave less water on the surface; got dry={}, damp={}",
            dry_state.accumulated_mm,
            damp_state.accumulated_mm,
        );
    }
}
