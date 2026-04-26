//! A water-balance heuristic for "how wet is the gravel right now?".
//!
//! We track a single `accumulated_water_mm` value per location. Each
//! simulated hour adds the hour's precipitation and subtracts a
//! drying-rate that depends on temperature, wind, sunshine, and humidity.
//! Saturation is capped at `GROUND_SATURATED` mm so a single deluge
//! doesn't take days to "drain" — gravel surfaces drain quickly even when
//! soaked.
//!
//! Not a real Penman–Monteith ET model; it's a transparent heuristic with
//! all coefficients tunable in one place. Good enough for scoring.

use serde::{Deserialize, Serialize};

use crate::score::thresholds::GROUND_SATURATED;
use crate::types::HourlyConditions;

/// Coefficients for the per-hour drying rate (mm/h). Exposed so tests and
/// future calibration can override them without forking the function.
#[derive(Debug, Clone, Copy)]
pub struct DryingParams {
    pub base: f64,
    pub per_deg_c_above_5: f64,
    pub per_ms_wind: f64,
    pub sunshine_max: f64,
    pub per_pct_humidity_above_50: f64,
    pub min_rate: f64,
    pub max_rate: f64,
}

impl Default for DryingParams {
    fn default() -> Self {
        Self {
            base: 0.05,
            per_deg_c_above_5: 0.010,
            per_ms_wind: 0.020,
            sunshine_max: 0.050, // multiplied by (1 - cloud%) × (uv/5)
            per_pct_humidity_above_50: 0.005,
            min_rate: 0.0,
            max_rate: 1.5,
        }
    }
}

/// Mutable state of the water-balance simulation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct DryingState {
    /// Accumulated standing water on the surface, mm.
    pub accumulated_mm: f64,
}

impl DryingState {
    pub fn new(initial_mm: f64) -> Self {
        Self {
            accumulated_mm: initial_mm.max(0.0),
        }
    }

    /// Returns true once the surface has reached saturation.
    pub fn is_saturated(&self) -> bool {
        self.accumulated_mm >= GROUND_SATURATED
    }
}

/// Drying rate (mm/h) for one hour of conditions.
///
/// Sunshine term uses cloud cover and (when available) UV index. With both
/// missing it falls back to "neutral" (no extra sunshine boost).
pub fn drying_rate(h: &HourlyConditions, p: &DryingParams) -> f64 {
    let temp_term = p.per_deg_c_above_5 * (h.temperature_c - 5.0).max(0.0);
    let wind_term = p.per_ms_wind * h.wind_speed_ms.max(0.0);

    let sunshine_term = match (h.cloud_area_fraction, h.uv_index_clear_sky) {
        (Some(cloud), Some(uv)) => {
            p.sunshine_max * (1.0 - cloud / 100.0).clamp(0.0, 1.0) * (uv / 5.0).clamp(0.0, 2.0)
        }
        (Some(cloud), None) => {
            // Without UV we still get a smaller boost from "clear-ish" skies.
            0.5 * p.sunshine_max * (1.0 - cloud / 100.0).clamp(0.0, 1.0)
        }
        _ => 0.0,
    };

    let humidity_penalty = match h.relative_humidity {
        Some(rh) => p.per_pct_humidity_above_50 * (rh - 50.0).max(0.0),
        None => 0.0,
    };

    (p.base + temp_term + wind_term + sunshine_term - humidity_penalty)
        .clamp(p.min_rate, p.max_rate)
}

/// Step the drying state forward by one hour.
///
/// Order: add this hour's precipitation, then subtract drying. Capped to
/// `[0, GROUND_SATURATED]`.
pub fn drying_step(state: DryingState, h: &HourlyConditions, p: &DryingParams) -> DryingState {
    let after_rain = state.accumulated_mm + h.precipitation_mm.max(0.0);
    let after_drying = (after_rain - drying_rate(h, p)).clamp(0.0, GROUND_SATURATED);
    DryingState {
        accumulated_mm: after_drying,
    }
}

