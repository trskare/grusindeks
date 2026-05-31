//! A water-balance heuristic for "how wet is the gravel right now?", plus
//! a longer-timescale "how long since meaningful rain" counter for
//! detecting loose/dusty conditions on the dry end.
//!
//! Each simulated hour caps incoming precipitation at `GROUND_SATURATED`
//! (gravel drains overflow into runoff that the model doesn't track),
//! then subtracts a drying-rate that depends on temperature, wind,
//! sunshine, and humidity. In parallel we keep
//! `hours_since_meaningful_rain`, which resets whenever the post-drying
//! accumulator clears `SURFACE_WETTED_MM` and otherwise increments by
//! one each step. The scoring layer uses that to flag flerdøgnstørke.
//!
//! When the air is below freezing the drying-rate is multiplied by
//! `FROST_FACTOR` since liquid evaporation effectively stops; only slow
//! sublimation remains. Snow accumulation/melt is **not** modelled —
//! callers should treat the score as undefined when snow is on the
//! ground.
//!
//! Not a real Penman–Monteith ET model; it's a transparent heuristic with
//! all coefficients tunable in one place. Good enough for scoring.

use serde::{Deserialize, Serialize};

use crate::score::thresholds::GROUND_SATURATED;
use crate::types::HourlyConditions;

/// Accumulated surface water (post-drying) at which we treat the gravel
/// as having been "actually wet" — enough to repack the surface. Lower
/// than that and a brief shower / drizzle didn't really do anything, so
/// the drought counter keeps climbing. Light continuous drizzle still
/// resets, because the drying model lets it accumulate across hours.
pub const SURFACE_WETTED_MM: f64 = 0.3;

/// Alias for [`SURFACE_WETTED_MM`] — kept because doc comments and the
/// README refer to "meaningful rain". The drought counter resets when the
/// **post-drying** accumulator clears this threshold, not when a single
/// hour delivers this much rain (so 4 hours × 0.1 mm of drizzle on humid
/// air still trips it; 0.5 mm on a hot windy hour does not).
pub const MEANINGFUL_RAIN_MM: f64 = SURFACE_WETTED_MM;

/// Multiplier applied to the per-hour drying rate when the air is below
/// freezing. Liquid evaporation effectively stops; only sublimation
/// continues, which is one-to-two orders of magnitude slower. Not zero
/// so a frozen surface doesn't stay "wet forever" in long winter
/// histories.
pub const FROST_FACTOR: f64 = 0.1;

/// Re-exported from `felt_temp` so the drying model and the cyclist
/// felt-T scale solar contributions against the same Nordic peak. Single
/// source of truth lives in `felt_temp::UV_NORDIC_PEAK` (= 7.0).
pub use crate::felt_temp::UV_NORDIC_PEAK;

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

/// Mutable state of the surface-condition simulation. Tracks both
/// short-timescale wetness (`accumulated_mm`) and long-timescale dryness
/// (`hours_since_meaningful_rain`), since they affect the score in
/// opposite directions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct SurfaceState {
    /// Accumulated standing water on the surface, mm.
    pub accumulated_mm: f64,
    /// Hours elapsed since the last hour that received at least
    /// `MEANINGFUL_RAIN_MM`. Drizzle does not reset this.
    pub hours_since_meaningful_rain: f64,
    /// `true` when the drought counter saturated against the replay
    /// lookback ceiling — i.e. `replay_into_state` walked all the
    /// observations it had without ever seeing the surface get wet.
    /// In that case `hours_since_meaningful_rain` is a *lower bound*
    /// on the true time since rain, not the actual value (the real
    /// drought could be 30 days while we only fed 7 days of Frost
    /// data). Renderers append a `+` to the day-count to flag this
    /// honestly. Defaults to `false`; populated by `replay_into_state`.
    #[serde(default)]
    pub drought_at_lookback_cap: bool,
}

impl SurfaceState {
    /// Build a `SurfaceState` with `initial_mm` of accumulated surface
    /// water. NaN and out-of-range inputs are clamped into
    /// `[0, GROUND_SATURATED]` so downstream code never sees a poisoned
    /// accumulator.
    pub fn new(initial_mm: f64) -> Self {
        let accumulated_mm = if initial_mm.is_finite() {
            initial_mm.clamp(0.0, GROUND_SATURATED)
        } else {
            0.0
        };
        Self {
            accumulated_mm,
            hours_since_meaningful_rain: 0.0,
            drought_at_lookback_cap: false,
        }
    }

    /// Returns true once the surface has reached saturation.
    pub fn is_saturated(&self) -> bool {
        self.accumulated_mm >= GROUND_SATURATED
    }
}

