//! End-to-end "compute Grusindeks for a location" pipeline.
//!
//! Wired into the CLI by `commands::score`. Lives in its own module so the
//! integration tests can call it directly with a test client.

use anyhow::Result;
use chrono::{DateTime, Duration, Local, NaiveDate, Utc};
use grusindeks_core::daily::{compute_day, BestWindowConfig, Confidence};
use grusindeks_core::drying::{drying_step, DryingParams, SurfaceState, SURFACE_WETTED_MM};
use grusindeks_core::geo::{sample_around, Point};
use grusindeks_core::lang::Language;
use grusindeks_core::score::{score, ScoreBreakdown};
use grusindeks_core::types::{HourlyConditions, Resolution, RideWindow};
use grusindeks_met::client::MetClient;
use grusindeks_met::frost;
use grusindeks_met::locationforecast;
use grusindeks_met::nowcast::{self, Nowcast};

use crate::aggregate::{
    AggregateScore, DayAggregate, HourScore, HourlyDayAggregate, HourlyForecast, MultiDayForecast,
    NowcastAlert, RainHistory,
};

/// Threshold for "this hour got rain on radar" — matches yr.no's
/// nedbørsvarsel which surfaces from ~0.1 mm/h. Used both for the alert
/// banner and to decide whether nowcast bumps `probability_of_precip` to
/// 95% on a forecast hour.
pub(crate) const NOWCAST_ALERT_THRESHOLD_MM_H: f64 = 0.1;

/// Probability written into `HourlyConditions::probability_of_precip` on
/// hours where nowcast saw rain ≥ threshold. Radar observation is near
/// certainty; we don't pin to 100 because the 5-minute-step extrapolation
/// can drift on the 90–120 min tail.
pub(crate) const NOWCAST_OVERRIDE_PROBABILITY_PCT: f64 = 95.0;

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
    /// We're about to fetch the radar nowcast (Norden only). Only fires
    /// when nowcast was requested for this run.
    fn nowcast_started(&self) {}
    /// Nowcast call done. `found = false` means the request failed or the
    /// location has no radar coverage; the run continues without alert.
    fn nowcast_finished(&self, _found: bool) {}
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
    /// Fetch the radar nowcast and apply its rain overrides to the next
    /// ~2 h of forecast hours. CLI sets this when the requested window
    /// starts within the nowcast horizon (see `cmd_score`).
    pub fetch_nowcast: bool,
    pub progress: &'a dyn ProgressSink,
}

/// Fetch every input we need and compute one Grusindeks per sample point.
pub async fn run_score(client: &MetClient, inputs: ScoreInputs<'_>) -> Result<AggregateScore> {
    let points = sample_around(inputs.center, inputs.radius_km);

    // `None` is propagated all the way through scoring so the ground axis
    // can render as "ukjent" instead of silently masquerading as
    // "akkurat regnet" (which is what `SurfaceState::default()` would
    // imply).
    let (surface, rain_history) = fetch_ground_state(
        client,
        inputs.frost_source_id,
        inputs.history_hours,
        inputs.progress,
    )
    .await;

    let mut per_point_hours = fetch_forecasts_parallel(client, &points, inputs.progress).await?;
    let nowcast =
        fetch_nowcast_optional(client, inputs.center, inputs.fetch_nowcast, inputs.progress).await;
    if let Some(nc) = &nowcast {
        apply_nowcast_overrides(&mut per_point_hours, nc);
    }
    let scored: Vec<(Point, _)> = per_point_hours
        .into_iter()
        .map(|(p, hours)| (p, score(&hours, inputs.window, surface, inputs.lang)))
        .collect();
    let mut out = AggregateScore::from_points(inputs.center, scored, inputs.lang);
    out.rain_history = rain_history;
    out.nowcast_alert = nowcast
        .as_ref()
        .and_then(|n| build_nowcast_alert(n, Utc::now()));
    Ok(out)
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
    /// See [`ScoreInputs::fetch_nowcast`].
    pub fetch_nowcast: bool,
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

    let (surface, rain_history) = fetch_ground_state(
        client,
        inputs.frost_source_id,
        inputs.history_hours,
        inputs.progress,
    )
    .await;

    let now = Utc::now();
    let mut per_point_hours = fetch_forecasts_parallel(client, &points, inputs.progress).await?;
    let nowcast =
        fetch_nowcast_optional(client, inputs.center, inputs.fetch_nowcast, inputs.progress).await;
    if let Some(nc) = &nowcast {
        apply_nowcast_overrides(&mut per_point_hours, nc);
    }

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
                inputs.best_window.clone(),
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
    Ok(MultiDayForecast {
        days,
        rain_history,
        nowcast_alert: nowcast.as_ref().and_then(|n| build_nowcast_alert(n, now)),
    })
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
    /// See [`ScoreInputs::fetch_nowcast`].
    pub fetch_nowcast: bool,
    pub progress: &'a dyn ProgressSink,
}

