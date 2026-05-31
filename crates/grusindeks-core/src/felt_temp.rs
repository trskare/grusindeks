//! Apparent temperature ("felt-T") for cyclists.
//!
//! Combines air temperature with wind (cold side: wind chill), humidity
//! (warm side: heat index) and solar radiation (sun warms the rider on
//! clear days) to produce the temperature a cyclist actually feels. The
//! cold-side formula is the NWS / Environment Canada wind chill (2001
//! JAG/TI revision); the warm-side is the NWS Rothfusz heat index
//! regression. Solar warming is added on top of either base, scaled by
//! cloud cover, UV index, and wind (which strips solar heat off the skin
//! via forced convection).
//!
//! Wind chill is "always on" for a moving cyclist because they generate
//! their own relative airflow from forward speed. We bake that into
//! `cyclist_effective_wind_ms` before feeding the chill formula, so a
//! still 0 °C day still feels like ~−4 °C — matching what the rider
//! experiences on the bike.

/// Cyclist self-generated wind at gravel cruising speed (≈ 18 km/h). Used
/// to floor the wind input to wind chill so a "calm" cold day still
/// reflects the relative airflow over the rider.
pub const CYCLING_WIND_MS: f64 = 5.0;

/// Above this air temp, wind chill no longer applies — switch to heat
/// index (or pass-through, if humidity is missing).
pub const WIND_CHILL_MAX_T_C: f64 = 10.0;

/// Inclusive lower bound for the heat-index branch (≈ 80 °F, the NWS
/// Rothfusz calibration threshold). Below this, the regression
/// extrapolates artificially upward at low RH, so we pass T through.
pub const HEAT_INDEX_MIN_T_C: f64 = 27.0;

/// NWS regression is unreliable below ≈ 4.8 km/h (1.33 m/s); below this
/// the formula returns warmer-than-air values, so we pass through.
const WIND_CHILL_MIN_KMH: f64 = 4.8;

/// UV index value that represents the upper end of typical Nordic peak
/// summer noon irradiance. Used to normalise `uv_index_clear_sky` into a
/// 0–1.5 sunshine multiplier. Shared with the drying model so both
/// signals scale consistently.
pub const UV_NORDIC_PEAK: f64 = 7.0;

/// Maximum °C added to felt-T from direct sunshine, hit at clear sky +
/// UV ≈ Nordic peak + still air. Calibrated against UTCI / MRT studies
/// where mean radiant temperature on bright days runs 20–30 °C above
/// air temp; the 3.5 °C felt-T equivalent is conservative because (a)
/// cyclists are partially clothed and moving, and (b) the temperature
/// sub-score curve already amplifies any felt-T shift toward whichever
/// edge of the comfort plateau the rider sits closest to.
pub const SOLAR_MAX_C: f64 = 3.5;

/// Wind attenuation slope in `1 / (1 + k · v)`. At 10 m/s relative wind
/// the rider feels roughly half the still-air sun warmth — forced
/// convection strips solar heat off the skin faster than radiation
/// deposits it.
const SOLAR_WIND_K: f64 = 0.1;

/// Threshold below which a solar contribution is treated as 0 — a sliver
/// of warmth from a nearly-overcast sky is noise, not signal, and
/// renderers can use this to suppress "Sol bidrar med +0.1 °C" chatter.
pub const SOLAR_NEGLIGIBLE_C: f64 = 0.5;

/// Wind chill in °C using the NWS / Environment Canada 2001 formula:
/// `Twc = 13.12 + 0.6215·T − 11.37·v^0.16 + 0.3965·T·v^0.16`
/// with T in °C and v in km/h at 10 m height. Returns the input air temp
/// unchanged when inputs fall outside the formula's calibrated band
/// (T > 10 °C, v < 4.8 km/h, or non-finite values), since extrapolation
/// produces nonsensical "felt warmer than air" results.
pub fn wind_chill_c(t_c: f64, wind_ms: f64) -> f64 {
    if !t_c.is_finite() || !wind_ms.is_finite() {
        return t_c;
    }
    if t_c > WIND_CHILL_MAX_T_C {
        return t_c;
    }
    let v_kmh = wind_ms.max(0.0) * 3.6;
    if v_kmh < WIND_CHILL_MIN_KMH {
        return t_c;
    }
    let v_pow = v_kmh.powf(0.16);
    13.12 + 0.6215 * t_c - 11.37 * v_pow + 0.3965 * t_c * v_pow
}

/// Effective wind for cyclist wind chill: the relative airflow including
/// rider self-generated wind from forward motion. Combines ambient and
/// cycling speed in quadrature, since the two vectors are roughly
/// uncorrelated over a typical ride.
pub fn cyclist_effective_wind_ms(ambient_ms: f64) -> f64 {
    if !ambient_ms.is_finite() {
        return CYCLING_WIND_MS;
    }
    let a = ambient_ms.max(0.0);
    (a * a + CYCLING_WIND_MS * CYCLING_WIND_MS).sqrt()
}

