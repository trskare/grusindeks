//! Grusindeks — the "how good is it to ride gravel right now" score.
//!
//! All scores are integers 0–100, higher is better. The total is a weighted
//! average of five sub-scores plus two **hard caps** for genuine
//! deal-breakers (heavy rain, storm-force wind), since a linear average
//! would be too forgiving in those cases.

use serde::{Deserialize, Serialize};

use crate::drying::SurfaceState;
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

    // ---- Ground optimum / dryness ----
    /// Surface water level where the ground subscore peaks. Lett fuktig
    /// grus pakker seg og ruller best — bone-dry is slightly worse, very
    /// wet is much worse.
    pub const GROUND_OPTIMAL_MM: f64 = 0.4;
    /// Subscore at exactly 0 mm accumulated water — small bonus for "lett
    /// fuktig" without hammering the score on a clear summer day.
    pub const GROUND_DRY_FLOOR_SCORE: u8 = 95;

    /// Hours-since-meaningful-rain at which the drought penalty starts to
    /// kick in (3 døgn).
    pub const DROUGHT_TRIGGER_HOURS: f64 = 72.0;
    /// Hours at which the drought penalty saturates (7 døgn).
    pub const DROUGHT_FULL_HOURS: f64 = 168.0;
    /// Maximum points the drought penalty can subtract from the ground
    /// subscore. Soft hint by design — wet ground stays the dominant
    /// negative factor.
    pub const DROUGHT_MAX_PENALTY: u8 = 10;

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

/// Which axis a penalty came from. `HardCap` is reserved for the
/// global "stop the score from being misleadingly high" rule when the
/// forecast contains heavy rain or storm-force wind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Component {
    Temperature,
    Wind,
    Precipitation,
    PrecipProbability,
    Ground,
    HardCap,
}

/// How much a penalty hurt the score. Ordered so `Critical > Major > Minor`,
/// which lets the renderer sort `Vec<Penalty>` and show the worst first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Minor,
    Major,
    Critical,
}

/// A human-readable explanation for why a sub-score got reduced. The
/// `message_no` is in Norwegian and includes the relevant numeric values
/// (wind speed, mm of rain, °C, etc.) so the CLI can render it as-is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Penalty {
    pub component: Component,
    pub severity: Severity,
    pub message_no: String,
}

/// The Grusindeks for one location and ride window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grusindeks {
    pub total: u8,
    pub breakdown: ScoreBreakdown,
    pub label: &'static str,
    /// `true` when a hard cap (heavy rain or storm wind) clamped the total.
    pub hard_capped: bool,
    /// Explanations for the sub-scores that got reduced. Sorted by
    /// `severity` descending so consumers can take the first N to surface
    /// the most important ones.
    #[serde(default)]
    pub penalties: Vec<Penalty>,
}

