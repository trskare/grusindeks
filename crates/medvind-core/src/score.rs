//! Grusindeks — the "how good is it to ride gravel right now" score.
//!
//! All scores are integers 0–100, higher is better. The total is a weighted
//! average of five sub-scores plus two **hard caps** for genuine
//! deal-breakers (heavy rain, storm-force wind), since a linear average
//! would be too forgiving in those cases.

use serde::{Deserialize, Serialize};

use crate::types::{HourlyConditions, RideWindow};

/// Default thresholds and weights. Held in one place so they're easy to
/// retune without hunting through the file.
pub mod thresholds {
    // ---- Temperature (°C) ----
    pub const TEMP_OPTIMAL_LOW: f64 = 12.0;
    pub const TEMP_OPTIMAL_HIGH: f64 = 22.0;
    pub const TEMP_ZERO_LOW: f64 = -5.0;
    pub const TEMP_ZERO_HIGH: f64 = 35.0;

    // ---- Wind (m/s) ----
    pub const WIND_PERFECT_MAX: f64 = 3.0;
    pub const WIND_OK_MAX: f64 = 7.0; // 100 -> 60 over [3, 7]
    pub const WIND_POOR_MAX: f64 = 12.0; // 60 -> 20 over [7, 12]
    pub const WIND_OK_AT_OK_MAX: u8 = 60;
    pub const WIND_POOR_AT_POOR_MAX: u8 = 20;
    /// Penalty (subtracted from wind sub-score) when gust > 1.5 × mean wind.
    pub const GUST_PENALTY: i32 = 20;
    pub const GUST_RATIO_THRESHOLD: f64 = 1.5;

    // ---- Precipitation (mm/h, mean over ride window) ----
    pub const PRECIP_DRIZZLE: f64 = 0.5; // 100 -> 60
    pub const PRECIP_HEAVY: f64 = 2.0; // 60 -> 20
    pub const PRECIP_DRIZZLE_AT: u8 = 60;
    pub const PRECIP_HEAVY_AT: u8 = 20;

    // ---- Ground saturation (mm of accumulated water) ----
    pub const GROUND_SATURATED: f64 = 5.0;

    // ---- Hard caps ----
    pub const HARD_CAP_PRECIP_MM_PER_HOUR: f64 = 5.0;
    pub const HARD_CAP_WIND_MS: f64 = 15.0;
    pub const HARD_CAP_TOTAL: u8 = 25;

    // ---- Weights (must sum to 100) ----
    pub const W_TEMP: u8 = 15;
    pub const W_WIND: u8 = 20;
    pub const W_PRECIP: u8 = 25;
    pub const W_PROB: u8 = 10;
    pub const W_GROUND: u8 = 30;

    pub const fn weights_sum() -> u8 {
        W_TEMP + W_WIND + W_PRECIP + W_PROB + W_GROUND
    }
}

/// Per-axis 0–100 sub-scores. Useful for the `--verbose` breakdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScoreBreakdown {
    pub temperature: u8,
    pub wind: u8,
    pub precipitation: u8,
    pub precip_probability: u8,
    pub ground: u8,
}

/// The Grusindeks for one location and ride window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grusindeks {
    pub total: u8,
    pub breakdown: ScoreBreakdown,
    pub label: &'static str,
    /// `true` when a hard cap (heavy rain or storm wind) clamped the total.
    pub hard_capped: bool,
}

