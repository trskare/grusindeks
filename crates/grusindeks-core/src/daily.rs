//! Day-level rollup of the Grusindeks for the multi-day forecast view.
//!
//! Where `score::score` evaluates one ride window, this module slices a
//! forecast into per-day buckets, computes a day score, and looks for an
//! optimal sub-window when the day has uneven weather (a "luke" — opening).
//! It also assigns a `Confidence` based on data resolution and forecast
//! horizon, since `api.met.no` only publishes hourly fidelity for the
//! first ~60 hours.

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

use crate::score::{score, Grusindeks};
use crate::types::{HourlyConditions, Resolution, RideWindow};

/// How much the user should trust a forecast. Drops as the horizon grows
/// and as the proportion of 6-hourly data overtakes hourly data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    Hoy,
    Middels,
    Lav,
}

impl Confidence {
    /// Norwegian label for the human renderer.
    pub fn label_no(self) -> &'static str {
        match self {
            Confidence::Hoy => "høy",
            Confidence::Middels => "middels",
            Confidence::Lav => "lav",
        }
    }
}

/// A sub-window of the day that scores meaningfully better than the day
/// as a whole — the "go ride now" suggestion.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OptimalWindow {
    pub window: RideWindow,
    pub score: Grusindeks,
    /// `optimal.total - day.total`. Always > 0 by construction; the caller
    /// decides what threshold counts as "meaningfully" better.
    pub improvement: u8,
}

/// One day's worth of summary data.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DayScore {
    /// The full day window in UTC. Whatever the caller passed in.
    pub window: RideWindow,
    /// Score over the whole day window.
    pub score: Grusindeks,
    pub confidence: Confidence,
    /// Number of hours of forecast data we actually had inside the window.
    pub hours_with_data: usize,
    /// `Some` when at least one shorter sub-window scores `min_improvement`
    /// or more above the day score.
    pub optimal_window: Option<OptimalWindow>,
    /// Single-glyph weather summary for the day. Derived from the in-window
    /// cloud cover, total precipitation, max wind, and mean temperature so
    /// the renderer can show one icon per day.
    pub weather_icon: &'static str,
}

/// Default minimum point-improvement before we surface a "best window" in
/// the multi-day view. Ten points is roughly one label step (e.g. OK→Bra)
/// — anything smaller is noise users wouldn't care about.
pub const DEFAULT_OPTIMAL_IMPROVEMENT: u8 = 10;

/// Default sub-window length when looking for an optimal "luke". Three
/// hours matches the default ride window in the single-day score command.
pub const DEFAULT_OPTIMAL_WINDOW_HOURS: i64 = 3;

/// Compute the `DayScore` for the given `day_window`.
///
/// `now` is used to compute the forecast horizon for confidence.
/// `ground_water_mm` is the drying-model state at the start of the window.
pub fn compute_day(
    hours: &[HourlyConditions],
    day_window: RideWindow,
    ground_water_mm: f64,
    now: DateTime<Utc>,
) -> DayScore {
    let in_window: Vec<&HourlyConditions> = hours
        .iter()
        .filter(|h| day_window.contains(h.time))
        .collect();

    let day_score = score(hours, day_window, ground_water_mm);
    let confidence = confidence_for(&in_window, day_window, now);

    let optimal_window = find_best_window(
        hours,
        day_window,
        DEFAULT_OPTIMAL_WINDOW_HOURS,
        ground_water_mm,
    )
    .filter(|ow| ow.improvement >= DEFAULT_OPTIMAL_IMPROVEMENT);

    let weather_icon = weather_icon_for(&in_window);

    DayScore {
        window: day_window,
        score: day_score,
        confidence,
        hours_with_data: in_window.len(),
        optimal_window,
        weather_icon,
    }
}

