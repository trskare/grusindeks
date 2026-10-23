//! A water-balance heuristic for "how wet is the gravel right now?", plus
//! a longer-timescale "how long since meaningful rain" counter for
//! detecting loose/dusty conditions on the dry end.
//!
//! Each simulated hour caps incoming precipitation at `GROUND_SATURATED`
//! (gravel drains overflow into runoff that the model doesn't track),
//! then **drains** the free water above field capacity (gravel sheds water
//! within hours, not days), then subtracts an evaporative drying-rate that
//! depends on temperature, wind, sunshine, and humidity. In parallel we keep
//! `hours_since_meaningful_rain`, which resets whenever the post-drying
//! accumulator clears `SURFACE_WETTED_MM` and otherwise increments by
//! one each step. The scoring layer uses that to flag flerdøgnstørke.
//!
//! Near and below freezing the drying-rate and drainage are scaled by a
//! freeze/thaw ramp (`liquid_fraction`) toward `FROST_FACTOR`, since liquid
//! evaporation and drainage effectively stop and only slow sublimation
//! remains. Snow accumulation/melt is **not** modelled — callers should treat
//! the score as undefined when snow is on the ground.
//!
//! Penman-style combination of net radiation and vapour transport, not
//! reference-crop ET or a calibrated gravel energy-balance model. Radiation,
//! surface albedo and aerodynamic exchange remain estimates; soil heat storage,
//! dew and a slow subsurface water reservoir are not represented.

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

/// Multiplier applied to the per-hour drying rate when the surface is fully
/// frozen. Liquid evaporation effectively stops; only sublimation continues,
/// which is one-to-two orders of magnitude slower. Not zero so a frozen
/// surface doesn't stay "wet forever" in long winter histories.
pub const FROST_FACTOR: f64 = 0.1;

/// Half-width (°C) of the freeze/thaw ramp around 0 °C. Below `-FREEZE_RAMP_C`
/// the surface is treated as fully frozen, above `+FREEZE_RAMP_C` as fully
/// liquid, with a linear blend in between. A hard switch at exactly 0 °C is
/// physically too sharp — real ground freezes and thaws gradually over a
/// degree or so, and surface temperature lags air temperature — so a −0.1 °C
/// hour shouldn't lock up all water while +0.1 °C drains it completely.
pub const FREEZE_RAMP_C: f64 = 1.0;

/// Fraction of the surface water that behaves as liquid (can drain and
/// evaporate freely) at `temp_c`: 0 when fully frozen (≤ `-FREEZE_RAMP_C`),
/// 1 when fully thawed (≥ `+FREEZE_RAMP_C`), linear in between. Used to gate
/// both drainage and the evaporation frost-factor so behaviour is continuous
/// across 0 °C instead of a cliff.
pub fn liquid_fraction(temp_c: f64) -> f64 {
    if !temp_c.is_finite() {
        return 1.0;
    }
    ((temp_c + FREEZE_RAMP_C) / (2.0 * FREEZE_RAMP_C)).clamp(0.0, 1.0)
}

/// Coefficients for the per-hour drying rate (mm/h) and the gravel
/// drainage term. Exposed so tests and future calibration can override
/// them without forking the function.
///
/// **Calibration note — keep these three in sync.** Under sustained rain at
/// intensity `R`, drainage drives the surface to a steady state
/// `s* = surface_retention_mm + R·(1 − drainage_fraction) / drainage_fraction`
/// (a linear-reservoir recession), so the wet-side ground subscore during
/// active rain is set as much by `drainage_fraction` as by `R`. Don't retune
/// `drainage_fraction`, `surface_retention_mm`, or [`GROUND_SATURATED`] in
/// isolation: re-check `score::ground_subscore`'s breakpoints (optimum, wet
/// ramp) afterwards, and prefer `score::active_rain_ground_ceiling` as the
/// authoritative "it's wet because it's raining right now" signal (the
/// equilibrium lags the first hour of rain by design).
#[derive(Debug, Clone, Copy)]
pub struct DryingParams {
    /// Fraction of incoming shortwave reflected by the surface (0–1).
    pub albedo: f64,
    /// Penman-style vapour transport, mm/h per kPa of VPD before weighting
    /// by gamma / (delta + gamma). Wind is converted from 10 m to 2 m.
    pub aerodynamic_mm_per_hour_kpa: f64,
    /// Wind enhancement per m/s at 2 m; multiplies VPD, never dries on its own.
    pub wind_response_per_ms: f64,
    pub max_rate: f64,
    /// Depth of the capillary-held "damp" surface film (mm) the gravel holds
    /// against gravity — what's left once free water has drained. This is a
    /// *grip-tuned surface-film retention depth*, NOT bulk-soil field capacity
    /// (which for gravel would be several mm over a wearing course): it's
    /// aligned with the ground-score's damp optimum so a freshly-drained
    /// surface settles toward "lett fuktig" rather than bone-dry. Water at or
    /// below this level only leaves by evaporation; water *above* it is free
    /// water that drains (see [`DryingParams::drainage_fraction`]).
    pub surface_retention_mm: f64,
    /// Fraction of the *free* water (the part above `surface_retention_mm`)
    /// that drains away each hour. Gravel sheds water fast — without this
    /// the model only lost water to slow evaporation and kept the surface
    /// "wet" for days after the gravel had actually drained dry. See the
    /// calibration note on [`DryingParams`] before changing it.
    pub drainage_fraction: f64,
}