/// Replace NaN / non-finite f64 with `fallback`. Centralises the NaN
/// posture: bad sensor data shouldn't poison the accumulator, so we treat
/// it as the "neutral" value.
fn finite_or(value: f64, fallback: f64) -> f64 {
    if value.is_finite() {
        value
    } else {
        fallback
    }
}

/// Drying rate (mm/h) for one hour of conditions.
///
/// Sunshine term uses cloud cover and (when available) UV index. With both
/// missing it falls back to "neutral" (no extra sunshine boost). The wind
/// term uses √u rather than u directly: aerodynamic mass-transfer is
/// concave in wind speed, so a lineær term would overestimate evaporation
/// at storm strength. The factor of 2 is calibrated so that the mid-range
/// `u ≈ 4 m/s` matches the previous lineær formula.
///
/// When the air is below freezing the entire rate is multiplied by
/// [`FROST_FACTOR`] — liquid evaporation effectively stops, only slow
/// sublimation continues. Without this gating a frozen, snow-free surface
/// would dry at ~0.05 mm/h and fool the drought counter into thinking the
/// gravel is fluffy when it's actually iced over.
pub fn drying_rate(h: &HourlyConditions, p: &DryingParams) -> f64 {
    let temp_c = finite_or(h.temperature_c, 0.0);
    let wind = finite_or(h.wind_speed_ms, 0.0).max(0.0);

    let temp_term = p.per_deg_c_above_5 * (temp_c - 5.0).max(0.0);
    // sqrt is concave: u=4 → 2, u=16 → 4. The ×2 normalises the formula
    // so it matches `per_ms_wind * u` at u ≈ 4 m/s, which is the typical
    // calm-to-light-breeze regime the coefficient was originally tuned
    // against.
    let wind_term = p.per_ms_wind * 2.0 * wind.sqrt();

    let sunshine_term = match (h.cloud_area_fraction, h.uv_index_clear_sky) {
        (Some(cloud), Some(uv)) if cloud.is_finite() && uv.is_finite() => {
            p.sunshine_max
                * (1.0 - cloud / 100.0).clamp(0.0, 1.0)
                * (uv / UV_NORDIC_PEAK).clamp(0.0, 1.5)
        }
        (Some(cloud), _) if cloud.is_finite() => {
            // Without UV we still get a smaller boost from "clear-ish" skies.
            0.5 * p.sunshine_max * (1.0 - cloud / 100.0).clamp(0.0, 1.0)
        }
        _ => 0.0,
    };

    let humidity_penalty = match h.relative_humidity {
        Some(rh) if rh.is_finite() => p.per_pct_humidity_above_50 * (rh - 50.0).max(0.0),
        _ => 0.0,
    };

    let raw = p.base + temp_term + wind_term + sunshine_term - humidity_penalty;
    let frost_factor = if temp_c < 0.0 { FROST_FACTOR } else { 1.0 };
    (raw * frost_factor).clamp(p.min_rate, p.max_rate)
}

/// Step the surface state forward by one hour.
///
/// Order: cap incoming `accumulated + precip` at `GROUND_SATURATED`
/// (overflow is treated as runoff that the model doesn't track), then
/// subtract drying. Capping at the input — not after drying — makes the
/// "vann forsvinner ut av regnskapet"-semantikk eksplisitt and avoids
/// having two scenarios with very different physical meaning (a 50 mm
/// deluge vs 5 hours of light rain) collapse to identical post-drying
/// states.
///
/// Non-finite inputs are sanitised: NaN precipitation is treated as 0,
/// NaN temperature/wind/humidity are treated as neutral by `drying_rate`.
/// This keeps a poisoned sensor reading from corrupting the accumulator.
///
/// The drought counter resets when the post-drying accumulator clears
/// `SURFACE_WETTED_MM`, otherwise increments by one.
pub fn drying_step(state: SurfaceState, h: &HourlyConditions, p: &DryingParams) -> SurfaceState {
    let precip = finite_or(h.precipitation_mm, 0.0).max(0.0);
    let after_rain = (state.accumulated_mm + precip).min(GROUND_SATURATED);
    let after_drying = (after_rain - drying_rate(h, p)).max(0.0);
    let hours_since_meaningful_rain = if after_drying >= SURFACE_WETTED_MM {
        0.0
    } else {
        state.hours_since_meaningful_rain + 1.0
    };
    SurfaceState {
        accumulated_mm: after_drying,
        hours_since_meaningful_rain,
        // The cap flag describes the *replay* result, not a per-hour
        // property — so per-step updates carry it forward unchanged.
        // `replay_into_state` is responsible for setting it based on
        // whether the full walk ever saw a reset.
        drought_at_lookback_cap: state.drought_at_lookback_cap,
    }
}