/// Pick a single weather emoji that best summarises the day. Order of
/// precedence is "what would a cyclist actually want to see first": storm
/// wind dominates, then snow, then rain, then cloud cover.
///
/// Rain threshold is intentionally tight — `🌧` only fires when there's
/// enough precipitation that the ride would actually feel wet. A 1-mm
/// shower spread over a 16-h day window scores ~93/100 and shouldn't
/// be flagged as a "rain day" or the icon contradicts the score.
pub fn weather_icon_for(hours: &[&HourlyConditions]) -> &'static str {
    if hours.is_empty() {
        return "·";
    }
    let n = hours.len() as f64;
    let mean_temp = hours.iter().map(|h| h.temperature_c).sum::<f64>() / n;
    let total_precip: f64 = hours.iter().map(|h| h.precipitation_mm).sum();
    let max_hourly_precip = hours
        .iter()
        .map(|h| h.precipitation_mm)
        .fold(f64::NEG_INFINITY, f64::max);
    let max_wind = hours
        .iter()
        .map(|h| h.wind_speed_ms)
        .fold(f64::NEG_INFINITY, f64::max);
    let mean_cloud: Option<f64> = {
        let xs: Vec<f64> = hours.iter().filter_map(|h| h.cloud_area_fraction).collect();
        if xs.is_empty() {
            None
        } else {
            Some(xs.iter().sum::<f64>() / xs.len() as f64)
        }
    };

    if max_wind.is_finite() && max_wind > 10.0 {
        return "🌬";
    }
    // Rain icon only when intensity *or* accumulation actually matters
    // for the ride. Tuned to roughly match the scoring drizzle threshold
    // (0.5 mm/h) and a multi-hour wet stretch (~3 mm total).
    let notable_rain = (max_hourly_precip.is_finite() && max_hourly_precip > 0.5)
        || total_precip > 3.0;
    if notable_rain {
        return if mean_temp <= 0.0 { "🌨" } else { "🌧" };
    }
    match mean_cloud {
        Some(c) if c >= 80.0 => "☁",
        Some(c) if c >= 40.0 => "⛅",
        Some(_) => "☀",
        // Long-range forecasts often miss cloud cover. Fall back to a
        // neutral cloudy-ish glyph rather than promising sunshine.
        None => "⛅",
    }
}

/// Slide a `length_hours` window across `day_window` (1-hour stride) and
/// return the highest-scoring sub-window.
///
/// Returns `None` when `day_window` is shorter than `length_hours` or no
/// hours fall inside it. The improvement field is `score(best) - score(day)`,
/// clamped to 0 when negative (which can happen if every sub-window is
/// dragged down by the same heavy rain).
pub fn find_best_window(
    hours: &[HourlyConditions],
    day_window: RideWindow,
    length_hours: i64,
    ground_water_mm: f64,
) -> Option<OptimalWindow> {
    if length_hours <= 0 || day_window.duration_hours() < length_hours {
        return None;
    }
    if !hours.iter().any(|h| day_window.contains(h.time)) {
        return None;
    }

    let day_total = score(hours, day_window, ground_water_mm).total;
    let len = Duration::hours(length_hours);

    let mut best: Option<(RideWindow, Grusindeks)> = None;
    let mut start = day_window.start;
    while start + len <= day_window.end {
        let candidate = RideWindow {
            start,
            end: start + len,
        };
        // Only score sub-windows that actually contain at least one hour
        // of data; otherwise we'd compare against the empty-window neutral
        // fallback inside `score`, which is misleading.
        let has_data = hours.iter().any(|h| candidate.contains(h.time));
        if has_data {
            let s = score(hours, candidate, ground_water_mm);
            best = match best {
                None => Some((candidate, s)),
                Some((_, ref cur)) if s.total > cur.total => Some((candidate, s)),
                Some(other) => Some(other),
            };
        }
        start += Duration::hours(1);
    }

    best.map(|(window, s)| {
        let improvement = s.total.saturating_sub(day_total);
        OptimalWindow {
            window,
            score: s,
            improvement,
        }
    })
}