impl Default for DryingParams {
    fn default() -> Self {
        Self {
            albedo: 0.23,
            // Conventional wet-surface Penman transport scale: 2.6 mm/day/kPa.
            // A starting estimate, not locally calibrated gravel coefficients.
            aerodynamic_mm_per_hour_kpa: 2.6 / 24.0,
            wind_response_per_ms: 0.54,
            max_rate: 1.5,
            surface_retention_mm: 0.4,
            drainage_fraction: 0.5,
        }
    }
}

/// How fast a *specific* gravel road sheds water — an opt-in, manually-chosen
/// property of the road surface, not something derivable from MET/weather
/// data (which carries no soil, slope or grain-size information). Lets a rider
/// who knows their local roads pick a drainage character; the default
/// [`DrainageClass::Normal`] reproduces the calibrated population-average
/// model, so nothing changes unless a road is explicitly classified.
///
/// Drainage time across real gravel roads spans more than an order of
/// magnitude (grain-size / fines content, crown & grade, compaction, base
/// permeability), so this is the right axis to expose — but only the rider
/// can supply it. See the team review's drainage section.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DrainageClass {
    /// Coarse, well-crowned, free-draining road (good base, real grade) —
    /// sheds standing water within roughly an hour.
    Rask,
    /// A typical, reasonably-built gravel road. The model's calibrated
    /// default; identical to [`DryingParams::default`].
    #[default]
    Normal,
    /// Fine-grained / compacted / flat / clay-bound surface that holds water
    /// for many hours after rain (wheel ruts, poor drainage).
    Treg,
}

