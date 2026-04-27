//! End-to-end "compute Grusindeks for a location" pipeline.
//!
//! Wired into the CLI by `commands::score`. Lives in its own module so the
//! integration tests can call it directly with a test client.

use anyhow::Result;
use chrono::{DateTime, Duration, NaiveDate, Utc};
use grusindeks_core::daily::{compute_day, BestWindowConfig, Confidence};
use grusindeks_core::drying::{drying_step, DryingParams, SurfaceState};
use grusindeks_core::geo::{sample_around, Point};
use grusindeks_core::lang::Language;
use grusindeks_core::score::{score, ScoreBreakdown};
use grusindeks_core::types::{HourlyConditions, Resolution, RideWindow};
use grusindeks_met::client::MetClient;
use grusindeks_met::frost;
use grusindeks_met::locationforecast;

use crate::aggregate::{
    AggregateScore, DayAggregate, HourScore, HourlyDayAggregate, HourlyForecast, MultiDayForecast,
};

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
    Ok(AggregateScore::from_points(
        inputs.center,
        scored,
        inputs.lang,
    ))
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
    /// How `compute_day` should look for the per-day "best sub-window".
    /// Default: 3-hour windows, only surfaced when ≥10 points above the
    /// day mean. The CLI's `--best-window` flag overrides this with a
    /// user-chosen length and `min_improvement = 0` so every day shows
    /// its top-scoring sub-window.
    pub best_window: BestWindowConfig,
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

    // Project ground state forward day by day. The same `Some(state)` was
    // previously fed to every day's `compute_day`, which meant Friday's
    // bakke pretended `now`'s soil moisture would still hold mid-week.
    // We use the center point's forecast as the single regional
    // trajectory — keeps "ground is shared across the sample disk"
    // semantics and avoids per-bush divergence. `None` (Frost
    // unavailable) propagates through unchanged.
    let day_starts: Vec<DateTime<Utc>> = inputs.days.iter().map(|d| d.window.start).collect();
    // sample_around() truncates every point to 4 decimals before fetching
    // (TOS), but `inputs.center` may carry full precision from --lat/--lon.
    // Compare on the truncated form so a center with 5+ decimals still
    // matches its corresponding fetched point.
    let center_truncated = inputs.center.truncated();
    let center_hours = per_point_hours
        .iter()
        .find(|(p, _)| *p == center_truncated)
        .map(|(_, h)| h.as_slice())
        .unwrap_or_else(|| {
            per_point_hours
                .first()
                .map(|(_, h)| h.as_slice())
                .unwrap_or(&[])
        });
    let projected_per_day: Vec<Option<SurfaceState>> = match surface {
        Some(initial) => {
            project_states_for_days(initial, center_hours, &day_starts, &DryingParams::default())
                .into_iter()
                .map(Some)
                .collect()
        }
        None => vec![None; inputs.days.len()],
    };

    let mut days = Vec::with_capacity(inputs.days.len());
    for (dw, day_surface) in inputs.days.iter().zip(projected_per_day.iter()) {
        let mut day_points = Vec::with_capacity(per_point_hours.len());
        for (p, hours) in &per_point_hours {
            let ds = compute_day(
                hours,
                dw.window,
                *day_surface,
                now,
                inputs.lang,
                inputs.best_window,
            );
            day_points.push((*p, ds));
        }
        days.push(DayAggregate::from_points(
            dw.date,
            dw.window,
            inputs.center,
            day_points,
            inputs.lang,
        ));
    }
    Ok(MultiDayForecast { days })
}

pub struct HourlyInputs<'a> {
    pub center: Point,
    pub radius_km: f64,
    pub days: Vec<DayWindow>,
    pub frost_source_id: Option<&'a str>,
    pub history_hours: i64,
    pub lang: Language,
    /// Local-clock hour values that span the configured daytime window
    /// (e.g. \[10..21\] for a 10:00-22:00 window). Drives the column header
    /// in the rendered grid; the orchestration layer only forwards it.
    pub header_hours: Vec<u8>,
    pub progress: &'a dyn ProgressSink,
}