/// Heat index in °C using the NWS Rothfusz regression. Inputs are T (°C)
/// and RH (%, 0–100). The regression is calibrated for T ≥ 27 °C and
/// RH ≥ 40 %; outside that band it can dip below the input T, so we cap
/// the result at `t_c` to keep "felt-T ≥ T" on warm-but-dry days.
pub fn heat_index_c(t_c: f64, rh_pct: f64) -> f64 {
    if !t_c.is_finite() || !rh_pct.is_finite() {
        return t_c;
    }
    if t_c < HEAT_INDEX_MIN_T_C {
        return t_c;
    }
    let rh = rh_pct.clamp(0.0, 100.0);
    let t_f = t_c * 1.8 + 32.0;
    let t2 = t_f * t_f;
    let rh2 = rh * rh;
    let hi_f = -42.379 + 2.049_015_23 * t_f + 10.143_331_27 * rh
        - 0.224_755_41 * t_f * rh
        - 6.837_83e-3 * t2
        - 5.481_717e-2 * rh2
        + 1.228_74e-3 * t2 * rh
        + 8.528_2e-4 * t_f * rh2
        - 1.99e-6 * t2 * rh2;
    let hi_c = (hi_f - 32.0) / 1.8;
    hi_c.max(t_c)
}

/// Felt-T routing: wind chill on the cold side (≤ 10 °C), heat index on
/// the warm side (≥ 20 °C and humidity available), pass-through in
/// between. `humidity_pct` is optional because long-range MET forecasts
/// don't always carry it; we fall back to the air temperature instead of
/// punishing the user for missing data.
pub fn apparent_temp(t_c: f64, wind_ms: f64, humidity_pct: Option<f64>) -> f64 {
    if !t_c.is_finite() {
        return t_c;
    }
    if t_c <= WIND_CHILL_MAX_T_C {
        let effective = cyclist_effective_wind_ms(wind_ms);
        return wind_chill_c(t_c, effective);
    }
    if t_c >= HEAT_INDEX_MIN_T_C {
        if let Some(rh) = humidity_pct {
            if rh.is_finite() {
                return heat_index_c(t_c, rh);
            }
        }
    }
    t_c
}

/// Solar warming added to felt-T for an outdoor cyclist, in °C. Always
/// non-negative — the sun never *cools* the rider; whether that warmth
/// helps or hurts the score is decided by the temperature sub-score
/// curve (good in cold, bad in heat).
///
/// Returns 0 when either input is missing or non-finite, mirroring the
/// drying model's defensive handling. Inputs:
/// * `cloud_pct` — total cloud area, 0–100 %.
/// * `uv_clear_sky` — UV index for clear sky (0+); api.met.no returns 0
///   at night, so no separate daylight gate is needed.
/// * `wind_ms` — ambient wind speed at 10 m. Higher wind reduces the
///   warmth the rider feels because moving air strips solar-deposited
///   heat off the skin faster than radiation adds it.
pub fn solar_warming_c(cloud_pct: Option<f64>, uv_clear_sky: Option<f64>, wind_ms: f64) -> f64 {
    let (cloud, uv) = match (cloud_pct, uv_clear_sky) {
        (Some(c), Some(u)) if c.is_finite() && u.is_finite() => (c, u),
        // Cloud-only fallback: gives half the boost when UV is missing.
        // Keeps long-range forecasts (which sometimes drop UV) from going
        // sun-blind, while still flagging that we're guessing.
        (Some(c), _) if c.is_finite() => {
            let clear = (1.0 - c / 100.0).clamp(0.0, 1.0);
            let wind_att = solar_wind_attenuation(wind_ms);
            return 0.5 * SOLAR_MAX_C * clear * wind_att;
        }
        _ => return 0.0,
    };
    let clear = (1.0 - cloud / 100.0).clamp(0.0, 1.0);
    let uv_factor = (uv / UV_NORDIC_PEAK).clamp(0.0, 1.5);
    let wind_att = solar_wind_attenuation(wind_ms);
    SOLAR_MAX_C * clear * uv_factor * wind_att
}

/// `1 / (1 + k · v)` with non-finite or negative wind treated as 0.
/// Capped at 1.0 to keep the function monotone-decreasing in v.
fn solar_wind_attenuation(wind_ms: f64) -> f64 {
    if !wind_ms.is_finite() {
        return 1.0;
    }
    let v = wind_ms.max(0.0);
    1.0 / (1.0 + SOLAR_WIND_K * v)
}