/// Compute the Grusindeks for the slice of `hours` that overlap `window`,
/// given the current `surface` state (from the drying model — accumulated
/// surface water plus hours-since-meaningful-rain for drought detection).
pub fn score(hours: &[HourlyConditions], window: RideWindow, surface: SurfaceState) -> Grusindeks {
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

    let max_gust_opt = max_gust.is_finite().then_some(max_gust);
    let max_prob_opt = max_prob.is_finite().then_some(max_prob);

    let raw_ground = ground_subscore(surface.accumulated_mm);
    let breakdown = ScoreBreakdown {
        temperature: temp_subscore(mean_temp),
        wind: wind_subscore(mean_wind, max_gust_opt),
        precipitation: precip_subscore(mean_precip),
        precip_probability: precip_prob_subscore(max_prob_opt),
        ground: apply_drought_penalty(raw_ground, surface.hours_since_meaningful_rain),
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

    let mut penalties = Vec::new();
    if let Some(p) = temp_penalty(breakdown.temperature, mean_temp) {
        penalties.push(p);
    }
    if let Some(p) = wind_penalty(breakdown.wind, mean_wind, max_gust_opt) {
        penalties.push(p);
    }
    if let Some(p) = precip_penalty(breakdown.precipitation, mean_precip) {
        penalties.push(p);
    }
    if let Some(p) = prob_penalty(breakdown.precip_probability, max_prob_opt) {
        penalties.push(p);
    }
    if let Some(p) = ground_penalty(breakdown.ground, surface.accumulated_mm) {
        penalties.push(p);
    }
    if let Some(p) = drought_penalty(breakdown.ground, surface) {
        penalties.push(p);
    }
    if hard_capped {
        if max_precip.is_finite() && max_precip > thresholds::HARD_CAP_PRECIP_MM_PER_HOUR {
            penalties.push(Penalty {
                component: Component::HardCap,
                severity: Severity::Critical,
                message_no: format!(
                    "Kraftig regn ventet ({max_precip:.1} mm/t) — score hard-cappet"
                ),
            });
        }
        if max_wind.is_finite() && max_wind > thresholds::HARD_CAP_WIND_MS {
            penalties.push(Penalty {
                component: Component::HardCap,
                severity: Severity::Critical,
                message_no: format!(
                    "Stormvind ventet ({max_wind:.1} m/s) — score hard-cappet"
                ),
            });
        }
    }
    // Worst penalty first so the renderer can take the head.
    penalties.sort_by(|a, b| b.severity.cmp(&a.severity));

    Grusindeks {
        total,
        breakdown,
        label: label_for(total),
        hard_capped,
        penalties,
    }
}

/// Severity bucketing for sub-score deficits. `Critical` is reserved for
/// hard-cap penalties (the global "stop misleading the user" rule), so the
/// per-axis penalties only ever produce `Minor` or `Major`.
fn severity_for(subscore: u8) -> Option<Severity> {
    match subscore {
        80..=100 => None,
        60..=79 => Some(Severity::Minor),
        _ => Some(Severity::Major),
    }
}

fn temp_penalty(subscore: u8, mean_temp: f64) -> Option<Penalty> {
    let severity = severity_for(subscore)?;
    let message_no = if mean_temp < thresholds::TEMP_OPTIMAL_LOW {
        format!("kjølig, snitt {mean_temp:.0} °C")
    } else {
        format!("varmt, snitt {mean_temp:.0} °C")
    };
    Some(Penalty {
        component: Component::Temperature,
        severity,
        message_no,
    })
}

fn wind_penalty(subscore: u8, mean_wind: f64, max_gust: Option<f64>) -> Option<Penalty> {
    let severity = severity_for(subscore)?;
    let gusty = matches!(
        max_gust,
        Some(g) if mean_wind > 0.0 && g > thresholds::GUST_RATIO_THRESHOLD * mean_wind
    );
    let message_no = match (mean_wind, gusty, max_gust) {
        // Gust dominated, mean still calm.
        (m, true, Some(g)) if m <= thresholds::WIND_PERFECT_MAX => {
            format!("kastevind opp i {g:.0} m/s")
        }
        // Both wind and gust contribute.
        (m, true, Some(g)) => {
            format!("snitt {m:.1} m/s, kastevind opp i {g:.0} m/s")
        }
        // Plain wind, no notable gust.
        (m, _, _) if m > thresholds::WIND_OK_MAX => {
            format!("snitt {m:.1} m/s — mye vind")
        }
        (m, _, _) => format!("snitt {m:.1} m/s"),
    };
    Some(Penalty {
        component: Component::Wind,
        severity,
        message_no,
    })
}

fn precip_penalty(subscore: u8, mean_precip: f64) -> Option<Penalty> {
    let severity = severity_for(subscore)?;
    let message_no = if mean_precip > thresholds::PRECIP_HEAVY {
        format!("kraftig regn, snitt {mean_precip:.1} mm/t")
    } else {
        format!("{mean_precip:.1} mm/t i snitt")
    };
    Some(Penalty {
        component: Component::Precipitation,
        severity,
        message_no,
    })
}

/// Probability penalty only fires when we actually have a probability
/// value. With no data the sub-score defaults to a neutral 50, and
/// blaming the score on "0 %" would mislead the user — they'd read it as
/// "the model is sure it won't rain".
fn prob_penalty(subscore: u8, max_prob: Option<f64>) -> Option<Penalty> {
    let severity = severity_for(subscore)?;
    let p = max_prob?.clamp(0.0, 100.0);
    Some(Penalty {
        component: Component::PrecipProbability,
        severity,
        message_no: format!("{p:.0} % sjanse for nedbør"),
    })
}

fn ground_penalty(subscore: u8, ground_water_mm: f64) -> Option<Penalty> {
    let severity = severity_for(subscore)?;
    let message_no = if ground_water_mm >= thresholds::GROUND_SATURATED {
        format!("gjennomvåt ({ground_water_mm:.1} mm akkumulert)")
    } else {
        format!("våt fra forrige døgn ({ground_water_mm:.1} mm)")
    };
    Some(Penalty {
        component: Component::Ground,
        severity,
        message_no,
    })
}

/// Drought-side counterpart to `ground_penalty`. Surfaces a Minor Penalty
/// when the surface has been parched long enough that the ground
/// subscore actually got knocked down by `apply_drought_penalty`. Skipped
/// when the surface is wet (`accumulated_mm` past the optimum) — that
/// case is already explained by `ground_penalty`.
fn drought_penalty(ground_subscore_after: u8, surface: SurfaceState) -> Option<Penalty> {
    if surface.hours_since_meaningful_rain <= thresholds::DROUGHT_TRIGGER_HOURS {
        return None;
    }
    if surface.accumulated_mm > thresholds::GROUND_OPTIMAL_MM {
        return None;
    }
    // Only emit when the drought penalty actually moved the score; if the
    // hours just barely cleared the trigger we'd round to zero.
    let raw = ground_subscore(surface.accumulated_mm);
    if raw == ground_subscore_after {
        return None;
    }
    let days = (surface.hours_since_meaningful_rain / 24.0).round() as u32;
    Some(Penalty {
        component: Component::Ground,
        severity: Severity::Minor,
        message_no: format!("tørt og løst dekke ({days} døgn uten regn)"),
    })
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

/// Peaks at `GROUND_OPTIMAL_MM` (lett fuktig grus). Bone-dry sits at
/// `GROUND_DRY_FLOOR_SCORE` — slightly worse than optimum but no
/// dramatic drop; the bigger dryness penalty comes from
/// `apply_drought_penalty` once it's been parched for several days.
/// Past optimum, the curve drops linearly to 0 at `GROUND_SATURATED`.
pub fn ground_subscore(accumulated_mm: f64) -> u8 {
    use thresholds::*;
    if accumulated_mm <= 0.0 {
        GROUND_DRY_FLOOR_SCORE
    } else if accumulated_mm < GROUND_OPTIMAL_MM {
        lerp_clamped(
            accumulated_mm,
            0.0,
            GROUND_OPTIMAL_MM,
            GROUND_DRY_FLOOR_SCORE,
            100,
        )
    } else if accumulated_mm >= GROUND_SATURATED {
        0
    } else {
        lerp_clamped(accumulated_mm, GROUND_OPTIMAL_MM, GROUND_SATURATED, 100, 0)
    }
}

/// Subtract drought points from `ground_score`. Zero effect under the
/// trigger, ramps linearly to `DROUGHT_MAX_PENALTY` at the full
/// threshold. Together with the dry-side floor in `ground_subscore`, the
/// total ground-axis loss caps at ~15 points.
pub fn apply_drought_penalty(ground_score: u8, hours_since_meaningful_rain: f64) -> u8 {
    use thresholds::*;
    if hours_since_meaningful_rain <= DROUGHT_TRIGGER_HOURS {
        return ground_score;
    }
    let span = DROUGHT_FULL_HOURS - DROUGHT_TRIGGER_HOURS;
    let frac = ((hours_since_meaningful_rain - DROUGHT_TRIGGER_HOURS) / span).clamp(0.0, 1.0);
    let penalty = (frac * f64::from(DROUGHT_MAX_PENALTY)).round() as u8;
    ground_score.saturating_sub(penalty)
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
    // Bone-dry sits at the dry-side floor (lett fuktig is the optimum).
    #[case(0.0, thresholds::GROUND_DRY_FLOOR_SCORE)]
    // Halfway between 0 mm and the optimum: 95 → 100 lerp at midpoint.
    #[case(0.2, 98)]
    // Optimum.
    #[case(thresholds::GROUND_OPTIMAL_MM, 100)]
    // Wet side: linear from 100 at 0.4 mm to 0 at 5 mm — 2.5 mm is at
    // (5.0 − 2.5) / (5.0 − 0.4) ≈ 54.3 % of the way down.
    #[case(2.5, 54)]
    #[case(5.0, 0)]
    #[case(10.0, 0)] // beyond saturated
    fn ground_subscore_examples(#[case] mm: f64, #[case] expected: u8) {
        assert_eq!(ground_subscore(mm), expected);
    }

    // ---- apply_drought_penalty ----

    #[rstest]
    #[case(60.0, 100, 100)] // under trigger → no change
    #[case(72.0, 100, 100)] // exactly at trigger → no change
    #[case(120.0, 100, 95)] // halfway through → -5
    #[case(168.0, 100, 90)] // saturates at -10
    #[case(240.0, 100, 90)] // beyond → still -10
    #[case(120.0, 3, 0)] // saturating subtract floors at 0
    fn drought_penalty_examples(
        #[case] hours: f64,
        #[case] base: u8,
        #[case] expected: u8,
    ) {
        assert_eq!(apply_drought_penalty(base, hours), expected);
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
        let s = score(&hours, window, SurfaceState::default());
        assert!(!s.hard_capped);
        // Temp=100, wind=100, precip=100, prob=95, ground=95 (lett-fuktig
        // optimum sits at 0.4 mm; 0 mm scores GROUND_DRY_FLOOR_SCORE = 95).
        // Weighted: 15*100 + 20*100 + 25*100 + 10*95 + 30*95 = 9800 / 100 = 98
        assert_eq!(s.total, 98);
        assert_eq!(s.label, "Strålende");
    }

    #[test]
    fn heavy_rain_triggers_hard_cap() {
        let mut hours = (14..17).map(nice_hour).collect::<Vec<_>>();
        hours[1].precipitation_mm = 6.0; // > 5 mm/h hard-cap threshold
        let window = RideWindow::from_hours(t(14), 3);
        let s = score(&hours, window, SurfaceState::default());
        assert!(s.hard_capped);
        assert!(s.total <= thresholds::HARD_CAP_TOTAL);
    }

    #[test]
    fn storm_wind_triggers_hard_cap() {
        let mut hours = (14..17).map(nice_hour).collect::<Vec<_>>();
        hours[2].wind_speed_ms = 18.0; // > 15 m/s
        let window = RideWindow::from_hours(t(14), 3);
        let s = score(&hours, window, SurfaceState::default());
        assert!(s.hard_capped);
        assert!(s.total <= thresholds::HARD_CAP_TOTAL);
    }

    #[test]
    fn saturated_ground_drags_score_down_without_hard_cap() {
        let hours = (14..17).map(nice_hour).collect::<Vec<_>>();
        let window = RideWindow::from_hours(t(14), 3);
        let dry = score(&hours, window, SurfaceState::default());
        let wet = score(&hours, window, SurfaceState::new(5.0)); // ground saturated
        assert!(!wet.hard_capped);
        assert!(wet.total < dry.total);
        // Ground sub-score worth 30 points; bone-dry now sits at 95 (not
        // 100), so the integer-rounded total diff is 29, not 30.
        assert_eq!(dry.total - wet.total, 29);
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
        let s = score(&hours, window, SurfaceState::default());
        assert!(!s.hard_capped);
        assert_eq!(s.total, 98);
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
            SurfaceState::default(),
        );
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"total\""));
        assert!(json.contains("\"breakdown\""));
        assert!(json.contains("\"temperature\""));
        assert!(json.contains("\"ground\""));
    }

    // ---- Penalties ----
    //
    // A `Penalty` explains *why* a sub-score got reduced. The CLI uses these
    // to print "└─ Vind: kastevind opp i 14 m/s" lines. Severity ordering
    // matters because the renderer shows the worst penalty per day first.

    #[test]
    fn perfect_conditions_emit_no_penalties() {
        let hours = (14..17).map(nice_hour).collect::<Vec<_>>();
        let s = score(&hours, RideWindow::from_hours(t(14), 3), SurfaceState::default());
        assert!(
            s.penalties.is_empty(),
            "expected no penalties on a strålende day, got {:?}",
            s.penalties
        );
    }

    #[test]
    fn cold_day_emits_minor_or_major_temperature_penalty() {
        // 0°C → temp_subscore ≈ 35 (between -5 and 12)
        let hours: Vec<HourlyConditions> = (14..17)
            .map(|h| HourlyConditions::minimal(t(h), 0.0, 2.0, 0.0))
            .collect();
        let s = score(&hours, RideWindow::from_hours(t(14), 3), SurfaceState::default());
        let temp_pen = s
            .penalties
            .iter()
            .find(|p| p.component == Component::Temperature)
            .expect("expected a temperature penalty for 0°C");
        assert!(
            matches!(temp_pen.severity, Severity::Minor | Severity::Major),
            "got severity {:?}",
            temp_pen.severity
        );
        assert!(
            temp_pen.message_no.contains("0"),
            "message should mention the temperature: {:?}",
            temp_pen.message_no
        );
    }

    #[test]
    fn windy_day_emits_wind_penalty_with_speed_in_message() {
        // mean wind 9 m/s → wind_subscore ≈ 44 (Major)
        let hours: Vec<HourlyConditions> = (14..17)
            .map(|h| HourlyConditions::minimal(t(h), 17.0, 9.0, 0.0))
            .collect();
        let s = score(&hours, RideWindow::from_hours(t(14), 3), SurfaceState::default());
        let wind_pen = s
            .penalties
            .iter()
            .find(|p| p.component == Component::Wind)
            .expect("expected wind penalty for 9 m/s");
        assert_eq!(wind_pen.severity, Severity::Major);
        assert!(
            wind_pen.message_no.contains("9"),
            "wind message should cite the speed: {:?}",
            wind_pen.message_no
        );
    }

    #[test]
    fn gusty_day_mentions_kastevind_in_wind_message() {
        // Calm mean (4 m/s) but strong gusts (8 m/s = 2× mean).
        let hours: Vec<HourlyConditions> = (14..17)
            .map(|h| HourlyConditions {
                wind_gust_ms: Some(8.0),
                ..HourlyConditions::minimal(t(h), 17.0, 4.0, 0.0)
            })
            .collect();
        let s = score(&hours, RideWindow::from_hours(t(14), 3), SurfaceState::default());
        let wind_pen = s
            .penalties
            .iter()
            .find(|p| p.component == Component::Wind)
            .expect("gust penalty applied → expected a wind Penalty");
        assert!(
            wind_pen.message_no.to_lowercase().contains("kast"),
            "expected gust mention in {:?}",
            wind_pen.message_no
        );
        assert!(
            wind_pen.message_no.contains("8"),
            "expected gust value in {:?}",
            wind_pen.message_no
        );
    }

    #[test]
    fn rainy_day_emits_precipitation_penalty() {
        let hours: Vec<HourlyConditions> = (14..17)
            .map(|h| HourlyConditions::minimal(t(h), 17.0, 2.0, 1.0))
            .collect();
        let s = score(&hours, RideWindow::from_hours(t(14), 3), SurfaceState::default());
        assert!(
            s.penalties
                .iter()
                .any(|p| p.component == Component::Precipitation),
            "expected precip penalty, got {:?}",
            s.penalties
        );
    }

    #[test]
    fn high_probability_emits_probability_penalty_when_amount_is_low() {
        // Dry forecast (0 mm) but the model is 90 % sure it'll rain — the
        // probability sub-score drops independently.
        let hours: Vec<HourlyConditions> = (14..17)
            .map(|h| HourlyConditions {
                probability_of_precip: Some(90.0),
                ..HourlyConditions::minimal(t(h), 17.0, 2.0, 0.0)
            })
            .collect();
        let s = score(&hours, RideWindow::from_hours(t(14), 3), SurfaceState::default());
        let prob_pen = s
            .penalties
            .iter()
            .find(|p| p.component == Component::PrecipProbability)
            .expect("expected probability penalty for 90 %");
        assert!(
            prob_pen.message_no.contains("90"),
            "expected 90 %% in {:?}",
            prob_pen.message_no
        );
    }

    #[test]
    fn saturated_ground_emits_ground_penalty() {
        let hours = (14..17).map(nice_hour).collect::<Vec<_>>();
        let s = score(&hours, RideWindow::from_hours(t(14), 3), SurfaceState::new(5.0));
        let g = s
            .penalties
            .iter()
            .find(|p| p.component == Component::Ground)
            .expect("saturated ground → expected ground penalty");
        assert_eq!(g.severity, Severity::Major);
    }

    #[test]
    fn hard_cap_emits_critical_penalty() {
        let mut hours = (14..17).map(nice_hour).collect::<Vec<_>>();
        hours[1].precipitation_mm = 6.0; // > 5 mm/h
        let s = score(&hours, RideWindow::from_hours(t(14), 3), SurfaceState::default());
        assert!(s.hard_capped);
        let crit = s
            .penalties
            .iter()
            .find(|p| p.severity == Severity::Critical)
            .expect("hard-cap → expected a Critical penalty");
        assert_eq!(crit.component, Component::HardCap);
        assert!(
            crit.message_no.to_lowercase().contains("kraftig")
                || crit.message_no.to_lowercase().contains("regn"),
            "hard-cap message should mention rain: {:?}",
            crit.message_no
        );
    }

    #[test]
    fn storm_wind_hard_cap_emits_critical_penalty_mentioning_wind() {
        let mut hours = (14..17).map(nice_hour).collect::<Vec<_>>();
        hours[2].wind_speed_ms = 18.0;
        let s = score(&hours, RideWindow::from_hours(t(14), 3), SurfaceState::default());
        assert!(s.hard_capped);
        let crit = s
            .penalties
            .iter()
            .find(|p| p.severity == Severity::Critical && p.component == Component::HardCap)
            .expect("storm hard-cap → expected a Critical/HardCap penalty");
        assert!(
            crit.message_no.to_lowercase().contains("storm")
                || crit.message_no.to_lowercase().contains("vind"),
            "storm message should mention wind: {:?}",
            crit.message_no
        );
    }

    #[test]
    fn penalties_sorted_by_severity_descending() {
        // Engineered to emit at least one Critical (hard cap), one Major
        // (saturated ground), and one Minor (slight breeze).
        let hours: Vec<HourlyConditions> = (14..17)
            .map(|h| HourlyConditions::minimal(t(h), 17.0, 5.0, 0.0))
            .collect();
        let mut hs = hours.clone();
        hs[0].precipitation_mm = 6.0;
        let s = score(&hs, RideWindow::from_hours(t(14), 3), SurfaceState::new(5.0));
        assert!(
            s.penalties.windows(2).all(|w| w[0].severity >= w[1].severity),
            "penalties not sorted desc: {:?}",
            s.penalties
        );
    }

    #[test]
    fn severity_orders_critical_above_major_above_minor() {
        assert!(Severity::Critical > Severity::Major);
        assert!(Severity::Major > Severity::Minor);
    }

    #[test]
    fn penalty_serializes_with_lowercase_component_and_severity() {
        let mut hours = (14..17).map(nice_hour).collect::<Vec<_>>();
        hours[1].precipitation_mm = 6.0;
        let s = score(&hours, RideWindow::from_hours(t(14), 3), SurfaceState::default());
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"penalties\""), "got {json}");
        // Component & Severity both render as lowercase strings.
        assert!(json.contains("\"hard_cap\""), "got {json}");
        assert!(json.contains("\"critical\""), "got {json}");
    }

    // ---- drought integration ----

    #[test]
    fn score_emits_drought_penalty_after_long_dry_spell() {
        let hours = (14..17).map(nice_hour).collect::<Vec<_>>();
        let surface = SurfaceState {
            accumulated_mm: 0.0,
            hours_since_meaningful_rain: 96.0, // 4 days
        };
        let s = score(&hours, RideWindow::from_hours(t(14), 3), surface);
        let drought = s
            .penalties
            .iter()
            .find(|p| {
                p.component == Component::Ground
                    && p.message_no.to_lowercase().contains("tørt")
            })
            .expect("expected a tørke penalty after 96h drought");
        assert_eq!(drought.severity, Severity::Minor);
        assert!(
            drought.message_no.contains("4 døgn"),
            "expected '4 døgn' in {:?}",
            drought.message_no
        );
    }

    #[test]
    fn score_drought_does_not_fire_when_ground_is_wet() {
        // Counter is high but ground is past the optimum — the wetness
        // penalty owns this case; no drought penalty should appear.
        let hours = (14..17).map(nice_hour).collect::<Vec<_>>();
        let surface = SurfaceState {
            accumulated_mm: 5.0,
            hours_since_meaningful_rain: 200.0,
        };
        let s = score(&hours, RideWindow::from_hours(t(14), 3), surface);
        let drought = s
            .penalties
            .iter()
            .find(|p| p.message_no.to_lowercase().contains("tørt"));
        assert!(drought.is_none(), "got {:?}", s.penalties);
    }

    #[test]
    fn score_dry_and_drought_combine_within_soft_hint_budget() {
        // Worst-case dry+drought against a perfect day: lett-fuktig floor
        // (-5 on ground subscore) plus full drought (-10) is bounded
        // within the agreed "soft hint" budget — total drop ≤ 5 points.
        let hours = (14..17).map(nice_hour).collect::<Vec<_>>();
        let baseline = score(
            &hours,
            RideWindow::from_hours(t(14), 3),
            SurfaceState {
                accumulated_mm: thresholds::GROUND_OPTIMAL_MM,
                hours_since_meaningful_rain: 0.0,
            },
        );
        let parched = score(
            &hours,
            RideWindow::from_hours(t(14), 3),
            SurfaceState {
                accumulated_mm: 0.0,
                hours_since_meaningful_rain: 240.0,
            },
        );
        assert!(parched.total < baseline.total);
        // Ground delta is 100 → 95-10 = 85, contribution diff 30·15/100 = 4.5
        // → integer total drop should be ≤ 5.
        assert!(
            baseline.total - parched.total <= 5,
            "drop {} exceeds soft-hint budget",
            baseline.total - parched.total
        );
    }
}