/// Hourly variant of [`run_forecast`]: for each requested day, score every
/// 1-hour bucket inside the day's clipped ride window and aggregate across
/// the sample disk. Surface state is projected forward hour-by-hour using
/// the centre point's forecast as the regional trajectory — same shape as
/// `run_forecast` so the two views stay consistent on bakke.
pub async fn run_hourly(client: &MetClient, inputs: HourlyInputs<'_>) -> Result<HourlyForecast> {
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

    let center_truncated = inputs.center.truncated();
    let center_hours = per_point_hours
        .iter()
        .find(|(p, _)| *p == center_truncated)
        .map(|(_, h)| h.as_slice())
        .unwrap_or_else(|| {
            per_point_hours
                .first()
                .map(|(_, h)| h.as_slice())
                .unwrap_or(&[])
        });

    // Gather every hour-bucket start time we'll need to score, across all
    // requested days. We dedupe via the natural chronological order: each
    // day's hour starts are strictly later than the previous day's.
    let mut bucket_starts: Vec<DateTime<Utc>> = Vec::new();
    let mut per_day_starts: Vec<Vec<DateTime<Utc>>> = Vec::with_capacity(inputs.days.len());
    for dw in &inputs.days {
        let starts = hour_bucket_starts(dw.window, center_hours);
        bucket_starts.extend_from_slice(&starts);
        per_day_starts.push(starts);
    }

    let projected: Vec<Option<SurfaceState>> = match surface {
        Some(initial) => project_states_for_days(
            initial,
            center_hours,
            &bucket_starts,
            &DryingParams::default(),
        )
        .into_iter()
        .map(Some)
        .collect(),
        None => vec![None; bucket_starts.len()],
    };

    let mut surface_at: std::collections::HashMap<DateTime<Utc>, Option<SurfaceState>> =
        std::collections::HashMap::with_capacity(bucket_starts.len());
    for (t, s) in bucket_starts.iter().zip(projected.into_iter()) {
        surface_at.insert(*t, s);
    }

    let mut days = Vec::with_capacity(inputs.days.len());
    for (dw, hour_starts) in inputs.days.iter().zip(per_day_starts.into_iter()) {
        let mut hours = Vec::with_capacity(hour_starts.len());
        for start in hour_starts {
            let hour_window = RideWindow {
                start,
                end: start + Duration::hours(1),
            };
            let surface_now = surface_at.get(&start).copied().unwrap_or(None);
            let mut totals: Vec<u8> = Vec::with_capacity(per_point_hours.len());
            let mut bd_temp: u32 = 0;
            let mut bd_wind: u32 = 0;
            let mut bd_precip: u32 = 0;
            let mut bd_prob: u32 = 0;
            let mut bd_ground: u32 = 0;
            let mut six_hourly = false;
            for (_, point_hours) in &per_point_hours {
                if let Some(h) = point_hours.iter().find(|h| h.time == start) {
                    if h.resolution == Resolution::SixHourly {
                        six_hourly = true;
                    }
                }
                let s = score(point_hours, hour_window, surface_now, inputs.lang);
                totals.push(s.total);
                bd_temp += u32::from(s.breakdown.temperature);
                bd_wind += u32::from(s.breakdown.wind);
                bd_precip += u32::from(s.breakdown.precipitation);
                bd_prob += u32::from(s.breakdown.precip_probability);
                bd_ground += u32::from(s.breakdown.ground);
            }
            if totals.is_empty() {
                continue;
            }
            let n = totals.len() as u32;
            let min = *totals.iter().min().unwrap();
            let max = *totals.iter().max().unwrap();
            let mean = mean_round(&totals);
            let breakdown = ScoreBreakdown {
                temperature: ((bd_temp + n / 2) / n) as u8,
                wind: ((bd_wind + n / 2) / n) as u8,
                precipitation: ((bd_precip + n / 2) / n) as u8,
                precip_probability: ((bd_prob + n / 2) / n) as u8,
                ground: ((bd_ground + n / 2) / n) as u8,
            };
            let confidence = hour_confidence(start, now, six_hourly);
            hours.push(HourScore {
                time: start,
                mean,
                min,
                max,
                breakdown,
                confidence,
            });
        }
        days.push(HourlyDayAggregate {
            date: dw.date,
            daytime_window: dw.window,
            hours,
        });
    }
    Ok(HourlyForecast {
        header_hours: inputs.header_hours,
        days,
    })
}