/// Heuristic confidence for a day:
///
/// * **Lav** when more than half of the in-window hours come from a 6-hour
///   bucket, or when the day starts more than 5 days from `now`.
/// * **Hoy** when every in-window hour is `Hourly` *and* the day starts
///   within 24 hours of `now`.
/// * **Middels** otherwise.
fn confidence_for(
    in_window: &[&HourlyConditions],
    day_window: RideWindow,
    now: DateTime<Utc>,
) -> Confidence {
    if in_window.is_empty() {
        return Confidence::Lav;
    }
    let horizon = day_window.start - now;
    let six_hourly_count = in_window
        .iter()
        .filter(|h| h.resolution == Resolution::SixHourly)
        .count();
    let six_hourly_ratio = six_hourly_count as f64 / in_window.len() as f64;

    if horizon > Duration::days(5) || six_hourly_ratio > 0.5 {
        return Confidence::Lav;
    }
    if horizon < Duration::days(1) && six_hourly_count == 0 {
        return Confidence::Hoy;
    }
    Confidence::Middels
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t(year: i32, month: u32, day: u32, hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, month, day, hour, 0, 0).unwrap()
    }

    fn nice_hour(time: DateTime<Utc>) -> HourlyConditions {
        HourlyConditions {
            probability_of_precip: Some(5.0),
            ..HourlyConditions::minimal(time, 17.0, 2.0, 0.0)
        }
    }

    fn awful_hour(time: DateTime<Utc>) -> HourlyConditions {
        HourlyConditions {
            probability_of_precip: Some(95.0),
            ..HourlyConditions::minimal(time, 5.0, 11.0, 3.0)
        }
    }

    fn sixhourly(time: DateTime<Utc>) -> HourlyConditions {
        HourlyConditions {
            resolution: Resolution::SixHourly,
            ..nice_hour(time)
        }
    }

    fn day_window(start_hour: u32, length_hours: i64) -> RideWindow {
        RideWindow::from_hours(t(2026, 4, 26, start_hour), length_hours)
    }

    // ---- find_best_window ----

    #[test]
    fn best_window_finds_only_dry_stretch_in_a_rainy_day() {
        // 06..09 dry, 09..15 awful, 15..20 dry — best 3h window must land
        // either at the start or end of the day.
        let mut hours: Vec<HourlyConditions> =
            (6..9).map(|h| nice_hour(t(2026, 4, 26, h))).collect();
        hours.extend((9..15).map(|h| awful_hour(t(2026, 4, 26, h))));
        hours.extend((15..20).map(|h| nice_hour(t(2026, 4, 26, h))));
        let day = day_window(6, 14); // 06..20

        let ow = find_best_window(&hours, day, 3, 0.0).expect("non-empty");
        let start_h = ow.window.start.format("%H").to_string();
        assert!(
            ["06", "07", "15", "16", "17"].contains(&start_h.as_str()),
            "best window started at {start_h}, expected a dry stretch"
        );
        // The day score is dragged down by 6 awful hours; the dry window
        // must score strictly better.
        assert!(ow.improvement > 0, "improvement was {}", ow.improvement);
    }

    #[test]
    fn best_window_returns_none_when_day_is_uniformly_perfect() {
        // No improvement possible — every window scores ~the same.
        let hours: Vec<HourlyConditions> = (6..18).map(|h| nice_hour(t(2026, 4, 26, h))).collect();
        let day = day_window(6, 12);
        let ow = find_best_window(&hours, day, 3, 0.0).unwrap();
        assert_eq!(
            ow.improvement, 0,
            "uniform perfect day should yield zero improvement"
        );
    }

    #[test]
    fn best_window_returns_none_when_day_shorter_than_length() {
        let hours: Vec<HourlyConditions> = (6..8).map(|h| nice_hour(t(2026, 4, 26, h))).collect();
        let day = day_window(6, 2);
        assert!(find_best_window(&hours, day, 3, 0.0).is_none());
    }

    #[test]
    fn best_window_returns_none_when_no_hours_in_window() {
        let hours: Vec<HourlyConditions> = (6..9).map(|h| nice_hour(t(2026, 4, 27, h))).collect(); // wrong day
        let day = day_window(6, 12);
        assert!(find_best_window(&hours, day, 3, 0.0).is_none());
    }

    // ---- compute_day ----

    #[test]
    fn compute_day_surfaces_optimal_window_when_improvement_clears_threshold() {
        let mut hours: Vec<HourlyConditions> =
            (6..9).map(|h| nice_hour(t(2026, 4, 26, h))).collect();
        hours.extend((9..15).map(|h| awful_hour(t(2026, 4, 26, h))));
        hours.extend((15..20).map(|h| nice_hour(t(2026, 4, 26, h))));
        let day = day_window(6, 14);
        let now = t(2026, 4, 26, 5); // ride starts in 1h
        let ds = compute_day(&hours, day, 0.0, now);

        assert!(ds.optimal_window.is_some(), "expected a luke to be flagged");
        let ow = ds.optimal_window.unwrap();
        assert!(ow.score.total > ds.score.total);
        assert!(ow.improvement >= DEFAULT_OPTIMAL_IMPROVEMENT);
    }

    #[test]
    fn compute_day_omits_optimal_window_when_improvement_is_small() {
        // Uniformly nice day: every sub-window scores the same → omit.
        let hours: Vec<HourlyConditions> = (6..18).map(|h| nice_hour(t(2026, 4, 26, h))).collect();
        let day = day_window(6, 12);
        let now = t(2026, 4, 26, 5);
        let ds = compute_day(&hours, day, 0.0, now);
        assert!(ds.optimal_window.is_none());
    }

    // ---- confidence_for ----

    #[test]
    fn confidence_high_for_today_with_only_hourly_data() {
        let hours: Vec<&HourlyConditions> = vec![]; // unused — compute via compute_day
        let _ = hours;
        let day_hours: Vec<HourlyConditions> =
            (6..18).map(|h| nice_hour(t(2026, 4, 26, h))).collect();
        let day = day_window(6, 12);
        let now = t(2026, 4, 26, 5);
        let ds = compute_day(&day_hours, day, 0.0, now);
        assert_eq!(ds.confidence, Confidence::Hoy);
    }

    #[test]
    fn confidence_medium_for_a_day_two_days_out() {
        let day_hours: Vec<HourlyConditions> =
            (6..18).map(|h| nice_hour(t(2026, 4, 28, h))).collect();
        let day = RideWindow::from_hours(t(2026, 4, 28, 6), 12);
        let now = t(2026, 4, 26, 5);
        let ds = compute_day(&day_hours, day, 0.0, now);
        assert_eq!(ds.confidence, Confidence::Middels);
    }

    #[test]
    fn confidence_low_for_day_more_than_five_days_out() {
        let day_hours: Vec<HourlyConditions> =
            (6..18).map(|h| nice_hour(t(2026, 5, 3, h))).collect();
        let day = RideWindow::from_hours(t(2026, 5, 3, 6), 12);
        let now = t(2026, 4, 26, 5);
        let ds = compute_day(&day_hours, day, 0.0, now);
        assert_eq!(ds.confidence, Confidence::Lav);
    }

    #[test]
    fn confidence_low_when_majority_six_hourly_even_within_horizon() {
        // Nearby day, but resolution is mostly six-hourly → still Lav.
        let day_hours: Vec<HourlyConditions> =
            (6..18).map(|h| sixhourly(t(2026, 4, 27, h))).collect();
        let day = RideWindow::from_hours(t(2026, 4, 27, 6), 12);
        let now = t(2026, 4, 26, 5);
        let ds = compute_day(&day_hours, day, 0.0, now);
        assert_eq!(ds.confidence, Confidence::Lav);
    }

    #[test]
    fn compute_day_counts_in_window_hours() {
        let hours: Vec<HourlyConditions> = (4..20).map(|h| nice_hour(t(2026, 4, 26, h))).collect();
        let day = day_window(6, 12); // 06..18
        let now = t(2026, 4, 26, 5);
        let ds = compute_day(&hours, day, 0.0, now);
        assert_eq!(ds.hours_with_data, 12);
    }

    // ---- weather_icon_for ----

    fn with_clouds(time: DateTime<Utc>, cloud_pct: f64) -> HourlyConditions {
        HourlyConditions {
            cloud_area_fraction: Some(cloud_pct),
            ..nice_hour(time)
        }
    }

    #[test]
    fn weather_icon_sun_for_clear_sky() {
        let hs: Vec<_> = (6..18)
            .map(|h| with_clouds(t(2026, 4, 26, h), 10.0))
            .collect();
        let refs: Vec<&HourlyConditions> = hs.iter().collect();
        assert_eq!(weather_icon_for(&refs), "☀");
    }

    #[test]
    fn weather_icon_partly_cloudy_for_mid_cloud() {
        let hs: Vec<_> = (6..18)
            .map(|h| with_clouds(t(2026, 4, 26, h), 60.0))
            .collect();
        let refs: Vec<&HourlyConditions> = hs.iter().collect();
        assert_eq!(weather_icon_for(&refs), "⛅");
    }

    #[test]
    fn weather_icon_overcast_for_heavy_cloud() {
        let hs: Vec<_> = (6..18)
            .map(|h| with_clouds(t(2026, 4, 26, h), 95.0))
            .collect();
        let refs: Vec<&HourlyConditions> = hs.iter().collect();
        assert_eq!(weather_icon_for(&refs), "☁");
    }

    #[test]
    fn weather_icon_rain_when_max_hourly_above_drizzle_threshold() {
        // One hour of moderate rain (0.8 mm/h) is enough to flag the
        // day as 🌧, even though the total is small.
        let mut hs: Vec<_> = (6..18)
            .map(|h| HourlyConditions {
                cloud_area_fraction: Some(90.0),
                ..HourlyConditions::minimal(t(2026, 4, 26, h), 8.0, 2.0, 0.0)
            })
            .collect();
        hs[3].precipitation_mm = 0.8;
        let refs: Vec<&HourlyConditions> = hs.iter().collect();
        assert_eq!(weather_icon_for(&refs), "🌧");
    }

    #[test]
    fn weather_icon_rain_when_total_above_3mm_even_if_no_spike() {
        // Wet all day at low intensity (0.3 mm/h × 12 h = 3.6 mm total).
        let hs: Vec<_> = (6..18)
            .map(|h| HourlyConditions {
                cloud_area_fraction: Some(90.0),
                ..HourlyConditions::minimal(t(2026, 4, 26, h), 8.0, 2.0, 0.3)
            })
            .collect();
        let refs: Vec<&HourlyConditions> = hs.iter().collect();
        assert_eq!(weather_icon_for(&refs), "🌧");
    }

    #[test]
    fn weather_icon_brief_shower_falls_back_to_cloud_not_rain() {
        // A single light hour (0.2 mm) over a 12-h day → cycling-wise
        // a non-event. Score sits in the 90s so the icon must agree.
        let mut hs: Vec<_> = (6..18)
            .map(|h| HourlyConditions {
                cloud_area_fraction: Some(60.0),
                ..HourlyConditions::minimal(t(2026, 4, 26, h), 12.0, 2.0, 0.0)
            })
            .collect();
        hs[5].precipitation_mm = 0.2;
        let refs: Vec<&HourlyConditions> = hs.iter().collect();
        let icon = weather_icon_for(&refs);
        assert!(
            icon == "⛅" || icon == "☀" || icon == "☁",
            "expected a cloud/sun glyph for a 0.2 mm shower, got {icon:?}"
        );
    }

    #[test]
    fn weather_icon_snow_when_precip_and_freezing() {
        let mut hs: Vec<_> = (6..18)
            .map(|h| HourlyConditions {
                cloud_area_fraction: Some(90.0),
                ..HourlyConditions::minimal(t(2026, 4, 26, h), -3.0, 2.0, 0.0)
            })
            .collect();
        hs[3].precipitation_mm = 0.8;
        let refs: Vec<&HourlyConditions> = hs.iter().collect();
        assert_eq!(weather_icon_for(&refs), "🌨");
    }

    #[test]
    fn weather_icon_wind_dominates_when_max_wind_above_10() {
        let hs: Vec<_> = (6..18)
            .map(|h| HourlyConditions {
                cloud_area_fraction: Some(20.0),
                ..HourlyConditions::minimal(t(2026, 4, 26, h), 12.0, 11.0, 0.0)
            })
            .collect();
        let refs: Vec<&HourlyConditions> = hs.iter().collect();
        assert_eq!(weather_icon_for(&refs), "🌬");
    }

    #[test]
    fn weather_icon_falls_back_when_no_cloud_data_and_dry() {
        // No cloud_area_fraction set on any hour, no precip. Should not
        // confidently promise sunshine — falls back to ⛅.
        let hs: Vec<HourlyConditions> = (6..18).map(|h| nice_hour(t(2026, 4, 26, h))).collect();
        let refs: Vec<&HourlyConditions> = hs.iter().collect();
        assert_eq!(weather_icon_for(&refs), "⛅");
    }

    #[test]
    fn weather_icon_set_by_compute_day() {
        let hs: Vec<_> = (6..18)
            .map(|h| with_clouds(t(2026, 4, 26, h), 10.0))
            .collect();
        let day = day_window(6, 12);
        let now = t(2026, 4, 26, 5);
        let ds = compute_day(&hs, day, 0.0, now);
        assert_eq!(ds.weather_icon, "☀");
    }
}
