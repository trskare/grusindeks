//! Sunrise/sunset for a date and location, from the standard "Sunrise/Sunset
//! Algorithm" (Almanac for Computers, 1990). Accurate to ~1 minute at
//! mid-latitudes — plenty for a "solnedgang 22:14" hint — and dependency-free
//! beyond `chrono`. Returns `None` for each event during midnight sun / polar
//! night, when the sun doesn't cross the horizon.

use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Timelike, Utc};
use serde::{Deserialize, Serialize};

/// Official zenith for sunrise/sunset (90°50'): the geometric horizon plus
/// atmospheric refraction and the sun's apparent radius.
const ZENITH_DEG: f64 = 90.833;

/// Solar elevation (altitude above the horizon, degrees) at `time` for the
/// given location. Negative when the sun is below the horizon (night).
///
/// Uses the same low-precision sun-longitude → declination model as
/// [`sun_times`], with an approximate local solar time (UTC + longitude/15,
/// ignoring the equation of time — worth < ~4° of hour angle, immaterial
/// for a drying-rate heuristic). Good to a degree or two at mid-latitudes,
/// which is all the gravel drying model needs to (a) zero out the solar term
/// at night and (b) scale it down through the dark half of the year.
pub fn solar_elevation_deg(time: DateTime<Utc>, lat_deg: f64, lon_deg: f64) -> f64 {
    let d2r = std::f64::consts::PI / 180.0;
    let utc_hours =
        time.hour() as f64 + time.minute() as f64 / 60.0 + time.second() as f64 / 3600.0;
    // Sun's true longitude → declination (same model as `event`).
    let t = time.ordinal() as f64 + utc_hours / 24.0;
    let m = 0.9856 * t - 3.289;
    let l =
        (m + 1.916 * (m * d2r).sin() + 0.020 * (2.0 * m * d2r).sin() + 282.634).rem_euclid(360.0);
    let dec = (0.39782 * (l * d2r).sin()).clamp(-1.0, 1.0).asin(); // radians
                                                                   // Approximate local solar time → hour angle (0 at solar noon).
    let solar_time = utc_hours + lon_deg / 15.0;
    let hour_angle = (solar_time - 12.0) * 15.0 * d2r;
    let lat = lat_deg * d2r;
    let sin_elev = lat.sin() * dec.sin() + lat.cos() * dec.cos() * hour_angle.cos();
    sin_elev.clamp(-1.0, 1.0).asin() / d2r
}

/// When the sun never crosses the horizon on a date (so there's neither a
/// sunrise nor a sunset), which half of the polar year it is — the bright one
/// (sun always up) or the dark one (sun always down). Lets a consumer tell the
/// two `None`-sunset cases apart: e.g. don't suggest "wait for a later window"
/// during polar night, when every later hour is dark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PolarDay {
    /// Midnight sun — the sun stays above the horizon all day.
    MidnightSun,
    /// Polar night — the sun stays below the horizon all day.
    PolarNight,
}

/// Sunrise and sunset for one date and location, in UTC.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SunTimes {
    pub sunrise: Option<DateTime<Utc>>,
    pub sunset: Option<DateTime<Utc>>,
    /// Set only when neither `sunrise` nor `sunset` occurs (the sun doesn't
    /// cross the horizon): which polar regime it is. `None` on ordinary days.
    #[serde(default)]
    pub polar: Option<PolarDay>,
}

/// Sunrise/sunset for `date` (civil date at the location) at `lat_deg`/`lon_deg`
/// (north / east positive).
pub fn sun_times(date: NaiveDate, lat_deg: f64, lon_deg: f64) -> SunTimes {
    let sunrise = event(date, lat_deg, lon_deg, true);
    let sunset = event(date, lat_deg, lon_deg, false);
    // No horizon crossing → classify by the sun's height at solar noon (its
    // daily maximum): above the horizon means it never set (midnight sun),
    // below means it never rose (polar night).
    let polar = match (sunrise, sunset) {
        (None, None) => date.and_hms_opt(0, 0, 0).map(|midnight| {
            let noon_secs = ((12.0 - lon_deg / 15.0).rem_euclid(24.0) * 3600.0).round() as i64;
            let noon = Utc.from_utc_datetime(&midnight) + Duration::seconds(noon_secs);
            if solar_elevation_deg(noon, lat_deg, lon_deg) > 0.0 {
                PolarDay::MidnightSun
            } else {
                PolarDay::PolarNight
            }
        }),
        _ => None,
    };
    SunTimes {
        sunrise,
        sunset,
        polar,
    }
}