/// Compute the Grusindeks for the slice of `hours` that overlap `window`,
/// given the current `ground_water_mm` (from the drying model).
pub fn score(hours: &[HourlyConditions], window: RideWindow, ground_water_mm: f64) -> Grusindeks {
    let in_window: Vec<&HourlyConditions> =
        hours.iter().filter(|h| window.contains(h.time)).collect();

    // Mean of the relevant signals over the window. Empty input: fall back
    // to a single neutral hour so the score is well-defined.
    let n = in_window.len().max(1) as f64;
    let mean_temp = in_window.iter().map(|h| h.temperature_c).sum::<f64>() / n.max(1.0);
    let mean_wind = in_window.iter().map(|h| h.wind_speed_ms).sum::<f64>() / n.max(1.0);
    let max_gust = in_window
        .iter()
        .filter_map(|h| h.wind_gust_ms)
        .fold(f64::NEG_INFINITY, f64::max);
    let mean_precip = in_window.iter().map(|h| h.precipitation_mm).sum::<f64>() / n.max(1.0);
    let max_precip = in_window
        .iter()
        .map(|h| h.precipitation_mm)
        .fold(f64::NEG_INFINITY, f64::max);
    let max_wind = in_window
        .iter()
        .map(|h| h.wind_speed_ms)
        .fold(f64::NEG_INFINITY, f64::max);
    let max_prob = in_window
        .iter()
        .filter_map(|h| h.probability_of_precip)
        .fold(f64::NEG_INFINITY, f64::max);

    let breakdown = ScoreBreakdown {
        temperature: temp_subscore(mean_temp),
        wind: wind_subscore(mean_wind, max_gust.is_finite().then_some(max_gust)),
        precipitation: precip_subscore(mean_precip),
        precip_probability: precip_prob_subscore(max_prob.is_finite().then_some(max_prob)),
        ground: ground_subscore(ground_water_mm),
    };

    let weighted = u32::from(breakdown.temperature) * u32::from(thresholds::W_TEMP)
        + u32::from(breakdown.wind) * u32::from(thresholds::W_WIND)
        + u32::from(breakdown.precipitation) * u32::from(thresholds::W_PRECIP)
        + u32::from(breakdown.precip_probability) * u32::from(thresholds::W_PROB)
        + u32::from(breakdown.ground) * u32::from(thresholds::W_GROUND);
    let raw_total = (weighted / u32::from(thresholds::weights_sum())) as u8;

    let hard_capped = max_precip.is_finite()
        && max_precip > thresholds::HARD_CAP_PRECIP_MM_PER_HOUR
        || max_wind.is_finite() && max_wind > thresholds::HARD_CAP_WIND_MS;
    let total = if hard_capped {
        raw_total.min(thresholds::HARD_CAP_TOTAL)
    } else {
        raw_total
    };

    Grusindeks {
        total,
        breakdown,
        label: label_for(total),
        hard_capped,
    }
}

/// Map a total to a short Norwegian label.
pub fn label_for(total: u8) -> &'static str {
    match total {
        0..=24 => "Dårlig",
        25..=44 => "Marginalt",
        45..=64 => "OK",
        65..=84 => "Bra",
        _ => "Strålende",
    }
}

// ---- Sub-scores ----

/// 100 inside the optimal band; linear falloff to 0 at the cold/hot
/// extremes.
pub fn temp_subscore(t: f64) -> u8 {
    use thresholds::*;
    if t.is_nan() {
        return 0;
    }
    if (TEMP_OPTIMAL_LOW..=TEMP_OPTIMAL_HIGH).contains(&t) {
        100
    } else if t < TEMP_OPTIMAL_LOW {
        // 0 at TEMP_ZERO_LOW, 100 at TEMP_OPTIMAL_LOW
        lerp_clamped(t, TEMP_ZERO_LOW, TEMP_OPTIMAL_LOW, 0, 100)
    } else {
        lerp_clamped(t, TEMP_OPTIMAL_HIGH, TEMP_ZERO_HIGH, 100, 0)
    }
}

/// Penalizes both mean wind and gusts. A gust > 1.5× mean docks
/// `GUST_PENALTY` points (saturating).
pub fn wind_subscore(mean_ms: f64, gust_ms: Option<f64>) -> u8 {
    use thresholds::*;
    let base: i32 = if mean_ms <= WIND_PERFECT_MAX {
        100
    } else if mean_ms <= WIND_OK_MAX {
        i32::from(lerp_clamped(
            mean_ms,
            WIND_PERFECT_MAX,
            WIND_OK_MAX,
            100,
            WIND_OK_AT_OK_MAX,
        ))
    } else if mean_ms <= WIND_POOR_MAX {
        i32::from(lerp_clamped(
            mean_ms,
            WIND_OK_MAX,
            WIND_POOR_MAX,
            WIND_OK_AT_OK_MAX,
            WIND_POOR_AT_POOR_MAX,
        ))
    } else {
        // Past WIND_POOR_MAX it's already < 20 — keep degrading toward 0 at
        // 20 m/s so the hard-cap logic still has room to bite.
        let extra = (mean_ms - WIND_POOR_MAX).clamp(0.0, 8.0);
        let from_poor = i32::from(WIND_POOR_AT_POOR_MAX) - (extra * 2.5).round() as i32;
        from_poor.max(0)
    };

    let gust_penalty = match gust_ms {
        Some(g) if mean_ms > 0.0 && g > GUST_RATIO_THRESHOLD * mean_ms => GUST_PENALTY,
        _ => 0,
    };

    (base - gust_penalty).clamp(0, 100) as u8
}