/// Replay a sequence of past hours to estimate "how wet is it right now".
/// Hours should be in chronological order. Returns the final state.
pub fn replay(initial: DryingState, history: &[HourlyConditions], p: &DryingParams) -> DryingState {
    history.iter().fold(initial, |s, h| drying_step(s, h, p))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn t(h: u32) -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 4, 26, h, 0, 0).unwrap()
    }

    fn dry_hour(h: u32) -> HourlyConditions {
        HourlyConditions {
            cloud_area_fraction: Some(20.0),
            uv_index_clear_sky: Some(4.0),
            relative_humidity: Some(40.0),
            ..HourlyConditions::minimal(t(h), 18.0, 4.0, 0.0)
        }
    }

    fn rainy_hour(h: u32, mm: f64) -> HourlyConditions {
        HourlyConditions {
            cloud_area_fraction: Some(95.0),
            uv_index_clear_sky: Some(0.0),
            relative_humidity: Some(95.0),
            ..HourlyConditions::minimal(t(h), 12.0, 3.0, mm)
        }
    }

    // ---- DryingState basics ----

    #[test]
    fn new_clamps_negative_to_zero() {
        assert_eq!(DryingState::new(-1.0).accumulated_mm, 0.0);
    }

    #[test]
    fn saturated_threshold() {
        assert!(!DryingState::new(4.99).is_saturated());
        assert!(DryingState::new(5.0).is_saturated());
        assert!(DryingState::new(10.0).is_saturated());
    }

    // ---- drying_rate ----

    #[test]
    fn base_rate_at_5c_no_wind_no_sun() {
        let h = HourlyConditions::minimal(t(0), 5.0, 0.0, 0.0);
        let r = drying_rate(&h, &DryingParams::default());
        assert!((r - 0.05).abs() < 1e-9, "got {r}");
    }

    #[test]
    fn rate_increases_with_temperature() {
        let p = DryingParams::default();
        let cool = drying_rate(&HourlyConditions::minimal(t(0), 5.0, 4.0, 0.0), &p);
        let warm = drying_rate(&HourlyConditions::minimal(t(0), 25.0, 4.0, 0.0), &p);
        assert!(warm > cool, "warm {warm} should exceed cool {cool}");
    }

    #[test]
    fn rate_increases_with_wind() {
        let p = DryingParams::default();
        let calm = drying_rate(&HourlyConditions::minimal(t(0), 15.0, 0.0, 0.0), &p);
        let windy = drying_rate(&HourlyConditions::minimal(t(0), 15.0, 8.0, 0.0), &p);
        assert!(windy > calm);
    }

    #[test]
    fn rate_decreases_with_humidity() {
        let p = DryingParams::default();
        let base = HourlyConditions {
            relative_humidity: Some(40.0),
            ..HourlyConditions::minimal(t(0), 18.0, 4.0, 0.0)
        };
        let humid = HourlyConditions {
            relative_humidity: Some(95.0),
            ..HourlyConditions::minimal(t(0), 18.0, 4.0, 0.0)
        };
        assert!(drying_rate(&base, &p) > drying_rate(&humid, &p));
    }

    #[test]
    fn rate_clamped_to_max() {
        let p = DryingParams::default();
        let extreme = HourlyConditions {
            cloud_area_fraction: Some(0.0),
            uv_index_clear_sky: Some(11.0),
            relative_humidity: Some(10.0),
            ..HourlyConditions::minimal(t(0), 35.0, 20.0, 0.0)
        };
        let r = drying_rate(&extreme, &p);
        assert!(r <= p.max_rate + 1e-9);
    }

    #[test]
    fn rate_clamped_above_zero() {
        let p = DryingParams::default();
        let cold_humid = HourlyConditions {
            cloud_area_fraction: Some(100.0),
            uv_index_clear_sky: Some(0.0),
            relative_humidity: Some(100.0),
            ..HourlyConditions::minimal(t(0), -10.0, 0.0, 0.0)
        };
        assert!(drying_rate(&cold_humid, &p) >= 0.0);
    }

    // ---- drying_step ----

    #[test]
    fn rain_adds_water_during_step() {
        // Rainy conditions push drying near zero (humid, no sun) — what we
        // care about is that the precipitation makes it into the bucket.
        let p = DryingParams::default();
        let s = DryingState::new(0.0);
        let h = rainy_hour(0, 2.0);
        let s2 = drying_step(s, &h, &p);
        assert!(
            s2.accumulated_mm > 1.5,
            "rain should add water; got {}",
            s2.accumulated_mm
        );
        assert!(s2.accumulated_mm <= 2.0);
    }

    #[test]
    fn dry_warm_hour_actually_dries() {
        // Counterpart to the rainy case: with sunshine and warmth, water
        // does decrease.
        let p = DryingParams::default();
        let s = DryingState::new(2.0);
        let h = dry_hour(12);
        let s2 = drying_step(s, &h, &p);
        assert!(s2.accumulated_mm < s.accumulated_mm);
    }

    #[test]
    fn step_caps_at_saturation() {
        let p = DryingParams::default();
        let s = DryingState::new(4.0);
        let h = rainy_hour(0, 50.0); // deluge
        let s2 = drying_step(s, &h, &p);
        assert!(s2.accumulated_mm <= GROUND_SATURATED);
    }

    #[test]
    fn step_floor_zero() {
        let p = DryingParams::default();
        let s = DryingState::new(0.0);
        let h = dry_hour(0);
        let s2 = drying_step(s, &h, &p);
        assert!(s2.accumulated_mm >= 0.0);
        assert_eq!(s2.accumulated_mm, 0.0);
    }

    // ---- replay ----

    #[test]
    fn replay_48h_no_rain_yields_dry() {
        let p = DryingParams::default();
        let history: Vec<_> = (0..48).map(|h| dry_hour(h % 24)).collect();
        let final_state = replay(DryingState::new(3.0), &history, &p);
        assert_eq!(final_state.accumulated_mm, 0.0);
    }

    #[test]
    fn replay_continuous_rain_yields_saturated() {
        let p = DryingParams::default();
        let history: Vec<_> = (0..24).map(|h| rainy_hour(h % 24, 3.0)).collect();
        let final_state = replay(DryingState::new(0.0), &history, &p);
        assert!(final_state.is_saturated());
    }

    #[test]
    fn replay_then_recover() {
        // 6h heavy rain, then 24h of warm dry weather → should largely recover.
        let p = DryingParams::default();
        let mut history: Vec<HourlyConditions> = (0..6).map(|h| rainy_hour(h, 3.0)).collect();
        history.extend((6..30).map(|h| dry_hour(h % 24)));
        let final_state = replay(DryingState::new(0.0), &history, &p);
        assert!(
            final_state.accumulated_mm < 1.0,
            "got {}",
            final_state.accumulated_mm
        );
    }

    #[test]
    fn replay_is_deterministic() {
        let p = DryingParams::default();
        let history: Vec<_> = (0..12)
            .map(|h| {
                if h % 4 == 0 {
                    rainy_hour(h, 1.5)
                } else {
                    dry_hour(h)
                }
            })
            .collect();
        let a = replay(DryingState::new(0.0), &history, &p);
        let b = replay(DryingState::new(0.0), &history, &p);
        assert_eq!(a, b);
    }

    #[test]
    fn replay_empty_history_returns_initial() {
        let p = DryingParams::default();
        let s = DryingState::new(2.5);
        assert_eq!(replay(s, &[], &p), s);
    }
}