/// Replay a sequence of past hours to estimate the surface state right
/// now. Hours should be in chronological order. Returns the final state.
pub fn replay(
    initial: SurfaceState,
    history: &[HourlyConditions],
    p: &DryingParams,
) -> SurfaceState {
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

    // ---- SurfaceState basics ----

    #[test]
    fn new_clamps_negative_to_zero() {
        assert_eq!(SurfaceState::new(-1.0).accumulated_mm, 0.0);
    }

    #[test]
    fn saturated_threshold() {
        assert!(!SurfaceState::new(4.99).is_saturated());
        assert!(SurfaceState::new(5.0).is_saturated());
        assert!(SurfaceState::new(10.0).is_saturated());
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
        let s = SurfaceState::new(0.0);
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
        let s = SurfaceState::new(2.0);
        let h = dry_hour(12);
        let s2 = drying_step(s, &h, &p);
        assert!(s2.accumulated_mm < s.accumulated_mm);
    }

    #[test]
    fn step_caps_at_saturation() {
        let p = DryingParams::default();
        let s = SurfaceState::new(4.0);
        let h = rainy_hour(0, 50.0); // deluge
        let s2 = drying_step(s, &h, &p);
        assert!(s2.accumulated_mm <= GROUND_SATURATED);
    }

    #[test]
    fn step_floor_zero() {
        let p = DryingParams::default();
        let s = SurfaceState::new(0.0);
        let h = dry_hour(0);
        let s2 = drying_step(s, &h, &p);
        assert!(s2.accumulated_mm >= 0.0);
        assert_eq!(s2.accumulated_mm, 0.0);
    }

    // ---- drought counter ----

    #[test]
    fn drought_counter_resets_when_surface_actually_gets_wet() {
        let p = DryingParams::default();
        let s = SurfaceState {
            accumulated_mm: 0.0,
            hours_since_meaningful_rain: 50.0,
            drought_at_lookback_cap: false,
        };
        // 0.5 mm shower → after-drying state clears SURFACE_WETTED_MM
        // under the rainy fixture (humid, no sun → drying ≈ 0).
        let s2 = drying_step(s, &rainy_hour(0, 0.5), &p);
        assert_eq!(s2.hours_since_meaningful_rain, 0.0);
    }

    #[test]
    fn drought_counter_does_not_reset_on_micro_sprinkle() {
        let p = DryingParams::default();
        let s = SurfaceState {
            accumulated_mm: 0.0,
            hours_since_meaningful_rain: 50.0,
            drought_at_lookback_cap: false,
        };
        // 0.1 mm in one hour stays well below the wetted threshold.
        let s2 = drying_step(s, &rainy_hour(0, 0.1), &p);
        assert_eq!(s2.hours_since_meaningful_rain, 51.0);
    }

    #[test]
    fn drought_counter_resets_after_drizzle_accumulates() {
        // Several light hours that each fall short individually but
        // collectively wet the surface should reset the counter.
        let p = DryingParams::default();
        let mut s = SurfaceState {
            accumulated_mm: 0.0,
            hours_since_meaningful_rain: 50.0,
            drought_at_lookback_cap: false,
        };
        for h in 0..4 {
            s = drying_step(s, &rainy_hour(h, 0.2), &p);
        }
        assert_eq!(s.hours_since_meaningful_rain, 0.0);
    }

    #[test]
    fn drought_counter_increments_on_dry_hours() {
        let p = DryingParams::default();
        let mut s = SurfaceState::default();
        for h in 0..3 {
            s = drying_step(s, &dry_hour(h), &p);
        }
        assert_eq!(s.hours_since_meaningful_rain, 3.0);
    }

    // ---- replay ----

    #[test]
    fn replay_48h_no_rain_yields_dry() {
        let p = DryingParams::default();
        let history: Vec<_> = (0..48).map(|h| dry_hour(h % 24)).collect();
        let final_state = replay(SurfaceState::new(3.0), &history, &p);
        assert_eq!(final_state.accumulated_mm, 0.0);
    }

    #[test]
    fn replay_continuous_rain_yields_saturated() {
        let p = DryingParams::default();
        let history: Vec<_> = (0..24).map(|h| rainy_hour(h % 24, 3.0)).collect();
        let final_state = replay(SurfaceState::new(0.0), &history, &p);
        assert!(final_state.is_saturated());
    }

    #[test]
    fn replay_then_recover() {
        // 6h heavy rain, then 24h of warm dry weather → should largely recover.
        let p = DryingParams::default();
        let mut history: Vec<HourlyConditions> = (0..6).map(|h| rainy_hour(h, 3.0)).collect();
        history.extend((6..30).map(|h| dry_hour(h % 24)));
        let final_state = replay(SurfaceState::new(0.0), &history, &p);
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
        let a = replay(SurfaceState::new(0.0), &history, &p);
        let b = replay(SurfaceState::new(0.0), &history, &p);
        assert_eq!(a, b);
    }

    #[test]
    fn replay_empty_history_returns_initial() {
        let p = DryingParams::default();
        let s = SurfaceState::new(2.5);
        assert_eq!(replay(s, &[], &p), s);
    }

    #[test]
    fn replay_tracks_drought_across_history() {
        // 7 dry days = 168 dry hours since the start; no rain ever fell, so
        // the counter just increments by the history length.
        let p = DryingParams::default();
        let history: Vec<_> = (0..168).map(|h| dry_hour(h % 24)).collect();
        let final_state = replay(SurfaceState::default(), &history, &p);
        assert_eq!(final_state.hours_since_meaningful_rain, 168.0);
    }

    #[test]
    fn replay_drought_counter_picks_up_from_last_rain() {
        // 24 h of solid rain (saturates the surface to 5 mm), then 100 h
        // of warm dry hours. The drying model takes ~17 hours to push
        // the surface below SURFACE_WETTED_MM, during which the counter
        // keeps resetting; afterwards it climbs each hour. Allow a wide
        // band — the exact value depends on the drying-rate
        // coefficients, which we don't want this test to lock down.
        let p = DryingParams::default();
        let mut history: Vec<HourlyConditions> = (0..24).map(|h| rainy_hour(h, 2.0)).collect();
        history.extend((24..124).map(|h| dry_hour(h % 24)));
        let final_state = replay(SurfaceState::default(), &history, &p);
        assert!(
            (60.0..=100.0).contains(&final_state.hours_since_meaningful_rain),
            "got {}",
            final_state.hours_since_meaningful_rain,
        );
    }

    // ---- NaN sanitisation ----

    #[test]
    fn surface_state_new_clamps_above_saturation() {
        assert_eq!(
            SurfaceState::new(100.0).accumulated_mm,
            GROUND_SATURATED,
            "values above saturation must clamp down so a poisoned init can't bypass the cap",
        );
    }

    #[test]
    fn surface_state_new_treats_nan_as_zero() {
        let s = SurfaceState::new(f64::NAN);
        assert_eq!(s.accumulated_mm, 0.0);
        assert!(!s.is_saturated());
    }

    #[test]
    fn surface_state_new_treats_inf_as_zero() {
        assert_eq!(SurfaceState::new(f64::INFINITY).accumulated_mm, 0.0);
        assert_eq!(SurfaceState::new(f64::NEG_INFINITY).accumulated_mm, 0.0);
    }

    #[test]
    fn drying_rate_handles_nan_temperature() {
        let p = DryingParams::default();
        let h = HourlyConditions::minimal(t(0), f64::NAN, 4.0, 0.0);
        let r = drying_rate(&h, &p);
        assert!(r.is_finite(), "NaN temperature must not poison the rate");
        assert!(r >= 0.0);
    }

    #[test]
    fn drying_rate_handles_nan_wind() {
        let p = DryingParams::default();
        let h = HourlyConditions::minimal(t(0), 18.0, f64::NAN, 0.0);
        let r = drying_rate(&h, &p);
        assert!(r.is_finite());
    }

    #[test]
    fn drying_rate_handles_nan_humidity() {
        let p = DryingParams::default();
        let h = HourlyConditions {
            relative_humidity: Some(f64::NAN),
            ..HourlyConditions::minimal(t(0), 18.0, 4.0, 0.0)
        };
        assert!(drying_rate(&h, &p).is_finite());
    }

    #[test]
    fn drying_step_handles_nan_precipitation_without_corruption() {
        let p = DryingParams::default();
        let s = SurfaceState::new(2.0);
        let h = HourlyConditions::minimal(t(0), 18.0, 4.0, f64::NAN);
        let s2 = drying_step(s, &h, &p);
        assert!(
            s2.accumulated_mm.is_finite() && (0.0..=GROUND_SATURATED).contains(&s2.accumulated_mm),
            "NaN precipitation must not poison the accumulator: got {}",
            s2.accumulated_mm
        );
        assert!(s2.hours_since_meaningful_rain.is_finite());
    }

    #[test]
    fn replay_with_nan_in_history_stays_finite() {
        let p = DryingParams::default();
        let mut history: Vec<HourlyConditions> = (0..6).map(dry_hour).collect();
        history.push(HourlyConditions::minimal(
            t(7),
            f64::NAN,
            f64::NAN,
            f64::NAN,
        ));
        history.extend((8..12).map(dry_hour));
        let final_state = replay(SurfaceState::new(1.0), &history, &p);
        assert!(final_state.accumulated_mm.is_finite());
        assert!(final_state.hours_since_meaningful_rain.is_finite());
    }

    // ---- Variant B: cap at input ----

    #[test]
    fn step_excess_rain_is_treated_as_runoff_at_input() {
        // 4 mm bucket + 50 mm rain → capped to GROUND_SATURATED *before*
        // drying applies, so drying actually reduces from the cap. The
        // drought counter resets because the post-drying state is still
        // above SURFACE_WETTED_MM.
        let p = DryingParams::default();
        let s = SurfaceState::new(4.0);
        let h = rainy_hour(0, 50.0);
        let s2 = drying_step(s, &h, &p);
        assert!(s2.accumulated_mm <= GROUND_SATURATED);
        assert!(
            s2.accumulated_mm >= GROUND_SATURATED - 1.0,
            "drying from cap shouldn't lose more than ~1 mm/h; got {}",
            s2.accumulated_mm,
        );
        assert_eq!(s2.hours_since_meaningful_rain, 0.0);
    }

    // ---- Frost gating ----

    #[test]
    fn drying_rate_drops_to_near_zero_when_frozen() {
        let p = DryingParams::default();
        let warm = drying_rate(&HourlyConditions::minimal(t(0), 10.0, 5.0, 0.0), &p);
        let frozen = drying_rate(&HourlyConditions::minimal(t(0), -5.0, 5.0, 0.0), &p);
        assert!(
            frozen < warm * 0.2,
            "frozen rate {frozen} should be much smaller than warm rate {warm}",
        );
        assert!(frozen >= 0.0);
    }

    #[test]
    fn frozen_surface_keeps_water_for_long_replay() {
        // 3 mm of "rain" on a frozen day; 24 hours of frozen drying. The
        // surface should still be wet, since liquid evaporation has stopped.
        let p = DryingParams::default();
        let s = SurfaceState::new(3.0);
        let cold_dry: Vec<HourlyConditions> = (0..24)
            .map(|h| HourlyConditions::minimal(t(h % 24), -5.0, 4.0, 0.0))
            .collect();
        let final_state = replay(s, &cold_dry, &p);
        assert!(
            final_state.accumulated_mm > 1.5,
            "frozen ground should retain most of its 3 mm; got {}",
            final_state.accumulated_mm,
        );
    }

    // ---- UV normalisation ----

    #[test]
    fn sunshine_term_caps_at_15_pct_of_max_when_uv_above_nordic_peak() {
        // UV > UV_NORDIC_PEAK should clamp to 1.5 × sunshine_max — not 2×
        // as in the old `(uv/5).clamp(0,2)` formula.
        let p = DryingParams::default();
        let extreme_sun = HourlyConditions {
            cloud_area_fraction: Some(0.0),
            uv_index_clear_sky: Some(20.0),
            relative_humidity: Some(50.0),
            ..HourlyConditions::minimal(t(0), 5.0, 0.0, 0.0)
        };
        let r = drying_rate(&extreme_sun, &p);
        // base + 0 temp + 0 wind + sunshine_max * 1.0 * 1.5 = 0.05 + 0.075 = 0.125
        assert!(
            (r - 0.125).abs() < 1e-9,
            "expected sunshine cap at 1.5×sunshine_max + base, got {r}",
        );
    }

    // ---- Concave wind ----

    #[test]
    fn wind_term_grows_concave_not_linear() {
        // sqrt: u=4 → 2; u=16 → 4. Old linear: u=4 → 4; u=16 → 16.
        // So doubling the windspeed in the high range no longer doubles
        // the wind contribution.
        let p = DryingParams::default();
        let calm = drying_rate(&HourlyConditions::minimal(t(0), 5.0, 4.0, 0.0), &p);
        let storm = drying_rate(&HourlyConditions::minimal(t(0), 5.0, 16.0, 0.0), &p);
        let calm_wind_term = calm - p.base; // temp_term=0 at 5°C
        let storm_wind_term = storm - p.base;
        // Linear would give ratio 4.0; sqrt gives 2.0.
        let ratio = storm_wind_term / calm_wind_term;
        assert!(
            ratio < 3.0,
            "sqrt wind should give ratio ~2 (got {ratio}); linear would be 4",
        );
    }
}