impl DrainageClass {
    /// The drying coefficients for this road class. Only the gravel-drainage
    /// terms differ; the evaporation coefficients (shared physics) stay at the
    /// default. `Normal` returns exactly [`DryingParams::default`].
    pub fn drying_params(self) -> DryingParams {
        let base = DryingParams::default();
        match self {
            DrainageClass::Rask => DryingParams {
                drainage_fraction: 0.8,
                surface_retention_mm: 0.25,
                ..base
            },
            DrainageClass::Normal => base,
            DrainageClass::Treg => DryingParams {
                drainage_fraction: 0.2,
                surface_retention_mm: 0.9,
                ..base
            },
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

/// Saturation vapour pressure (kPa), Magnus approximation. The caller bounds
/// temperature to the weather domain to avoid the singularity at −243.04 °C.
fn saturation_vapour_kpa(temp_c: f64) -> f64 {
    0.610_94 * (17.625 * temp_c / (temp_c + 243.04)).exp()
}

/// Potential evaporation (mm/h) from a wet surface.
///
/// A Penman-style combination weights net radiation by delta/(delta+gamma)
/// and vapour transport by gamma/(delta+gamma). VPD is es(T)*(1−RH/100):
/// neither temperature nor wind supplies an unconditional drying bonus.
///
/// Incoming shortwave is the shared hourly solar estimate, NOT UV. Longwave
/// cooling uses the FAO-56 vapour-emissivity factor with a cloud-cover estimate
/// of sky transmissivity. This hourly adaptation assumes surface temperature
/// equals air temperature and neglects ground heat storage. Negative radiation
/// offsets aerodynamic drying *before* the final zero clamp: a cold clear night
/// cannot turn radiative cooling into energy for evaporation. Dry advected air
/// can still evaporate water at night. Negative totals do not add dew.
///
/// Missing radiation adds no solar energy; missing RH/cloud use the historical
/// replay's 70% fallback, not dry air or clear sky. These estimates are not a
/// substitute for observations. `drying_step` limits actual loss to stored water.
pub fn drying_rate(h: &HourlyConditions, p: &DryingParams) -> f64 {
    let temp_c = finite_or(h.temperature_c, 0.0).clamp(-80.0, 60.0);
    let wind_10m = finite_or(h.wind_speed_ms, 0.0).max(0.0);
    let rh = finite_or(h.relative_humidity.unwrap_or(70.0), 70.0).clamp(0.0, 100.0);
    let cloud = finite_or(h.cloud_area_fraction.unwrap_or(70.0), 70.0).clamp(0.0, 100.0) / 100.0;
    let shortwave = finite_or(h.shortwave_radiation_w_m2.unwrap_or(0.0), 0.0).max(0.0);

    let es = saturation_vapour_kpa(temp_c);
    let actual_vapour = es * rh / 100.0;
    let vpd = es - actual_vapour;
    let delta = 17.625 * 243.04 * es / (temp_c + 243.04).powi(2);
    // kPa/°C, near sea level. No pressure/elevation observation in this model.
    const GAMMA: f64 = 0.0665;
    const STEFAN_BOLTZMANN: f64 = 5.670_374_419e-8;
    const LATENT_HEAT_J_KG: f64 = 2_450_000.0;
    // FAO-56 logarithmic wind-height adjustment: 4.87 / ln(67.8*10 − 5.42).
    const WIND_10M_TO_2M: f64 = 0.747_951_075_167_944_1;

    let sky_transmissivity = 1.0 - 0.75 * cloud.powf(3.4);
    let cloud_emissivity = (1.35 * sky_transmissivity - 0.35).clamp(0.05, 1.0);
    let longwave_loss = STEFAN_BOLTZMANN
        * (temp_c + 273.15).powi(4)
        * (0.34 - 0.14 * actual_vapour.sqrt()).max(0.0)
        * cloud_emissivity;
    let net_radiation = (1.0 - p.albedo) * shortwave - longwave_loss;
    // W/m² × seconds / (J/kg) = kg/m² = mm of water per hourly step.
    let energy_mm = net_radiation * 3600.0 / LATENT_HEAT_J_KG;
    let aerodynamic_mm = p.aerodynamic_mm_per_hour_kpa
        * (1.0 + p.wind_response_per_ms * wind_10m * WIND_10M_TO_2M)
        * vpd;
    let potential = (delta * energy_mm + GAMMA * aerodynamic_mm) / (delta + GAMMA);
    let frost_factor = FROST_FACTOR + (1.0 - FROST_FACTOR) * liquid_fraction(temp_c);
    (potential * frost_factor).clamp(0.0, p.max_rate)
}

/// Step the surface state forward by one hour.
///
/// Order: cap incoming `accumulated + precip` at `GROUND_SATURATED`
/// (overflow is treated as runoff that the model doesn't track), then
/// **drain** free water, then subtract evaporative drying. Capping at the
/// input — not after drying — makes the "vann forsvinner ut av
/// regnskapet"-semantikk eksplisitt and avoids having two scenarios with
/// very different physical meaning (a 50 mm deluge vs 5 hours of light
/// rain) collapse to identical post-drying states.
///
/// Infiltration capacity is assumed effectively infinite: all rain enters the
/// surface bucket within the hour and only the amount past `GROUND_SATURATED`
/// runs off. Gravel's infiltration rate vastly exceeds typical rainfall, so we
/// don't separately model intense rain overwhelming infiltration and ponding;
/// the active-rain ground-score ceiling captures "it's pouring now" instead.
///
/// Drainage models the fact that gravel sheds water fast: the part of the
/// surface water above `surface_retention_mm` is *free* water that infiltrates
/// / runs off, and `drainage_fraction` of it leaves each hour. Water at or
/// below field capacity is capillary-held "damp" that only evaporation can
/// remove. Drainage is gated by frost the same way evaporation is — scaled by
/// the [`liquid_fraction`] freeze/thaw ramp, so a fully frozen surface
/// neither drains nor evaporates appreciably (only the slow sublimation in
/// `drying_rate`), and the transition across 0 °C is gradual, not a cliff.
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
    // Drain free water (above field capacity). Frozen surfaces don't drain —
    // the water is locked up as ice — so scale drainage by the same
    // freeze/thaw `liquid_fraction` ramp that gates evaporation, rather than a
    // hard cut at 0 °C.
    let temp_c = finite_or(h.temperature_c, 0.0);
    let free = (after_rain - p.surface_retention_mm).max(0.0);
    let drained = free * p.drainage_fraction.clamp(0.0, 1.0) * liquid_fraction(temp_c);
    let after_drainage = after_rain - drained;
    let evaporated = drying_rate(h, p).min(after_drainage);
    let after_drying = after_drainage - evaporated;
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
    use crate::sun::shortwave_radiation_w_m2;
    use chrono::{TimeZone, Utc};

    fn t(h: u32) -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 4, 26, h, 0, 0).unwrap()
    }