/// Mean precipitation intensity (mm/h) over the ride window.
pub fn precip_subscore(mm_per_hour: f64) -> u8 {
    use thresholds::*;
    if mm_per_hour <= 0.0 {
        100
    } else if mm_per_hour <= PRECIP_DRIZZLE {
        lerp_clamped(mm_per_hour, 0.0, PRECIP_DRIZZLE, 100, PRECIP_DRIZZLE_AT)
    } else if mm_per_hour <= PRECIP_HEAVY {
        lerp_clamped(
            mm_per_hour,
            PRECIP_DRIZZLE,
            PRECIP_HEAVY,
            PRECIP_DRIZZLE_AT,
            PRECIP_HEAVY_AT,
        )
    } else {
        // Drop quickly past 2 mm/h; 0 at >= 5 mm/h.
        lerp_clamped(mm_per_hour, PRECIP_HEAVY, 5.0, PRECIP_HEAVY_AT, 0)
    }
}

/// `100 - probability_of_precipitation`. Missing data is neutral (50).
pub fn precip_prob_subscore(prob_pct: Option<f64>) -> u8 {
    match prob_pct {
        Some(p) => (100.0 - p.clamp(0.0, 100.0)).round() as u8,
        None => 50,
    }
}

/// 100 when bone-dry; 0 at ≥ saturated. Linear in between.
pub fn ground_subscore(accumulated_mm: f64) -> u8 {
    use thresholds::*;
    if accumulated_mm <= 0.0 {
        100
    } else if accumulated_mm >= GROUND_SATURATED {
        0
    } else {
        let frac = accumulated_mm / GROUND_SATURATED;
        ((1.0 - frac) * 100.0).round() as u8
    }
}