/// Round-to-nearest mean for u8 score totals — same behaviour as
/// `aggregate::mean_u8` but kept local to avoid widening that helper's
/// visibility.
fn mean_round(xs: &[u8]) -> u8 {
    let n = xs.len() as u32;
    let sum: u32 = xs.iter().map(|&v| u32::from(v)).sum();
    ((sum + n / 2) / n) as u8
}

/// Confidence for a single hour bucket. Mirrors `daily::confidence_for`
/// but at hour granularity: a 6-hourly resolution flag on the bucket is
/// enough to drop confidence to `Lav`, since the score is then derived
/// from a 6-h smear rather than the actual hour. Buckets more than 5
/// days out drop to `Lav` regardless.
fn hour_confidence(start: DateTime<Utc>, now: DateTime<Utc>, six_hourly: bool) -> Confidence {
    let horizon = start - now;
    if six_hourly || horizon > Duration::days(5) {
        return Confidence::Lav;
    }
    if horizon < Duration::days(1) {
        Confidence::Hoy
    } else {
        Confidence::Middels
    }
}

/// Hour bucket starts that fall fully inside `window`. Anchored to the
/// forecast's hour timestamps (typically HH:00 UTC) rather than to
/// `window.start` directly, so a clipped today (window starts at e.g.
/// 14:23) still produces well-aligned buckets at 15:00, 16:00, …, and
/// the partial leading hour is dropped instead of being silently scored
/// against half its data.
fn hour_bucket_starts(
    window: RideWindow,
    forecast_hours: &[HourlyConditions],
) -> Vec<DateTime<Utc>> {
    forecast_hours
        .iter()
        .filter(|h| {
            let bucket_end = h.time + Duration::hours(1);
            h.time >= window.start && bucket_end <= window.end
        })
        .map(|h| h.time)
        .collect()
}