    fn dry_hour(h: u32) -> HourlyConditions {
        HourlyConditions {
            thunder: false,
            cloud_area_fraction: Some(20.0),
            shortwave_radiation_w_m2: Some(shortwave_radiation_w_m2(
                t(h),
                59.91,
                10.75,
                Some(20.0),
            )),
            relative_humidity: Some(40.0),
            ..HourlyConditions::minimal(t(h), 18.0, 4.0, 0.0)
        }
    }

    fn rainy_hour(h: u32, mm: f64) -> HourlyConditions {
        HourlyConditions {
            thunder: false,
            cloud_area_fraction: Some(95.0),
            shortwave_radiation_w_m2: Some(0.0),
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
    fn saturated_air_without_solar_energy_cannot_evaporate_retained_water() {
        let p = DryingParams::default();
        for temp in [2.0, 5.0, 15.0] {
            for wind in [0.0, 3.0, 12.0] {
                let h = HourlyConditions {
                    relative_humidity: Some(100.0),
                    cloud_area_fraction: Some(100.0),
                    shortwave_radiation_w_m2: Some(0.0),
                    ..HourlyConditions::minimal(t(0), temp, wind, 0.0)
                };
                let mut state = SurfaceState::new(0.4);
                for _ in 0..24 {
                    state = drying_step(state, &h, &p);
                }
                assert_eq!(state.accumulated_mm, 0.4, "T={temp}, wind={wind}");
                assert_eq!(state.hours_since_meaningful_rain, 0.0);
            }
        }
    }

    #[test]
    fn cold_humid_air_dries_slower_than_mild_air_at_equal_rh_and_wind() {
        let p = DryingParams::default();
        let cold = HourlyConditions {
            relative_humidity: Some(95.0),
            cloud_area_fraction: Some(100.0),
            shortwave_radiation_w_m2: Some(0.0),
            ..HourlyConditions::minimal(t(0), 2.0, 3.0, 0.0)
        };
        let mild = HourlyConditions {
            temperature_c: 15.0,
            ..cold.clone()
        };
        assert!(drying_rate(&cold, &p) < drying_rate(&mild, &p));
        let mut state = SurfaceState::new(0.4);
        for _ in 0..24 {
            state = drying_step(state, &cold, &p);
        }
        assert!(
            state.accumulated_mm >= SURFACE_WETTED_MM,
            "one cold humid night/day must not turn retained water into drought"
        );
    }

    #[test]
    fn radiative_cooling_offsets_but_does_not_forbid_night_evaporation() {
        let p = DryingParams::default();
        let clear = HourlyConditions {
            relative_humidity: Some(30.0),
            cloud_area_fraction: Some(0.0),
            shortwave_radiation_w_m2: Some(0.0),
            ..HourlyConditions::minimal(t(0), 20.0, 8.0, 0.0)
        };
        let overcast = HourlyConditions {
            cloud_area_fraction: Some(100.0),
            ..clear.clone()
        };
        let clear_rate = drying_rate(&clear, &p);
        assert!(clear_rate > 0.0, "dry advected air can dry at night");
        assert!(
            clear_rate < drying_rate(&overcast, &p),
            "clear-night cooling must offset, not increase, evaporation"
        );
    }

    #[test]
    fn solar_energy_can_evaporate_water_in_saturated_air_but_not_create_negative_water() {
        let p = DryingParams::default();
        let h = HourlyConditions {
            relative_humidity: Some(100.0),
            cloud_area_fraction: Some(0.0),
            shortwave_radiation_w_m2: Some(500.0),
            ..HourlyConditions::minimal(t(12), 10.0, 0.0, 0.0)
        };
        assert!(drying_rate(&h, &p) > 0.1, "sun can heat a wet surface");
        assert_eq!(
            drying_step(SurfaceState::new(0.01), &h, &p).accumulated_mm,
            0.0
        );
        let dark = HourlyConditions {
            shortwave_radiation_w_m2: None,
            ..h
        };
        assert_eq!(
            drying_rate(&dark, &p),
            0.0,
            "clear skies without radiation data must not invent sunlight"
        );
    }

    #[test]
    fn autumn_solar_cycle_retains_water_longer_than_summer_at_identical_weather() {
        // Isolate evaporation from the unchanged fast drainage, so both
        // scenarios retain water instead of hitting the zero-water floor.
        let p = DryingParams {
            drainage_fraction: 0.0,
            ..DryingParams::default()
        };
        let simulate = |month, day| {
            let start = Utc.with_ymd_and_hms(2026, month, day, 0, 0, 0).unwrap();
            let mut state = SurfaceState::new(5.0);
            let mut potential = 0.0;
            for hour in 0..24 {
                let time = start + chrono::Duration::hours(hour);
                let h = HourlyConditions {
                    relative_humidity: Some(90.0),
                    cloud_area_fraction: Some(70.0),
                    shortwave_radiation_w_m2: Some(shortwave_radiation_w_m2(
                        time,
                        59.91,
                        10.75,
                        Some(70.0),
                    )),
                    ..HourlyConditions::minimal(time, 10.0, 3.0, 0.0)
                };
                potential += drying_rate(&h, &p);
                state = drying_step(state, &h, &p);
            }
            (potential, state.accumulated_mm)
        };
        let summer = simulate(6, 21);
        let autumn = simulate(10, 24);
        assert!(
            summer.0 > autumn.0 * 2.0,
            "solar season must materially affect drying"
        );
        assert!(
            autumn.1 > summer.1,
            "short autumn days must retain more water"
        );
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
            thunder: false,
            relative_humidity: Some(40.0),
            ..HourlyConditions::minimal(t(0), 18.0, 4.0, 0.0)
        };
        let humid = HourlyConditions {
            thunder: false,
            relative_humidity: Some(95.0),
            ..HourlyConditions::minimal(t(0), 18.0, 4.0, 0.0)
        };
        assert!(drying_rate(&base, &p) > drying_rate(&humid, &p));
    }

    #[test]
    fn rate_clamped_to_max() {
        let p = DryingParams::default();
        let extreme = HourlyConditions {
            thunder: false,
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
            thunder: false,
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
        // Rainy conditions push evaporation near zero (humid, no sun) — what
        // we care about is that the precipitation makes it into the bucket.
        // Drainage sheds part of the free water within the hour, so 2 mm of
        // rain leaves less than 2 mm standing, but clearly more than the
        // field-capacity floor.
        let p = DryingParams::default();
        let s = SurfaceState::new(0.0);
        let h = rainy_hour(0, 2.0);
        let s2 = drying_step(s, &h, &p);
        assert!(
            s2.accumulated_mm > p.surface_retention_mm,
            "rain should add water above field capacity; got {}",
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
    fn replay_continuous_rain_yields_wet_surface() {
        // With gravel drainage, continuous heavy rain no longer ponds all
        // the way to the saturation cap — free water drains each hour and
        // the surface settles at a wet equilibrium well above field
        // capacity. That's the physically honest result: the gravel is
        // soaked, but it isn't holding 5 mm of standing water. The
        // active-rain ceiling in the scorer handles "it's pouring right
        // now" separately.
        let p = DryingParams::default();
        let history: Vec<_> = (0..24).map(|h| rainy_hour(h % 24, 3.0)).collect();
        let final_state = replay(SurfaceState::new(0.0), &history, &p);
        assert!(
            final_state.accumulated_mm > 2.0,
            "continuous 3 mm/h rain should leave the surface clearly wet; got {}",
            final_state.accumulated_mm,
        );
        // And the ground subscore for that wetness should be poor — well
        // below the 80-point "no penalty" line.
        assert!(crate::score::ground_subscore(final_state.accumulated_mm) < 60);
        // Drainage must be load-bearing: with it off the surface would pin to
        // the saturation cap. This makes the test fail if drainage regresses.
        let no_drainage = replay(
            SurfaceState::new(0.0),
            &history,
            &DryingParams {
                drainage_fraction: 0.0,
                ..p
            },
        );
        assert!(
            final_state.accumulated_mm < no_drainage.accumulated_mm - 1.0,
            "drainage should hold the equilibrium clearly below the no-drainage cap; \
             got drained={}, no_drainage={}",
            final_state.accumulated_mm,
            no_drainage.accumulated_mm,
        );
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
        // 24 h of solid rain (wets the surface), then 100 h of warm dry
        // hours. With drainage the surface clears SURFACE_WETTED_MM within
        // a few hours of the rain stopping, so the counter resets for only
        // that short tail and then climbs each hour — landing near (but
        // below) the full 100 h. Allow a wide band: the exact value depends
        // on the drainage + drying coefficients, which we don't want this
        // test to lock down.
        let p = DryingParams::default();
        let mut history: Vec<HourlyConditions> = (0..24).map(|h| rainy_hour(h, 2.0)).collect();
        history.extend((24..124).map(|h| dry_hour(h % 24)));
        let final_state = replay(SurfaceState::default(), &history, &p);
        assert!(
            (60.0..=100.0).contains(&final_state.hours_since_meaningful_rain),
            "got {}",
            final_state.hours_since_meaningful_rain,
        );
        // Drainage-sensitive: a slower-draining surface stays wet longer after
        // the rain, so its drought counter must end up strictly lower. Guards
        // against drainage silently regressing to "barely drains".
        let slow = replay(
            SurfaceState::default(),
            &history,
            &DryingParams {
                drainage_fraction: 0.1,
                ..p
            },
        );
        assert!(
            slow.hours_since_meaningful_rain < final_state.hours_since_meaningful_rain,
            "slower drainage should clear later (lower counter); got slow={}, default={}",
            slow.hours_since_meaningful_rain,
            final_state.hours_since_meaningful_rain,
        );
    }

    // ---- NaN sanitisation ----

    #[test]
    fn nan_temperature_behaves_as_zero_celsius_in_rate_and_step() {
        // Defensive posture: a NaN temperature reaching the drying maths is
        // sanitised to 0 °C (mid freeze-ramp) — NOT the replay layer's 10 °C
        // fallback. In practice the replay substitutes 10 °C for a missing
        // observation before drying ever sees it, so this only governs the
        // last-ditch guard; pin it so the two layers' postures stay documented.
        let p = DryingParams::default();
        let nan_rate = drying_rate(&HourlyConditions::minimal(t(0), f64::NAN, 4.0, 0.0), &p);
        let zero_rate = drying_rate(&HourlyConditions::minimal(t(0), 0.0, 4.0, 0.0), &p);
        assert!(
            (nan_rate - zero_rate).abs() < 1e-12,
            "NaN temp should behave as 0 °C in drying_rate: {nan_rate} vs {zero_rate}",
        );
        let step = |temp| {
            drying_step(
                SurfaceState::new(GROUND_SATURATED),
                &HourlyConditions::minimal(t(0), temp, 2.0, 0.0),
                &p,
            )
            .accumulated_mm
        };
        assert!(
            (step(f64::NAN) - step(0.0)).abs() < 1e-12,
            "NaN-temp drainage should match 0 °C",
        );
    }

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
            thunder: false,
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
    fn step_excess_rain_runs_off_at_input_then_drains() {
        // 4 mm bucket + 50 mm rain → capped to GROUND_SATURATED *before*
        // anything else (overflow is runoff the model doesn't track). Then
        // drainage sheds the free water above field capacity within the
        // hour, so the post-step surface sits well below the cap — but the
        // surface is still soaked, so the drought counter resets.
        let p = DryingParams::default();
        let s = SurfaceState::new(4.0);
        let h = rainy_hour(0, 50.0);
        let s2 = drying_step(s, &h, &p);
        assert!(s2.accumulated_mm <= GROUND_SATURATED);
        // From a 5 mm cap, one hour of drainage removes drainage_fraction of
        // the free water (5 − 0.4 = 4.6 mm), leaving ≈ 2.7 mm.
        let expected =
            GROUND_SATURATED - (GROUND_SATURATED - p.surface_retention_mm) * p.drainage_fraction;
        assert!(
            (s2.accumulated_mm - expected).abs() < 0.2,
            "expected ≈{expected:.2} mm after one hour of drainage from the cap; got {}",
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

    // ---- Controlled weather for drainage tests ----

    /// Build an hour fixing every drying input except the one under test,
    /// so a single-variable sweep is unambiguous.
    fn hour_with(
        temp: f64,
        wind: f64,
        humidity: f64,
        cloud: Option<f64>,
        shortwave: Option<f64>,
        precip: f64,
    ) -> HourlyConditions {
        HourlyConditions {
            thunder: false,
            cloud_area_fraction: cloud,
            shortwave_radiation_w_m2: shortwave,
            relative_humidity: Some(humidity),
            ..HourlyConditions::minimal(t(0), temp, wind, precip)
        }
    }

    // ---- Gravel drainage ----

    #[test]
    fn saturated_surface_drains_within_hours() {
        // A saturated surface on a cool, humid, calm day (evaporation ≈ 0)
        // must still shed most of its water within a handful of hours — that
        // shedding is drainage, not evaporation. Before the drainage term,
        // this surface would have stayed near-saturated for days.
        let p = DryingParams::default();
        // Cool/humid/calm → drying_rate clamps to ~0, isolating drainage.
        let damp_cool = hour_with(6.0, 1.0, 95.0, Some(95.0), None, 0.0);
        let mut s = SurfaceState::new(GROUND_SATURATED);
        for _ in 0..6 {
            s = drying_step(s, &damp_cool, &p);
        }
        assert!(
            s.accumulated_mm < 1.0,
            "6 h of drainage should clear a saturated surface to < 1 mm even with ~zero evaporation; got {}",
            s.accumulated_mm,
        );
    }

    #[test]
    fn drainage_does_not_touch_water_at_or_below_field_capacity() {
        // Water at field capacity is capillary-held "damp" — drainage leaves
        // it alone, so it can only fall via evaporation. With evaporation
        // pinned to ~0 (cool/humid/calm) the damp level must be preserved.
        let p = DryingParams::default();
        let damp_cool = hour_with(6.0, 1.0, 95.0, Some(95.0), None, 0.0);
        let s = SurfaceState::new(p.surface_retention_mm);
        let s2 = drying_step(s, &damp_cool, &p);
        assert!(
            (s2.accumulated_mm - p.surface_retention_mm).abs() < 1e-9,
            "field-capacity water must not drain; got {}",
            s2.accumulated_mm,
        );
    }

    #[test]
    fn frozen_surface_does_not_drain() {
        // Water locked up as ice neither evaporates nor drains. A saturated,
        // frozen surface must hold its water across many hours.
        let p = DryingParams::default();
        let frozen = hour_with(-5.0, 4.0, 80.0, Some(50.0), None, 0.0);
        let mut s = SurfaceState::new(GROUND_SATURATED);
        for _ in 0..12 {
            s = drying_step(s, &frozen, &p);
        }
        assert!(
            s.accumulated_mm > GROUND_SATURATED - 0.5,
            "frozen surface should retain ~all of its water (no drainage); got {}",
            s.accumulated_mm,
        );
    }

    #[test]
    fn drainage_makes_recovery_much_faster_than_evaporation_alone() {
        // Same wet surface and same weather, with vs without drainage. The
        // drained surface must recover dramatically faster — this is the
        // whole point of the fix.
        let with_drainage = DryingParams::default();
        let no_drainage = DryingParams {
            drainage_fraction: 0.0,
            ..DryingParams::default()
        };
        // A mild, partly-cloudy day after the rain.
        let day: Vec<HourlyConditions> = (0..12)
            .map(|h| HourlyConditions {
                thunder: false,
                cloud_area_fraction: Some(40.0),
                uv_index_clear_sky: None,
                relative_humidity: Some(65.0),
                ..HourlyConditions::minimal(t(h % 24), 14.0, 3.0, 0.0)
            })
            .collect();
        let drained = replay(SurfaceState::new(GROUND_SATURATED), &day, &with_drainage);
        let evap_only = replay(SurfaceState::new(GROUND_SATURATED), &day, &no_drainage);
        assert!(
            drained.accumulated_mm < evap_only.accumulated_mm,
            "drainage should recover faster (drained={}, evap_only={})",
            drained.accumulated_mm,
            evap_only.accumulated_mm,
        );
        assert!(
            drained.accumulated_mm < 0.5,
            "with drainage the surface should be near-dry after 12 h; got {}",
            drained.accumulated_mm,
        );
        assert!(
            evap_only.accumulated_mm > 1.0,
            "evaporation alone should still leave the surface wet after 12 h; got {}",
            evap_only.accumulated_mm,
        );
    }

    // ---- DrainageClass ----

    #[test]
    fn drainage_class_orders_water_retention() {
        // Same saturated start + same mild dry day: a faster-draining road
        // class must hold less residual water than a slower one.
        let day: Vec<HourlyConditions> = (0..6)
            .map(|h| HourlyConditions {
                thunder: false,
                cloud_area_fraction: Some(50.0),
                uv_index_clear_sky: None,
                relative_humidity: Some(70.0),
                ..HourlyConditions::minimal(t(h % 24), 12.0, 2.0, 0.0)
            })
            .collect();
        let run = |c: DrainageClass| {
            replay(
                SurfaceState::new(GROUND_SATURATED),
                &day,
                &c.drying_params(),
            )
            .accumulated_mm
        };
        let rask = run(DrainageClass::Rask);
        let normal = run(DrainageClass::Normal);
        let treg = run(DrainageClass::Treg);
        assert!(
            rask < normal,
            "rask ({rask}) should hold less than normal ({normal})"
        );
        assert!(
            normal < treg,
            "normal ({normal}) should hold less than treg ({treg})"
        );
    }

    #[test]
    fn drainage_class_normal_equals_default() {
        let n = DrainageClass::Normal.drying_params();
        let d = DryingParams::default();
        assert_eq!(n.drainage_fraction, d.drainage_fraction);
        assert_eq!(n.surface_retention_mm, d.surface_retention_mm);
    }

    #[test]
    fn drainage_class_default_is_normal() {
        assert_eq!(DrainageClass::default(), DrainageClass::Normal);
    }

    // ---- Freeze/thaw ramp (B3 + D4 boundaries) ----

    #[test]
    fn liquid_fraction_ramps_linearly_across_zero() {
        assert_eq!(liquid_fraction(-FREEZE_RAMP_C), 0.0);
        assert_eq!(liquid_fraction(0.0), 0.5);
        assert_eq!(liquid_fraction(FREEZE_RAMP_C), 1.0);
        assert_eq!(
            liquid_fraction(-5.0),
            0.0,
            "well below freezing → fully frozen"
        );
        assert_eq!(
            liquid_fraction(15.0),
            1.0,
            "well above freezing → fully liquid"
        );
        assert_eq!(
            liquid_fraction(f64::NAN),
            1.0,
            "NaN treated as liquid (neutral)"
        );
    }

    #[test]
    fn frost_gate_is_continuous_and_monotone_across_zero() {
        let p = DryingParams::default();
        let r = |t_c| {
            drying_rate(
                &HourlyConditions {
                    shortwave_radiation_w_m2: Some(200.0),
                    ..HourlyConditions::minimal(t(12), t_c, 4.0, 0.0)
                },
                &p,
            )
        };
        // No cliff at 0 °C — the old hard switch jumped 10× here.
        assert!(
            (r(-0.001) - r(0.001)).abs() < 1e-3,
            "expected continuity at 0 °C, got {} vs {}",
            r(-0.001),
            r(0.001),
        );
        // Strictly increasing through the ramp.
        assert!(r(-1.0) < r(0.0) && r(0.0) < r(1.0), "ramp must be monotone");
    }

    #[test]
    fn drainage_is_continuous_and_monotone_across_zero() {
        let p = DryingParams::default();
        let retained = |t_c| {
            drying_step(
                SurfaceState::new(GROUND_SATURATED),
                &HourlyConditions::minimal(t(0), t_c, 2.0, 0.0),
                &p,
            )
            .accumulated_mm
        };
        // Continuous across 0 °C (no drainage cliff).
        assert!(
            (retained(-0.05) - retained(0.05)).abs() < 0.2,
            "drainage should be continuous near 0 °C: {} vs {}",
            retained(-0.05),
            retained(0.05),
        );
        // Colder → retains more water (less drainage).
        assert!(retained(-2.0) > retained(0.0));
        assert!(retained(0.0) > retained(2.0));
    }

    #[test]
    fn freeze_thaw_replay_stays_finite_and_bounded() {
        let p = DryingParams::default();
        // 48 h alternating −2/+2 °C, dry: must settle to a finite, in-range
        // value (no oscillating blow-up, no NaN).
        let hist: Vec<HourlyConditions> = (0..48)
            .map(|h| {
                let temp = if h % 2 == 0 { -2.0 } else { 2.0 };
                HourlyConditions::minimal(t(h % 24), temp, 3.0, 0.0)
            })
            .collect();
        let s = replay(SurfaceState::new(3.0), &hist, &p);
        assert!(s.accumulated_mm.is_finite());
        assert!((0.0..=GROUND_SATURATED).contains(&s.accumulated_mm));
    }

    /// A cold, saturated-humid, calm, sunless hour drives evaporation to its
    /// clamp floor (0) and — being frozen — drainage to 0, so the surface
    /// neither gains nor loses water. Lets us pin exact threshold behaviour.
    fn no_op_hour(temp_c: f64, precip: f64) -> HourlyConditions {
        HourlyConditions {
            thunder: false,
            cloud_area_fraction: Some(100.0),
            uv_index_clear_sky: Some(0.0),
            relative_humidity: Some(100.0),
            ..HourlyConditions::minimal(t(0), temp_c, 0.0, precip)
        }
    }

    #[test]
    fn drought_counter_resets_at_exactly_surface_wetted_mm() {
        let p = DryingParams::default();
        let s = SurfaceState {
            accumulated_mm: SURFACE_WETTED_MM,
            hours_since_meaningful_rain: 10.0,
            drought_at_lookback_cap: false,
        };
        // Frozen + humid: no evaporation, and the water is below field
        // capacity so no drainage → after_drying == SURFACE_WETTED_MM exactly.
        let s2 = drying_step(s, &no_op_hour(-5.0, 0.0), &p);
        assert!(
            (s2.accumulated_mm - SURFACE_WETTED_MM).abs() < 1e-9,
            "expected the surface to hold at SURFACE_WETTED_MM, got {}",
            s2.accumulated_mm,
        );
        assert_eq!(
            s2.hours_since_meaningful_rain, 0.0,
            "exactly SURFACE_WETTED_MM must count as wet (>= boundary)",
        );
    }

    #[test]
    fn rain_caps_at_exactly_ground_saturated() {
        let p = DryingParams::default();
        // Frozen + humid + heavy rain: input caps at GROUND_SATURATED and
        // neither drains nor evaporates → lands exactly on the cap.
        let s2 = drying_step(SurfaceState::new(4.0), &no_op_hour(-5.0, 100.0), &p);
        assert_eq!(s2.accumulated_mm, GROUND_SATURATED);
    }
}