fn event(date: NaiveDate, lat: f64, lon: f64, sunrise: bool) -> Option<DateTime<Utc>> {
    let d2r = std::f64::consts::PI / 180.0;
    let r2d = 180.0 / std::f64::consts::PI;

    // 1. Day of the year and an approximate event time.
    let n = date.ordinal() as f64;
    let lng_hour = lon / 15.0;
    let t = if sunrise {
        n + (6.0 - lng_hour) / 24.0
    } else {
        n + (18.0 - lng_hour) / 24.0
    };

    // 2. Sun's mean anomaly and true longitude.
    let m = 0.9856 * t - 3.289;
    let l =
        (m + 1.916 * (m * d2r).sin() + 0.020 * (2.0 * m * d2r).sin() + 282.634).rem_euclid(360.0);

    // 3. Right ascension, forced into the same quadrant as the longitude.
    let mut ra = ((0.91764 * (l * d2r).tan()).atan() * r2d).rem_euclid(360.0);
    let l_quadrant = (l / 90.0).floor() * 90.0;
    let ra_quadrant = (ra / 90.0).floor() * 90.0;
    ra = (ra + (l_quadrant - ra_quadrant)) / 15.0; // degrees → hours

    // 4. Declination and the local hour angle.
    let sin_dec = 0.39782 * (l * d2r).sin();
    let cos_dec = sin_dec.asin().cos();
    let cos_h =
        ((ZENITH_DEG * d2r).cos() - sin_dec * (lat * d2r).sin()) / (cos_dec * (lat * d2r).cos());
    if !(-1.0..=1.0).contains(&cos_h) {
        return None; // sun never rises (>1) or never sets (<-1) on this date
    }
    let h = if sunrise {
        360.0 - cos_h.acos() * r2d
    } else {
        cos_h.acos() * r2d
    } / 15.0;

    // 5. Mean time of the event, then convert to UTC hours of the day.
    let local_t = h + ra - 0.06571 * t - 6.622;
    let ut = (local_t - lng_hour).rem_euclid(24.0);

    let secs = (ut * 3600.0).round() as i64;
    let midnight = Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0)?);
    Some(midnight + Duration::seconds(secs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Timelike;

    /// Oslo (59.91 N, 10.75 E) on the summer solstice: sunrise ~04:00,
    /// sunset ~22:40 local (CEST = UTC+2), i.e. ~02:00 / ~20:40 UTC. Allow a
    /// generous tolerance — we only need minute-ish accuracy.
    #[test]
    fn oslo_midsummer_is_a_long_day() {
        let date = NaiveDate::from_ymd_opt(2026, 6, 21).unwrap();
        let s = sun_times(date, 59.91, 10.75);
        let sunrise = s.sunrise.expect("sun rises at midsummer in Oslo");
        let sunset = s.sunset.expect("sun sets at midsummer in Oslo");
        assert!(sunrise < sunset);
        // Daylight well over 18 hours.
        let daylight_h = (sunset - sunrise).num_minutes() as f64 / 60.0;
        assert!(daylight_h > 18.0, "daylight was {daylight_h:.1} h");
        // Sunrise around 02:00 UTC, sunset around 20:40 UTC (± ~30 min).
        assert!(
            (1..=3).contains(&sunrise.hour()),
            "sunrise hour {}",
            sunrise.hour()
        );
        assert!(
            (20..=21).contains(&sunset.hour()),
            "sunset hour {}",
            sunset.hour()
        );
    }

    /// Far north of the Arctic Circle in midsummer: the sun never sets.
    #[test]
    fn polar_midnight_sun_has_no_sunset() {
        let date = NaiveDate::from_ymd_opt(2026, 6, 21).unwrap();
        let s = sun_times(date, 78.22, 15.65); // Longyearbyen
        assert!(s.sunset.is_none());
        assert!(s.sunrise.is_none());
        assert_eq!(s.polar, Some(PolarDay::MidnightSun));
    }

    /// Same place mid-winter: the sun never rises — polar night, distinguished
    /// from midnight sun even though both have no sunrise/sunset.
    #[test]
    fn polar_night_is_classified_distinctly() {
        let date = NaiveDate::from_ymd_opt(2026, 12, 21).unwrap();
        let s = sun_times(date, 78.22, 15.65); // Longyearbyen
        assert!(s.sunset.is_none());
        assert!(s.sunrise.is_none());
        assert_eq!(s.polar, Some(PolarDay::PolarNight));
    }

    // ---- solar_elevation_deg ----

    fn at(y: i32, mo: u32, d: u32, h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, 0, 0).unwrap()
    }

    #[test]
    fn elevation_is_negative_at_night_positive_at_noon() {
        // Oslo. Local solar noon ≈ 11:00 UTC (lon 10.75 → +43 min); midnight
        // ≈ 23:00 UTC.
        let noon = solar_elevation_deg(at(2026, 6, 21, 11), 59.91, 10.75);
        let midnight = solar_elevation_deg(at(2026, 6, 21, 23), 59.91, 10.75);
        assert!(noon > 40.0, "midsummer noon should be high in Oslo: {noon}");
        // Even midsummer, Oslo is south of the Arctic circle → sun dips below.
        assert!(
            midnight < 0.0,
            "Oslo sun should be below horizon at midnight: {midnight}"
        );
    }

    #[test]
    fn summer_noon_is_far_higher_than_winter_noon() {
        // The seasonal swing the drying model must see: a clear winter day
        // contributes far less solar drying than a clear summer day.
        let summer = solar_elevation_deg(at(2026, 6, 21, 11), 59.91, 10.75);
        let winter = solar_elevation_deg(at(2026, 12, 21, 11), 59.91, 10.75);
        assert!(
            winter > 0.0,
            "winter noon sun still rises in Oslo: {winter}"
        );
        assert!(
            summer > winter + 30.0,
            "summer noon ({summer}) should tower over winter noon ({winter})",
        );
    }

    #[test]
    fn equator_noon_is_near_overhead_at_equinox() {
        // Sanity check against a known geometry: equator, equinox, local noon
        // → sun nearly overhead (~90°).
        let elev = solar_elevation_deg(at(2026, 3, 20, 12), 0.0, 0.0);
        assert!(
            elev > 80.0,
            "equinox equator noon should be near zenith: {elev}"
        );
    }
}