/// Hourly variant of [`run_forecast`]: for each requested day, score every
/// 1-hour bucket inside the day's clipped ride window and aggregate across
/// the sample disk. Surface state is projected forward hour-by-hour using
/// the centre point's forecast as the regional trajectory — same shape as
/// `run_forecast` so the two views stay consistent on bakke.
pub async fn run_hourly(client: &MetClient, inputs: HourlyInputs<'_>) -> Result<HourlyForecast> {
    let points = sample_around(inputs.center, inputs.radius_km);

    let (surface, rain_history) = fetch_ground_state(
        client,
        inputs.frost_source_id,
        inputs.history_hours,
        inputs.progress,
    )
    .await;

    let now = Utc::now();
    let mut per_point_hours = fetch_forecasts_parallel(client, &points, inputs.progress).await?;
    let nowcast =
        fetch_nowcast_optional(client, inputs.center, inputs.fetch_nowcast, inputs.progress).await;
    if let Some(nc) = &nowcast {
        apply_nowcast_overrides(&mut per_point_hours, nc);
    }

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
        // Centre-point WindowStats over the *full* daytime window — the
        // hourly grid's per-bucket scoring above doesn't surface temp/precip
        // ranges, so we run one extra `score()` call against the centre's
        // full-day window to populate them. Cheap (one fold per day) and
        // keeps the per-day Tall row consistent with the multi-day view.
        let stats = if hours.is_empty() {
            None
        } else {
            // Surface state at the day's start is the most representative
            // snapshot for the day; per-hour drift is small and we'd need
            // to pick *some* anchor anyway.
            let day_surface = surface_at.get(&dw.window.start).copied().unwrap_or(None);
            let s = score(center_hours, dw.window, day_surface, inputs.lang);
            (!s.stats.is_empty()).then_some(s.stats)
        };
        days.push(HourlyDayAggregate {
            date: dw.date,
            daytime_window: dw.window,
            hours,
            stats,
        });
    }
    Ok(HourlyForecast {
        header_hours: inputs.header_hours,
        days,
        rain_history,
        nowcast_alert: nowcast.as_ref().and_then(|n| build_nowcast_alert(n, now)),
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

/// Apply radar-based overrides to the per-point forecast. Mutates each
/// `HourlyConditions` whose hour bucket overlaps the nowcast window so
/// the score reflects the **most pessimistic** of the two sources:
///
/// 1. `precipitation_mm` ← `max(locationforecast_mm, sum(rate)/12)` —
///    nowcast steps are 5-min apart, so 12 of them cover an hour. We use
///    sum-of-rates rather than mean × 1h so partial coverage (nowcast
///    runs out 90 min in, only 6 steps fall in the last bucket) shows up
///    as "rain only fell for half the hour" instead of being projected
///    forward. The `max()` against locationforecast keeps us from
///    *underestimating* a bucket where radar happens to dip but the
///    longer-range forecast still expects rain.
///
/// 2. `probability_of_precip` ← `max(existing, 95)` when **any** step in
///    the bucket is at or above [`NOWCAST_ALERT_THRESHOLD_MM_H`]. A 5-min
///    burst on radar means rain definitely fell in that hour, even if
///    `nc_mm` rounds small.
///
/// No-op when `radar_coverage_ok` is false or `steps` is empty (we just
/// got a "no coverage" response — bail and let locationforecast stand
/// alone). Radar bucket geometry is regional, so we apply the same
/// nowcast to **every** sample point's hours.
pub(crate) fn apply_nowcast_overrides(
    per_point_hours: &mut [(Point, Vec<HourlyConditions>)],
    nowcast: &Nowcast,
) {
    if !nowcast.radar_coverage_ok || nowcast.steps.is_empty() {
        return;
    }
    // Bracket the nowcast window for an early skip — most forecast hours
    // sit far outside it.
    let nc_first = nowcast.steps.first().expect("non-empty checked above").time;
    let nc_last_step = nowcast.steps.last().expect("non-empty checked above").time;
    // Each step represents the rate at a 5-min mark; cover the trailing
    // 5-min interval too so a step at 14:55 still feeds into a 14:00–15:00
    // bucket.
    let nc_end = nc_last_step + Duration::minutes(5);

    for (_, hours) in per_point_hours.iter_mut() {
        for h in hours.iter_mut() {
            let bucket_end = h.time + Duration::hours(1);
            if bucket_end <= nc_first || h.time >= nc_end {
                continue;
            }
            let mut sum_rate = 0.0_f64;
            let mut peak_rate = 0.0_f64;
            let mut count: u32 = 0;
            for s in &nowcast.steps {
                if s.time >= h.time && s.time < bucket_end {
                    sum_rate += s.precipitation_rate_mm_h;
                    if s.precipitation_rate_mm_h > peak_rate {
                        peak_rate = s.precipitation_rate_mm_h;
                    }
                    count += 1;
                }
            }
            if count == 0 {
                continue;
            }
            // 12 5-min steps fill an hour → rate sum × (5/60) = sum / 12 mm.
            let nc_mm = sum_rate / 12.0;
            if nc_mm > h.precipitation_mm {
                h.precipitation_mm = nc_mm;
            }
            if peak_rate >= NOWCAST_ALERT_THRESHOLD_MM_H {
                let new_prob = NOWCAST_OVERRIDE_PROBABILITY_PCT;
                h.probability_of_precip = Some(match h.probability_of_precip {
                    Some(existing) if existing > new_prob => existing,
                    _ => new_prob,
                });
            }
        }
    }
}

/// Build a presentation-ready alert from a [`Nowcast`]. `None` when the
/// series is dry, has no radar coverage, or starts entirely in the past
/// (nowcast occasionally surfaces a step or two with `time` already
/// elapsed by the time we render — surfacing those would say "regn om
/// -3 min", which is just confusing).
pub(crate) fn build_nowcast_alert(nowcast: &Nowcast, now: DateTime<Utc>) -> Option<NowcastAlert> {
    if !nowcast.radar_coverage_ok {
        return None;
    }
    let summary = nowcast.summary(NOWCAST_ALERT_THRESHOLD_MM_H)?;
    if summary.last_rain_at < now {
        return None;
    }
    Some(NowcastAlert {
        first_rain_at: summary.first_rain_at,
        last_rain_at: summary.last_rain_at,
        peak_at: summary.peak_at,
        peak_mm_h: summary.peak_mm_h,
    })
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

/// Fetch the radar nowcast for `center` when the caller asked for it.
/// Failure modes (network error, no radar coverage, decode error) are
/// **non-fatal**: we log and return `None` so the run continues with
/// locationforecast alone. Nowcast is a best-effort enhancement, not a
/// dependency of the core score.
async fn fetch_nowcast_optional(
    client: &MetClient,
    center: Point,
    fetch: bool,
    progress: &dyn ProgressSink,
) -> Option<Nowcast> {
    if !fetch {
        return None;
    }
    progress.nowcast_started();
    let result = nowcast::fetch(client, center).await;
    let outcome = match result {
        Ok(n) if n.radar_coverage_ok => Some(n),
        Ok(_) => {
            tracing::info!(
                "nowcast: no radar coverage for ({:.4}, {:.4}); skipping radar overrides",
                center.lat,
                center.lon,
            );
            None
        }
        Err(e) => {
            tracing::warn!("nowcast lookup failed, radar overrides skipped: {e}");
            None
        }
    };
    progress.nowcast_finished(outcome.is_some());
    outcome
}

async fn fetch_ground_state(
    client: &MetClient,
    frost_source_id: Option<&str>,
    history_hours: i64,
    progress: &dyn ProgressSink,
) -> (Option<SurfaceState>, Option<RainHistory>) {
    let Some(src) = frost_source_id else {
        return (None, None);
    };
    if client.config().frost_client_id.is_none() {
        return (None, None);
    }
    progress.ground_started();
    let to: DateTime<Utc> = Utc::now();
    let from = to - Duration::hours(history_hours);
    let result = frost::fetch_hourly_observations(client, src, from, to).await;
    let outcome = match &result {
        Ok(history) => {
            let surface = replay_into_state(history, &DryingParams::default());
            let rain = compute_rain_history(history, history_hours);
            (Some(surface), rain)
        }
        Err(e) => {
            tracing::warn!("frost lookup failed, ground state will be reported as unknown: {e}",);
            (None, None)
        }
    };
    progress.ground_finished(outcome.0.is_some());
    outcome
}

/// Roll a list of Frost observations into a [`RainHistory`] aggregate.
///
/// Days are bucketed by **system local date** (`chrono::Local`) so a
/// Saturday-evening downpour shows up under Saturday in Monday-morning
/// reports rather than as an UTC-Sunday surprise.
///
/// `rain_days` counts distinct local dates where **the drying model's
/// post-tørking accumulator actually crossed [`SURFACE_WETTED_MM`]**.
/// This is the *same* condition the Bakke chip's drought counter uses
/// to reset, so Regn 7d and Bakke can never disagree: if Bakke says
/// "7 døgn uten regn", Regn 7d sees zero regndøgn over the matching
/// window. A naive raw-precipitation threshold (≥0.3 mm in any hour)
/// would still mis-count a 0.3 mm hour that got eaten by drying on a
/// hot windy day, leaving the user with "7 døgn uten regn" alongside
/// "1 regndøgn" — exactly what triggered this fix.
///
/// `total_mm` and `wettest_day` are restricted to the regndøgn-set, so
/// every number on the chip describes rain that actually mattered. Sub-
/// threshold drizzle that came and went without wetting the surface is
/// excluded from the totals (it would otherwise show up as "0.4 mm siste
/// 7 d" with zero regndøgn — confusing).
///
/// Returns `None` only when the call produced no usable observations at
/// all. A non-empty-but-no-rain period yields `Some` with `total_mm = 0.0`
/// and `rain_days = 0`; the renderer collapses that to a "tørt siste 7
/// døgn" chip rather than hiding the row entirely (the user *did* check,
/// and a dry week is the answer).
pub fn compute_rain_history(
    history: &[frost::HourlyObservation],
    lookback_hours: i64,
) -> Option<RainHistory> {
    if history.is_empty() {
        return None;
    }
    let params = DryingParams::default();

    // Walk the drying model and collect every local date where the
    // post-drying accumulator reached SURFACE_WETTED_MM at least once.
    // Same gap-fill semantics as `replay_into_state` (the source of truth
    // for the Bakke chip's drought counter), so the two views stay in
    // lock-step by construction.
    let mut wet_days = std::collections::BTreeSet::<NaiveDate>::new();
    replay_for_each(history, &params, |hc, state| {
        if state.accumulated_mm >= SURFACE_WETTED_MM {
            wet_days.insert(hc.time.with_timezone(&Local).date_naive());
        }
    });

    // Daily totals across raw observations, then restricted to the wet-day
    // set so the chip's `total_mm` matches `rain_days`. A 0.3 mm drizzle
    // day that never wet the surface contributes to neither.
    let mut per_day_total: std::collections::BTreeMap<NaiveDate, f64> =
        std::collections::BTreeMap::new();
    for h in history {
        let mm = h.precip_mm.unwrap_or(0.0);
        if !mm.is_finite() || mm <= 0.0 {
            continue;
        }
        let local_date = h.time.with_timezone(&Local).date_naive();
        *per_day_total.entry(local_date).or_insert(0.0) += mm;
    }
    let wet_day_totals: std::collections::BTreeMap<NaiveDate, f64> = per_day_total
        .into_iter()
        .filter(|(d, _)| wet_days.contains(d))
        .collect();

    let total_mm: f64 = wet_day_totals.values().sum();
    let (wettest_day, wettest_day_mm) = wet_day_totals
        .iter()
        .max_by(|a, b| {
            a.1.partial_cmp(b.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.0.cmp(b.0))
        })
        .map(|(d, mm)| (*d, *mm))
        .unwrap_or_else(|| {
            // No regndøgn at all — anchor the "wettest day" to the
            // most recent observation's date so the field is always a
            // valid date even though `wettest_day_mm == 0.0`. Renderers
            // collapse the dry case before reading this field anyway.
            let last = history.last().expect("checked non-empty above");
            (last.time.with_timezone(&Local).date_naive(), 0.0)
        });
    let rain_days = wet_days.len() as u32;
    Some(RainHistory {
        total_mm,
        wettest_day_mm,
        wettest_day,
        rain_days,
        lookback_hours,
    })
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
    let mut final_state = SurfaceState::default();
    let mut step_count: u32 = 0;
    replay_for_each(history, p, |_, state| {
        final_state = *state;
        step_count += 1;
    });
    // Counter saturated against lookback ⇒ no reset ever happened, so
    // the displayed day-count is a lower bound rather than the truth.
    // The renderer surfaces this with a `+` suffix (e.g. "7+ døgn").
    if step_count > 0 && final_state.hours_since_meaningful_rain as u32 >= step_count {
        final_state.drought_at_lookback_cap = true;
    }
    final_state
}

/// Walk Frost observations through the drying model, invoking `on_step`
/// after each step (gap-fill hours included). The callback receives the
/// synthesised `HourlyConditions` for that step plus the post-step
/// `SurfaceState`, which is enough to detect "this hour the surface was
/// wet" (`state.accumulated_mm >= SURFACE_WETTED_MM`) without needing to
/// re-derive the drying maths in two places.
///
/// This is the shared spine for both the surface-state replay
/// ([`replay_into_state`]) and the rain-history aggregator
/// ([`compute_rain_history`]). Keeping them on one walk guarantees the
/// Bakke drought counter and the Regn 7d regndøgn-counter agree on what
/// counts as "the surface got wet".
fn replay_for_each<F>(history: &[frost::HourlyObservation], p: &DryingParams, mut on_step: F)
where
    F: FnMut(&HourlyConditions, &SurfaceState),
{
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
                    on_step(&filler, &state);
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
        on_step(&synth, &state);
        prev_time = Some(h.time);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use grusindeks_met::nowcast::NowcastStep;
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
        fn nowcast_started(&self) {
            self.events.lock().unwrap().push("nowcast_started");
        }
        fn nowcast_finished(&self, _: bool) {
            self.events.lock().unwrap().push("nowcast_finished");
        }
    }

    /// Build a nowcast response body with 24 5-min steps (2h horizon) at
    /// the given rate, anchored on `start`. Lets us drive end-to-end
    /// tests without a frozen fixture going stale every day.
    fn nowcast_body(start: DateTime<Utc>, rate: f64, coverage_ok: bool) -> String {
        let coverage = if coverage_ok { "ok" } else { "no coverage" };
        let mut steps = String::new();
        for i in 0..24 {
            if !steps.is_empty() {
                steps.push(',');
            }
            let t = start + Duration::minutes(5 * i);
            steps.push_str(&format!(
                r#"{{"time":"{}","data":{{"instant":{{"details":{{"precipitation_rate":{:.2}}}}}}}}}"#,
                t.format("%Y-%m-%dT%H:%M:%SZ"),
                rate,
            ));
        }
        format!(
            r#"{{"properties":{{"meta":{{"radar_coverage":"{coverage}"}},"timeseries":[{steps}]}}}}"#,
        )
    }

    #[tokio::test]
    async fn run_score_with_wet_nowcast_degrades_precip_subscore() {
        use grusindeks_met::client::{MetClient, MetClientConfig, UserAgent};
        use wiremock::matchers::{method, path as path_m};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let lf_fixture = include_str!("../../../fixtures/locationforecast_oslo.json");
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_m("/weatherapi/locationforecast/2.0/complete"))
            .respond_with(ResponseTemplate::new(200).set_body_string(lf_fixture))
            .mount(&server)
            .await;
        // Wet radar: 1 mm/h sustained for the next 2 hours.
        let now = Utc::now();
        // Anchor steps at the next 5-min mark so they line up with the
        // forecast hour buckets the apply step will hit.
        let nc_start = now + Duration::seconds(60); // ensure first_rain is in the future
        Mock::given(method("GET"))
            .and(path_m("/weatherapi/nowcast/2.0/complete"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(nowcast_body(nc_start, 1.0, true)),
            )
            .mount(&server)
            .await;

        let ua = UserAgent::new("grusindeks-test", "0.1.0", "dev@example.invalid").unwrap();
        let mut cfg = MetClientConfig::production(ua, None);
        cfg.api_base = format!("{}/", server.uri()).parse().unwrap();
        let client = MetClient::new(cfg).unwrap();

        let center = Point::new(59.9139, 10.7522);
        let win = RideWindow::from_hours(now, 3);

        let dry = run_score(
            &client,
            ScoreInputs {
                center,
                radius_km: 20.0,
                window: win,
                frost_source_id: None,
                history_hours: 168,
                lang: Language::Norwegian,
                fetch_nowcast: false,
                progress: &RecordingProgress::default(),
            },
        )
        .await
        .unwrap();

        let wet = run_score(
            &client,
            ScoreInputs {
                center,
                radius_km: 20.0,
                window: win,
                frost_source_id: None,
                history_hours: 168,
                lang: Language::Norwegian,
                fetch_nowcast: true,
                progress: &RecordingProgress::default(),
            },
        )
        .await
        .unwrap();

        let alert = wet
            .nowcast_alert
            .clone()
            .expect("wet radar should produce an alert");
        assert!(
            alert.peak_mm_h >= 0.9,
            "alert should reflect 1 mm/h radar rate, got {}",
            alert.peak_mm_h
        );
        assert!(
            wet.mean <= dry.mean,
            "radar-overridden run should not score higher than the dry baseline (wet={}, dry={})",
            wet.mean,
            dry.mean,
        );
        // Probability subscore: nowcast bumps `probability_of_precip` to
        // 95 on the affected hours, so the prob subscore should drop or
        // stay at 0 — never improve relative to the dry baseline.
        let dry_prob = avg_axis_probability(&dry);
        let wet_prob = avg_axis_probability(&wet);
        assert!(
            wet_prob <= dry_prob,
            "radar should not improve the probability axis (dry={dry_prob}, wet={wet_prob})",
        );
    }

    #[tokio::test]
    async fn run_score_with_no_radar_coverage_skips_alert() {
        use grusindeks_met::client::{MetClient, MetClientConfig, UserAgent};
        use wiremock::matchers::{method, path as path_m};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let lf_fixture = include_str!("../../../fixtures/locationforecast_oslo.json");
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_m("/weatherapi/locationforecast/2.0/complete"))
            .respond_with(ResponseTemplate::new(200).set_body_string(lf_fixture))
            .mount(&server)
            .await;
        let now = Utc::now();
        Mock::given(method("GET"))
            .and(path_m("/weatherapi/nowcast/2.0/complete"))
            .respond_with(ResponseTemplate::new(200).set_body_string(nowcast_body(
                now + Duration::seconds(60),
                1.0,
                false,
            )))
            .mount(&server)
            .await;

        let ua = UserAgent::new("grusindeks-test", "0.1.0", "dev@example.invalid").unwrap();
        let mut cfg = MetClientConfig::production(ua, None);
        cfg.api_base = format!("{}/", server.uri()).parse().unwrap();
        let client = MetClient::new(cfg).unwrap();

        let center = Point::new(59.9139, 10.7522);
        let win = RideWindow::from_hours(now, 3);
        let agg = run_score(
            &client,
            ScoreInputs {
                center,
                radius_km: 20.0,
                window: win,
                frost_source_id: None,
                history_hours: 168,
                lang: Language::Norwegian,
                fetch_nowcast: true,
                progress: &RecordingProgress::default(),
            },
        )
        .await
        .unwrap();
        assert!(
            agg.nowcast_alert.is_none(),
            "no radar coverage → no alert (got {:?})",
            agg.nowcast_alert,
        );
    }

    fn avg_axis_probability(agg: &AggregateScore) -> u32 {
        let n = agg.points.len() as u32;
        let sum: u32 = agg
            .points
            .iter()
            .map(|p| u32::from(p.score.breakdown.precip_probability))
            .sum();
        (sum + n / 2) / n
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
                fetch_nowcast: false,
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

    // ---- apply_nowcast_overrides ----

    fn point_oslo() -> Point {
        Point::new(59.9139, 10.7522)
    }

    /// 12 nowcast steps (5-min apart) within `[start, start + 1h)`,
    /// constant rate. With rate=0.6 the per-hour mm is 0.6.
    fn constant_rate_steps(start: DateTime<Utc>, rate: f64) -> Vec<NowcastStep> {
        (0..12)
            .map(|i| NowcastStep {
                time: start + Duration::minutes(5 * i),
                precipitation_rate_mm_h: rate,
            })
            .collect()
    }

    #[test]
    fn nowcast_overrides_dry_locationforecast_with_radar_rain() {
        let h_start = Utc.with_ymd_and_hms(2026, 4, 26, 14, 0, 0).unwrap();
        let hours = vec![
            HourlyConditions::minimal(h_start, 12.0, 3.0, 0.0),
            HourlyConditions::minimal(h_start + Duration::hours(1), 12.0, 3.0, 0.0),
        ];
        let mut per_point = vec![(point_oslo(), hours.clone())];
        let nowcast = Nowcast {
            radar_coverage_ok: true,
            steps: constant_rate_steps(h_start, 0.6),
        };
        apply_nowcast_overrides(&mut per_point, &nowcast);
        let updated = &per_point[0].1;
        // First hour got 0.6 mm of radar rain.
        assert!(
            (updated[0].precipitation_mm - 0.6).abs() < 1e-9,
            "expected 0.6 mm, got {}",
            updated[0].precipitation_mm
        );
        // Second hour outside nowcast window → untouched.
        assert_eq!(updated[1].precipitation_mm, 0.0);
        // Probability bumped to 95 because peak rate (0.6) clears the threshold.
        assert_eq!(updated[0].probability_of_precip, Some(95.0));
        assert_eq!(updated[1].probability_of_precip, None);
        let _ = hours; // silence the unused-original-clone warning
    }

    #[test]
    fn nowcast_does_not_lower_a_wetter_locationforecast() {
        // Locationforecast says 1.5 mm in this hour; radar only sees 0.3 mm.
        // Conservative max() rule keeps locationforecast.
        let h_start = Utc.with_ymd_and_hms(2026, 4, 26, 14, 0, 0).unwrap();
        let mut per_point = vec![(
            point_oslo(),
            vec![HourlyConditions {
                probability_of_precip: Some(80.0),
                ..HourlyConditions::minimal(h_start, 12.0, 3.0, 1.5)
            }],
        )];
        let nowcast = Nowcast {
            radar_coverage_ok: true,
            steps: constant_rate_steps(h_start, 0.3),
        };
        apply_nowcast_overrides(&mut per_point, &nowcast);
        let updated = &per_point[0].1[0];
        assert_eq!(updated.precipitation_mm, 1.5, "max() should win");
        // Probability does climb to 95 since radar rate (0.3 ≥ 0.1) confirms rain.
        assert_eq!(updated.probability_of_precip, Some(95.0));
    }

    #[test]
    fn nowcast_skips_buckets_outside_window() {
        // Hour starts 3h after now — nowcast only covers next 2h.
        let now = Utc.with_ymd_and_hms(2026, 4, 26, 14, 0, 0).unwrap();
        let mut per_point = vec![(
            point_oslo(),
            vec![HourlyConditions::minimal(
                now + Duration::hours(3),
                12.0,
                3.0,
                0.0,
            )],
        )];
        let nowcast = Nowcast {
            radar_coverage_ok: true,
            steps: constant_rate_steps(now, 1.0),
        };
        apply_nowcast_overrides(&mut per_point, &nowcast);
        assert_eq!(
            per_point[0].1[0].precipitation_mm, 0.0,
            "bucket 3h out should be untouched"
        );
        assert_eq!(per_point[0].1[0].probability_of_precip, None);
    }

    #[test]
    fn nowcast_no_coverage_is_a_noop() {
        let h_start = Utc.with_ymd_and_hms(2026, 4, 26, 14, 0, 0).unwrap();
        let mut per_point = vec![(
            point_oslo(),
            vec![HourlyConditions::minimal(h_start, 12.0, 3.0, 0.0)],
        )];
        // Steps would otherwise mutate the bucket — but coverage_ok=false bails.
        let nowcast = Nowcast {
            radar_coverage_ok: false,
            steps: constant_rate_steps(h_start, 0.6),
        };
        apply_nowcast_overrides(&mut per_point, &nowcast);
        assert_eq!(per_point[0].1[0].precipitation_mm, 0.0);
    }

    #[test]
    fn nowcast_partial_coverage_only_affects_present_minutes() {
        // 6 steps over the first 30 min of an hour, then nothing. Sum of
        // rates = 6 × 0.6 = 3.6 mm/h-equivalents → 3.6 / 12 = 0.3 mm.
        // (Reads as "rain fell for half the hour at 0.6 mm/h".)
        let h_start = Utc.with_ymd_and_hms(2026, 4, 26, 14, 0, 0).unwrap();
        let mut per_point = vec![(
            point_oslo(),
            vec![HourlyConditions::minimal(h_start, 12.0, 3.0, 0.0)],
        )];
        let nowcast = Nowcast {
            radar_coverage_ok: true,
            steps: (0..6)
                .map(|i| NowcastStep {
                    time: h_start + Duration::minutes(5 * i),
                    precipitation_rate_mm_h: 0.6,
                })
                .collect(),
        };
        apply_nowcast_overrides(&mut per_point, &nowcast);
        assert!(
            (per_point[0].1[0].precipitation_mm - 0.3).abs() < 1e-9,
            "expected 0.3 mm for 30-min coverage, got {}",
            per_point[0].1[0].precipitation_mm
        );
    }

    #[test]
    fn nowcast_propagates_to_every_sample_point() {
        let h_start = Utc.with_ymd_and_hms(2026, 4, 26, 14, 0, 0).unwrap();
        let make_hours = || vec![HourlyConditions::minimal(h_start, 12.0, 3.0, 0.0)];
        let mut per_point = vec![
            (Point::new(59.91, 10.75), make_hours()),
            (Point::new(60.00, 10.75), make_hours()),
            (Point::new(59.85, 10.85), make_hours()),
        ];
        let nowcast = Nowcast {
            radar_coverage_ok: true,
            steps: constant_rate_steps(h_start, 0.6),
        };
        apply_nowcast_overrides(&mut per_point, &nowcast);
        for (_, hours) in &per_point {
            assert!(
                (hours[0].precipitation_mm - 0.6).abs() < 1e-9,
                "every sample point should receive the same radar override"
            );
        }
    }

    // ---- build_nowcast_alert ----

    #[test]
    fn build_nowcast_alert_dry_returns_none() {
        let now = Utc.with_ymd_and_hms(2026, 4, 26, 14, 0, 0).unwrap();
        let nowcast = Nowcast {
            radar_coverage_ok: true,
            steps: constant_rate_steps(now, 0.0),
        };
        assert!(build_nowcast_alert(&nowcast, now).is_none());
    }

    #[test]
    fn build_nowcast_alert_no_coverage_returns_none() {
        let now = Utc.with_ymd_and_hms(2026, 4, 26, 14, 0, 0).unwrap();
        let nowcast = Nowcast {
            radar_coverage_ok: false,
            steps: constant_rate_steps(now, 0.6),
        };
        assert!(build_nowcast_alert(&nowcast, now).is_none());
    }

    #[test]
    fn build_nowcast_alert_returns_some_when_rain_in_future() {
        let now = Utc.with_ymd_and_hms(2026, 4, 26, 14, 0, 0).unwrap();
        let nowcast = Nowcast {
            radar_coverage_ok: true,
            steps: vec![
                NowcastStep {
                    time: now + Duration::minutes(15),
                    precipitation_rate_mm_h: 0.4,
                },
                NowcastStep {
                    time: now + Duration::minutes(20),
                    precipitation_rate_mm_h: 0.6,
                },
            ],
        };
        let a = build_nowcast_alert(&nowcast, now).expect("rain in future");
        assert_eq!(a.first_rain_at, now + Duration::minutes(15));
        assert!((a.peak_mm_h - 0.6).abs() < 1e-9);
    }

    #[test]
    fn build_nowcast_alert_returns_none_when_only_past_steps() {
        // Nowcast occasionally returns the most recent few steps in the
        // past — those shouldn't surface as "regn om -3 min".
        let now = Utc.with_ymd_and_hms(2026, 4, 26, 14, 0, 0).unwrap();
        let nowcast = Nowcast {
            radar_coverage_ok: true,
            steps: vec![NowcastStep {
                time: now - Duration::minutes(10),
                precipitation_rate_mm_h: 0.6,
            }],
        };
        assert!(build_nowcast_alert(&nowcast, now).is_none());
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

    // ---- compute_rain_history ----

    #[test]
    fn rain_history_none_for_empty_history() {
        assert!(compute_rain_history(&[], 168).is_none());
    }

    #[test]
    fn rain_history_dry_period_is_some_with_zeroes() {
        // We *did* check Frost; the answer is "no rain". Renderer collapses
        // the chip to "tørt siste N døgn" but the data layer must report
        // truthfully rather than masquerade as Frost-was-down.
        let now = Utc::now();
        let history: Vec<frost::HourlyObservation> = (0..168)
            .map(|i| frost::HourlyObservation {
                time: now - Duration::hours(168 - i),
                precip_mm: Some(0.0),
                temp_c: None,
                wind_ms: None,
                humidity_pct: None,
            })
            .collect();
        let h = compute_rain_history(&history, 168).expect("Some for non-empty input");
        assert_eq!(h.total_mm, 0.0);
        assert_eq!(h.rain_days, 0);
        assert_eq!(h.lookback_hours, 168);
    }

    #[test]
    fn rain_history_aggregates_days_in_local_tz() {
        // Two distinct local-day rain events, each large enough to reset
        // the drying counter (>1 mm/h dwarfs the per-hour drying rate).
        let base = chrono::TimeZone::with_ymd_and_hms(&Utc, 2026, 4, 18, 9, 0, 0).unwrap();
        let mut history = Vec::new();
        for i in 0..3 {
            history.push(frost::HourlyObservation {
                time: base + Duration::hours(i),
                precip_mm: Some(1.5),
                temp_c: None,
                wind_ms: None,
                humidity_pct: None,
            });
        }
        history.push(frost::HourlyObservation {
            time: base + Duration::hours(28),
            precip_mm: Some(5.4),
            temp_c: None,
            wind_ms: None,
            humidity_pct: None,
        });
        let h = compute_rain_history(&history, 168).expect("Some");
        assert_eq!(h.rain_days, 2);
        // Total is restricted to wet-day rows: 3 × 1.5 + 5.4 = 9.9.
        assert!((h.total_mm - 9.9).abs() < 1e-6, "got {}", h.total_mm);
        assert!(
            (h.wettest_day_mm - 5.4).abs() < 1e-6,
            "wettest day should be the 5.4 mm downpour, got {}",
            h.wettest_day_mm,
        );
    }

    #[test]
    fn rain_history_subthreshold_drizzle_is_not_a_rain_day() {
        // 0.1 mm × 5 hours = 0.5 mm cumulative — well over the daily-sum
        // threshold, but no single hour reaches 0.3 mm and drying eats the
        // accumulator below it every step. The drying counter never resets,
        // so neither does the regndøgn count: Regn 7d and Bakke stay in
        // lock-step on what "regnet" means.
        let base = chrono::TimeZone::with_ymd_and_hms(&Utc, 2026, 4, 24, 6, 0, 0).unwrap();
        let history: Vec<frost::HourlyObservation> = (0..5)
            .map(|i| frost::HourlyObservation {
                time: base + Duration::hours(i),
                precip_mm: Some(0.1),
                temp_c: None,
                wind_ms: None,
                humidity_pct: None,
            })
            .collect();
        let h = compute_rain_history(&history, 168).expect("Some");
        assert_eq!(
            h.rain_days, 0,
            "5 × 0.1 mm should not count as a regndøgn — surface never wetted",
        );
        // Sub-threshold drizzle is excluded from total_mm too — otherwise
        // the chip says "0.5 mm siste 7 d, 0 regndøgn", which raises the
        // exact "where did that mm go?" question that makes the chip
        // disagree with Bakke on the user's screen.
        assert_eq!(
            h.total_mm, 0.0,
            "sub-threshold drizzle must not show up in total_mm either",
        );
    }

    #[test]
    fn rain_history_single_strong_hour_counts_as_rain_day() {
        // 1.0 mm in one hour easily clears the drying rate, so the
        // accumulator reaches >= SURFACE_WETTED_MM and the drought counter
        // resets — that's a regndøgn.
        let now = Utc::now();
        let history = vec![frost::HourlyObservation {
            time: now - Duration::hours(2),
            precip_mm: Some(1.0),
            temp_c: None,
            wind_ms: None,
            humidity_pct: None,
        }];
        let h = compute_rain_history(&history, 168).expect("Some");
        assert_eq!(h.rain_days, 1);
        assert!((h.total_mm - 1.0).abs() < 1e-9);
    }

    #[test]
    fn rain_history_threshold_hour_eaten_by_drying_does_not_count() {
        // Exactly 0.3 mm in one hour, no carry-over. The drying step
        // applies a non-zero drying rate (warm, dry-ish defaults) so
        // `after_drying < 0.3` even though `precip_mm == SURFACE_WETTED_MM`.
        // This is precisely the case the user flagged: "0.4 mm siste 7 d,
        // våtest day 0.3 mm, 1 regndøgn" alongside a Bakke "7 døgn uten
        // regn". With the drying-replay-based rule, this collapses to
        // zero regndøgn, in step with Bakke.
        let now = Utc::now();
        let history = vec![frost::HourlyObservation {
            time: now - Duration::hours(2),
            // Warm + dry-ish so drying eats the 0.3 mm. The defaults in
            // replay_for_each set 70% RH / 70% cloud when fields are None,
            // so we set temp/wind explicitly to push drying up.
            precip_mm: Some(0.3),
            temp_c: Some(20.0),
            wind_ms: Some(5.0),
            humidity_pct: Some(40.0),
        }];
        let h = compute_rain_history(&history, 168).expect("Some");
        assert_eq!(
            h.rain_days, 0,
            "0.3 mm eaten by drying must not count as a regndøgn (matches Bakke)",
        );
        assert_eq!(h.total_mm, 0.0);
    }

    #[test]
    fn replay_marks_drought_at_lookback_cap_when_no_rain_in_history() {
        // 168 hours of dry observations — the counter ends at ≥168 with
        // no reset, so the chip's day-count is a lower bound. Flag it.
        let now = Utc::now();
        let history: Vec<frost::HourlyObservation> = (0..168)
            .map(|i| frost::HourlyObservation {
                time: now - Duration::hours(168 - i),
                precip_mm: Some(0.0),
                temp_c: None,
                wind_ms: None,
                humidity_pct: None,
            })
            .collect();
        let state = replay_into_state(&history, &DryingParams::default());
        assert!(
            state.drought_at_lookback_cap,
            "all-dry history should set the cap flag",
        );
    }

    #[test]
    fn replay_does_not_mark_cap_when_history_contains_rain() {
        // A reset somewhere in the lookback ⇒ the day-count is exact, not
        // a lower bound. No `+` should be appended in that case.
        let now = Utc::now();
        let mut history: Vec<frost::HourlyObservation> = (0..168)
            .map(|i| frost::HourlyObservation {
                time: now - Duration::hours(168 - i),
                precip_mm: Some(0.0),
                temp_c: None,
                wind_ms: None,
                humidity_pct: None,
            })
            .collect();
        // One real rain hour 96h ago.
        history[72].precip_mm = Some(2.0);
        let state = replay_into_state(&history, &DryingParams::default());
        assert!(
            !state.drought_at_lookback_cap,
            "history with a reset should not set the cap flag",
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
            drought_at_lookback_cap: false,
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
            drought_at_lookback_cap: false,
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
            drought_at_lookback_cap: false,
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