/// Linear interpolation that saturates outside `[x_lo, x_hi]`. Returns
/// `y_lo` at `x_lo` and `y_hi` at `x_hi`.
fn lerp_clamped(x: f64, x_lo: f64, x_hi: f64, y_lo: u8, y_hi: u8) -> u8 {
    if x <= x_lo {
        return y_lo;
    }
    if x >= x_hi {
        return y_hi;
    }
    let t = (x - x_lo) / (x_hi - x_lo);
    let y = f64::from(y_lo) + t * (f64::from(y_hi) - f64::from(y_lo));
    y.round().clamp(0.0, 100.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use rstest::rstest;

    fn t(h: u32) -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 4, 26, h, 0, 0).unwrap()
    }

    // ---- Sanity check: weights sum to 100 ----

    #[test]
    fn weights_sum_to_100() {
        assert_eq!(thresholds::weights_sum(), 100);
    }

    // ---- temp_subscore ----

    #[rstest]
    #[case(12.0, 100)]
    #[case(17.0, 100)]
    #[case(22.0, 100)]
    #[case(-5.0, 0)]
    #[case(35.0, 0)]
    #[case(-10.0, 0)] // beyond cold
    #[case(40.0, 0)] // beyond hot
    #[case(3.5, 50)] // halfway between -5 and 12
    fn temperature_subscore(#[case] t_c: f64, #[case] expected: u8) {
        assert_eq!(temp_subscore(t_c), expected);
    }

    // ---- wind_subscore ----

    #[rstest]
    #[case(0.0, None, 100)]
    #[case(3.0, None, 100)]
    #[case(7.0, None, 60)]
    #[case(12.0, None, 20)]
    #[case(15.0, None, 12)] // 20 − (3 × 2.5) = 12; > 15 m/s also hits hard cap
    #[case(20.0, None, 0)]
    fn wind_subscore_no_gust(#[case] mean: f64, #[case] gust: Option<f64>, #[case] expected: u8) {
        assert_eq!(wind_subscore(mean, gust), expected);
    }

    #[test]
    fn wind_subscore_gust_penalty_applies_when_gust_over_1_5x() {
        // Mean 4 m/s, gust 7 m/s = 1.75× → penalty.
        let with_gust = wind_subscore(4.0, Some(7.0));
        let no_gust = wind_subscore(4.0, None);
        assert_eq!(with_gust as i32, no_gust as i32 - 20);
    }

    #[test]
    fn wind_subscore_no_penalty_when_gust_close_to_mean() {
        let with_gust = wind_subscore(4.0, Some(5.0)); // 1.25×
        let no_gust = wind_subscore(4.0, None);
        assert_eq!(with_gust, no_gust);
    }

    // ---- precip_subscore ----

    #[rstest]
    #[case(0.0, 100)]
    #[case(0.5, 60)]
    #[case(2.0, 20)]
    #[case(5.0, 0)]
    #[case(10.0, 0)] // saturates
    #[case(0.25, 80)] // halfway in drizzle band
    fn precipitation_subscore(#[case] mm_h: f64, #[case] expected: u8) {
        assert_eq!(precip_subscore(mm_h), expected);
    }

    // ---- precip_prob_subscore ----

    #[rstest]
    #[case(Some(0.0), 100)]
    #[case(Some(50.0), 50)]
    #[case(Some(100.0), 0)]
    #[case(None, 50)] // unknown = neutral
    fn probability_subscore(#[case] prob: Option<f64>, #[case] expected: u8) {
        assert_eq!(precip_prob_subscore(prob), expected);
    }

    // ---- ground_subscore ----

    #[rstest]
    #[case(0.0, 100)]
    #[case(2.5, 50)] // halfway saturated
    #[case(5.0, 0)]
    #[case(10.0, 0)] // beyond saturated
    fn ground_subscore_examples(#[case] mm: f64, #[case] expected: u8) {
        assert_eq!(ground_subscore(mm), expected);
    }

    // ---- score (integration) ----

    fn nice_hour(time_h: u32) -> HourlyConditions {
        // 17°C, 2 m/s wind, no rain, low prob — should yield a perfect score.
        HourlyConditions {
            probability_of_precip: Some(5.0),
            ..HourlyConditions::minimal(t(time_h), 17.0, 2.0, 0.0)
        }
    }

    #[test]
    fn perfect_conditions_yield_high_score() {
        let hours = (14..17).map(nice_hour).collect::<Vec<_>>();
        let window = RideWindow::from_hours(t(14), 3);
        let s = score(&hours, window, 0.0);
        assert!(!s.hard_capped);
        // Temp=100, wind=100, precip=100, prob=95, ground=100
        // Weighted: 15*100 + 20*100 + 25*100 + 10*95 + 30*100 = 9950 / 100 = 99
        assert_eq!(s.total, 99);
        assert_eq!(s.label, "Strålende");
    }

    #[test]
    fn heavy_rain_triggers_hard_cap() {
        let mut hours = (14..17).map(nice_hour).collect::<Vec<_>>();
        hours[1].precipitation_mm = 6.0; // > 5 mm/h hard-cap threshold
        let window = RideWindow::from_hours(t(14), 3);
        let s = score(&hours, window, 0.0);
        assert!(s.hard_capped);
        assert!(s.total <= thresholds::HARD_CAP_TOTAL);
    }

    #[test]
    fn storm_wind_triggers_hard_cap() {
        let mut hours = (14..17).map(nice_hour).collect::<Vec<_>>();
        hours[2].wind_speed_ms = 18.0; // > 15 m/s
        let window = RideWindow::from_hours(t(14), 3);
        let s = score(&hours, window, 0.0);
        assert!(s.hard_capped);
        assert!(s.total <= thresholds::HARD_CAP_TOTAL);
    }

    #[test]
    fn saturated_ground_drags_score_down_without_hard_cap() {
        let hours = (14..17).map(nice_hour).collect::<Vec<_>>();
        let window = RideWindow::from_hours(t(14), 3);
        let dry = score(&hours, window, 0.0);
        let wet = score(&hours, window, 5.0); // ground saturated
        assert!(!wet.hard_capped);
        assert!(wet.total < dry.total);
        // Ground sub-score worth 30 points; difference should be exactly 30.
        assert_eq!(dry.total - wet.total, 30);
    }

    #[test]
    fn score_only_considers_hours_in_window() {
        // Out-of-window hours have terrible weather; should be ignored.
        let mut hours = (14..17).map(nice_hour).collect::<Vec<_>>();
        let mut bad = nice_hour(20);
        bad.precipitation_mm = 100.0;
        bad.wind_speed_ms = 30.0;
        hours.push(bad);
        let window = RideWindow::from_hours(t(14), 3); // 14..17, excludes 20
        let s = score(&hours, window, 0.0);
        assert!(!s.hard_capped);
        assert_eq!(s.total, 99);
    }

    #[test]
    fn label_thresholds() {
        assert_eq!(label_for(0), "Dårlig");
        assert_eq!(label_for(24), "Dårlig");
        assert_eq!(label_for(25), "Marginalt");
        assert_eq!(label_for(44), "Marginalt");
        assert_eq!(label_for(45), "OK");
        assert_eq!(label_for(64), "OK");
        assert_eq!(label_for(65), "Bra");
        assert_eq!(label_for(84), "Bra");
        assert_eq!(label_for(85), "Strålende");
        assert_eq!(label_for(100), "Strålende");
    }

    #[test]
    fn breakdown_serializes_with_field_names() {
        let s = score(
            &(14..17).map(nice_hour).collect::<Vec<_>>(),
            RideWindow::from_hours(t(14), 3),
            0.0,
        );
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"total\""));
        assert!(json.contains("\"breakdown\""));
        assert!(json.contains("\"temperature\""));
        assert!(json.contains("\"ground\""));
    }
}