/// Felt-T including solar warming. Wraps [`apparent_temp`] and adds the
/// non-negative `solar_warming_c` term; pass `None` for either sun input
/// to skip the solar contribution. Used by the score path; the bare
/// [`apparent_temp`] is kept for callers (and tests) that only want the
/// wind-chill / heat-index base.
pub fn apparent_temp_with_sun(
    t_c: f64,
    wind_ms: f64,
    humidity_pct: Option<f64>,
    cloud_pct: Option<f64>,
    uv_clear_sky: Option<f64>,
) -> f64 {
    let base = apparent_temp(t_c, wind_ms, humidity_pct);
    if !base.is_finite() {
        return base;
    }
    base + solar_warming_c(cloud_pct, uv_clear_sky, wind_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64, eps: f64) {
        assert!((a - b).abs() < eps, "expected {a} ≈ {b} (eps {eps})");
    }

    #[test]
    fn wind_chill_passthrough_when_warm() {
        approx(wind_chill_c(15.0, 8.0), 15.0, 1e-9);
    }

    #[test]
    fn wind_chill_passthrough_when_calm() {
        approx(wind_chill_c(0.0, 1.0), 0.0, 1e-9);
    }

    #[test]
    fn wind_chill_passthrough_on_nan() {
        assert!(wind_chill_c(f64::NAN, 5.0).is_nan());
        approx(wind_chill_c(0.0, f64::NAN), 0.0, 1e-9);
    }

    #[test]
    fn wind_chill_zero_celsius_moderate_wind() {
        // 0 °C, 5 m/s (18 km/h) ambient → about −5 °C.
        let chill = wind_chill_c(0.0, 5.0);
        assert!((-5.5..-3.0).contains(&chill), "got {chill}");
    }

    #[test]
    fn wind_chill_minus_ten_strong_wind() {
        // −10 °C, 10 m/s (36 km/h) → about −20 °C.
        let chill = wind_chill_c(-10.0, 10.0);
        assert!((-22.0..-18.0).contains(&chill), "got {chill}");
    }

    #[test]
    fn cyclist_effective_wind_floors_at_self_wind() {
        approx(cyclist_effective_wind_ms(0.0), CYCLING_WIND_MS, 1e-9);
        let combined = cyclist_effective_wind_ms(8.0);
        // sqrt(64 + 25) ≈ 9.43, less than scalar sum.
        assert!(combined > 8.0 && combined < 8.0 + CYCLING_WIND_MS);
        approx(combined, 89_f64.sqrt(), 1e-9);
    }

    #[test]
    fn heat_index_passthrough_when_cool_or_dry() {
        approx(heat_index_c(22.0, 30.0), 22.0, 1e-9);
        approx(heat_index_c(18.0, 90.0), 18.0, 1e-9);
    }

    #[test]
    fn heat_index_warm_humid_adds_load() {
        // NWS table: 30 °C, 70 % RH → ≈ 35 °C.
        let hi = heat_index_c(30.0, 70.0);
        assert!((33.0..37.0).contains(&hi), "got {hi}");
    }

    #[test]
    fn apparent_temp_cold_side_uses_chill() {
        let felt = apparent_temp(0.0, 8.0, None);
        assert!(felt < -5.0, "got {felt}");
    }

    #[test]
    fn apparent_temp_calm_cold_still_chills_from_self_wind() {
        // Calm ambient at 0 °C, but rider self-wind = 5 m/s.
        let felt = apparent_temp(0.0, 0.0, None);
        assert!((-5.5..-2.0).contains(&felt), "got {felt}");
    }

    #[test]
    fn apparent_temp_neutral_band_passes_through() {
        approx(apparent_temp(15.0, 5.0, Some(60.0)), 15.0, 1e-9);
        approx(apparent_temp(18.0, 12.0, None), 18.0, 1e-9);
    }

    #[test]
    fn apparent_temp_warm_humid_uses_heat_index() {
        // 28 °C, 75 % RH → ≈ 32 °C.
        let felt = apparent_temp(28.0, 2.0, Some(75.0));
        assert!((30.0..34.0).contains(&felt), "got {felt}");
    }

    #[test]
    fn apparent_temp_warm_no_humidity_passes_through() {
        approx(apparent_temp(28.0, 2.0, None), 28.0, 1e-9);
    }

    #[test]
    fn apparent_temp_handles_nan() {
        assert!(apparent_temp(f64::NAN, 5.0, Some(70.0)).is_nan());
    }

    #[test]
    fn solar_warming_clear_sky_peak_uv_still_air() {
        // Clear sky, peak Nordic UV, zero ambient wind → at SOLAR_MAX_C.
        approx(
            solar_warming_c(Some(0.0), Some(UV_NORDIC_PEAK), 0.0),
            SOLAR_MAX_C,
            1e-9,
        );
    }

    #[test]
    fn solar_warming_overcast_returns_zero() {
        approx(
            solar_warming_c(Some(100.0), Some(UV_NORDIC_PEAK), 0.0),
            0.0,
            1e-9,
        );
    }

    #[test]
    fn solar_warming_no_uv_at_night() {
        // api.met.no returns UV=0 at night; expect no warming.
        approx(solar_warming_c(Some(0.0), Some(0.0), 0.0), 0.0, 1e-9);
    }

    #[test]
    fn solar_warming_missing_uv_uses_half_boost() {
        // No UV reading + clear sky → falls back to half SOLAR_MAX_C, no
        // wind. Keeps long-range forecasts sun-aware without overclaiming.
        approx(
            solar_warming_c(Some(0.0), None, 0.0),
            0.5 * SOLAR_MAX_C,
            1e-9,
        );
    }

    #[test]
    fn solar_warming_missing_cloud_returns_zero() {
        // Without cloud cover we can't tell sunny from grey — bail.
        approx(solar_warming_c(None, Some(UV_NORDIC_PEAK), 0.0), 0.0, 1e-9);
        approx(solar_warming_c(None, None, 0.0), 0.0, 1e-9);
    }

    #[test]
    fn solar_warming_wind_attenuates() {
        let calm = solar_warming_c(Some(0.0), Some(UV_NORDIC_PEAK), 0.0);
        let breezy = solar_warming_c(Some(0.0), Some(UV_NORDIC_PEAK), 10.0);
        // 1 / (1 + 0.1 · 10) = 0.5.
        approx(breezy / calm, 0.5, 1e-9);
    }

    #[test]
    fn solar_warming_uv_clamps_above_peak() {
        // Midsummer noon UV can exceed Nordic peak; we cap at 1.5×.
        let high = solar_warming_c(Some(0.0), Some(UV_NORDIC_PEAK * 3.0), 0.0);
        approx(high, SOLAR_MAX_C * 1.5, 1e-9);
    }

    #[test]
    fn solar_warming_partial_clouds_scale_linearly() {
        let half = solar_warming_c(Some(50.0), Some(UV_NORDIC_PEAK), 0.0);
        let full = solar_warming_c(Some(0.0), Some(UV_NORDIC_PEAK), 0.0);
        approx(half / full, 0.5, 1e-9);
    }

    #[test]
    fn solar_warming_handles_nan_inputs() {
        // Non-finite cloud → no signal at all, even if UV is OK.
        approx(solar_warming_c(Some(f64::NAN), Some(5.0), 4.0), 0.0, 1e-9);
        // Non-finite UV with finite cloud → falls back to the cloud-only
        // half-boost path, same as missing UV. Both "Some(NaN)" and
        // "None" represent "we don't trust this reading".
        let cloud_only = solar_warming_c(Some(50.0), None, 4.0);
        let with_nan_uv = solar_warming_c(Some(50.0), Some(f64::NAN), 4.0);
        approx(with_nan_uv, cloud_only, 1e-9);
        assert!(cloud_only > 0.0);
    }

    #[test]
    fn apparent_temp_with_sun_warms_cold_clear_day() {
        // 5 °C, light wind, clear sky, peak UV → felt-T warmer than the
        // wind-chilled base.
        let base = apparent_temp(5.0, 2.0, None);
        let with_sun = apparent_temp_with_sun(5.0, 2.0, None, Some(0.0), Some(UV_NORDIC_PEAK));
        assert!(
            with_sun > base,
            "with_sun {with_sun} should exceed base {base}"
        );
        // Magnitude check: warming should be inside [2.5, SOLAR_MAX_C].
        let delta = with_sun - base;
        assert!((2.5..=SOLAR_MAX_C).contains(&delta), "got delta {delta}");
    }

    #[test]
    fn apparent_temp_with_sun_overcast_matches_base() {
        let base = apparent_temp(5.0, 2.0, None);
        let with_sun = apparent_temp_with_sun(5.0, 2.0, None, Some(100.0), Some(UV_NORDIC_PEAK));
        approx(with_sun, base, 1e-9);
    }

    #[test]
    fn apparent_temp_with_sun_pushes_hot_day_further_from_optimum() {
        // 28 °C heat-index base + sun → even hotter felt-T. The score
        // layer's temp-curve will then penalise correctly.
        let base = apparent_temp(28.0, 2.0, Some(75.0));
        let with_sun =
            apparent_temp_with_sun(28.0, 2.0, Some(75.0), Some(0.0), Some(UV_NORDIC_PEAK));
        assert!(
            with_sun > base,
            "with_sun {with_sun} should exceed base {base}"
        );
    }

    #[test]
    fn apparent_temp_with_sun_propagates_nan() {
        assert!(
            apparent_temp_with_sun(f64::NAN, 5.0, None, Some(0.0), Some(UV_NORDIC_PEAK)).is_nan()
        );
    }
}