/// Walk `hours` (chronological) forward from `initial`, snapshotting the
/// drying state at every entry in `day_starts` (also chronological). The
/// snapshot is the state *at the start of* the snapshot hour — the rain
/// in the hour beginning exactly at `snapshot` has not been applied yet,
/// since by convention `drying_step` advances by exactly one hour.
///
/// Forecast gaps (rare — locationforecast already spreads 6h buckets to
/// hourly records) are tolerated by simply skipping. Adding synthetic
/// dry filler the way `replay_into_state` does would over-credit drying
/// across a missing block; better to under-credit and accept the small
/// drift.
fn project_states_for_days(
    initial: SurfaceState,
    hours: &[HourlyConditions],
    day_starts: &[DateTime<Utc>],
    params: &DryingParams,
) -> Vec<SurfaceState> {
    let mut out = Vec::with_capacity(day_starts.len());
    let mut state = initial;
    let mut idx = 0;
    for &snapshot in day_starts {
        while idx < hours.len() && hours[idx].time < snapshot {
            state = drying_step(state, &hours[idx], params);
            idx += 1;
        }
        out.push(state);
    }
    out
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
        match joined {
            Ok(Ok((p, hours))) => {
                out.push((p, hours));
                progress.forecast_point_done();
            }
            // First failure wins. Abort the remaining fan-out tasks so we
            // don't sit through up to 15s of timeouts on requests we no
            // longer care about, then surface the original error.
            Ok(Err(e)) => {
                set.abort_all();
                progress.forecast_finished();
                return Err(e);
            }
            Err(join_err) => {
                set.abort_all();
                progress.forecast_finished();
                return Err(join_err.into());
            }
        }
    }
    progress.forecast_finished();
    // JoinSet yields tasks in completion order — i.e. whichever HTTP
    // request finished first. Sort by (lat, lon) so downstream aggregation
    // picks the same "worst point" / "best point" on every run when the
    // sample disk has score ties, and so the JSON `points[]` array is
    // stable across invocations.
    out.sort_by(|(a, _), (b, _)| {
        a.lat
            .partial_cmp(&b.lat)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                a.lon
                    .partial_cmp(&b.lon)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });
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

    // ---- project_states_for_days ----

    fn ts(h: i64) -> DateTime<Utc> {
        chrono::TimeZone::with_ymd_and_hms(&Utc, 2026, 4, 27, 0, 0, 0).unwrap() + Duration::hours(h)
    }

    fn dry_hour(h: i64) -> HourlyConditions {
        let mut hc = HourlyConditions::minimal(ts(h), 15.0, 3.0, 0.0);
        hc.relative_humidity = Some(60.0);
        hc.cloud_area_fraction = Some(50.0);
        hc
    }

    fn rainy_hour(h: i64, mm: f64) -> HourlyConditions {
        let mut hc = HourlyConditions::minimal(ts(h), 10.0, 2.0, mm);
        hc.relative_humidity = Some(90.0);
        hc.cloud_area_fraction = Some(95.0);
        hc
    }

    #[test]
    fn projection_returns_initial_state_for_today_when_first_snapshot_is_now() {
        // Day 1 snapshot equals first forecast hour → no hours stepped,
        // state == initial. Mirrors today's "i dag" window which is
        // clipped to `now`.
        let initial = SurfaceState {
            accumulated_mm: 1.5,
            hours_since_meaningful_rain: 12.0,
        };
        let hours: Vec<_> = (0..24).map(dry_hour).collect();
        let states = project_states_for_days(initial, &hours, &[ts(0)], &DryingParams::default());
        assert_eq!(states, vec![initial]);
    }

    #[test]
    fn projection_dries_ground_over_consecutive_dry_days() {
        let initial = SurfaceState {
            accumulated_mm: 2.0,
            hours_since_meaningful_rain: 0.0,
        };
        // 4 dry days (96 hours), snapshot at start of each day.
        let hours: Vec<_> = (0..96).map(dry_hour).collect();
        let day_starts = vec![ts(0), ts(24), ts(48), ts(72)];
        let states =
            project_states_for_days(initial, &hours, &day_starts, &DryingParams::default());
        assert_eq!(states.len(), 4);
        // accumulated_mm strictly decreases day over day with no rain.
        for w in states.windows(2) {
            assert!(
                w[1].accumulated_mm <= w[0].accumulated_mm,
                "ground should not gain water without rain: {w:?}"
            );
        }
        // Drought counter strictly climbs.
        assert!(states[3].hours_since_meaningful_rain > states[0].hours_since_meaningful_rain);
    }

    #[test]
    fn projection_wets_ground_after_forecast_rain() {
        let initial = SurfaceState::default();
        // Day 0 dry, day 1 has heavy rain hours 24..30, day 2 dry again.
        let mut hours: Vec<HourlyConditions> = (0..24).map(dry_hour).collect();
        hours.extend((24..30).map(|h| rainy_hour(h, 2.0)));
        hours.extend((30..72).map(dry_hour));
        let day_starts = vec![ts(0), ts(24), ts(48)];
        let states =
            project_states_for_days(initial, &hours, &day_starts, &DryingParams::default());
        assert_eq!(states[0].accumulated_mm, 0.0, "today: no rain yet");
        assert_eq!(
            states[1].accumulated_mm, 0.0,
            "tomorrow morning: rain hasn't fallen yet (snapshot at start)"
        );
        assert!(
            states[2].accumulated_mm > 0.0,
            "day after tomorrow: rain has fallen, ground is wet — got {:?}",
            states[2]
        );
    }

    #[test]
    fn projection_handles_snapshot_before_first_hour() {
        // Snapshot earlier than any forecast hour → no stepping, state
        // stays at initial. Defensive against forecast that starts after
        // the requested day window.
        let initial = SurfaceState {
            accumulated_mm: 0.5,
            hours_since_meaningful_rain: 30.0,
        };
        let hours: Vec<_> = (10..20).map(dry_hour).collect();
        let states = project_states_for_days(initial, &hours, &[ts(0)], &DryingParams::default());
        assert_eq!(states, vec![initial]);
    }

    #[test]
    fn projection_returns_one_state_per_day_start() {
        let initial = SurfaceState::default();
        let hours: Vec<_> = (0..96).map(dry_hour).collect();
        let day_starts = vec![ts(0), ts(24), ts(48), ts(72)];
        let states =
            project_states_for_days(initial, &hours, &day_starts, &DryingParams::default());
        assert_eq!(states.len(), day_starts.len());
    }
}
