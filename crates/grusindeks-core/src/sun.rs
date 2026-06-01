//! Sunrise/sunset for a date and location, from the standard "Sunrise/Sunset
//! Algorithm" (Almanac for Computers, 1990). Accurate to ~1 minute at
//! mid-latitudes — plenty for a "solnedgang 22:14" hint — and dependency-free
//! beyond `chrono`. Returns `None` for each event during midnight sun / polar
//! night, when the sun doesn't cross the horizon.

use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Serialize};

/// Official zenith for sunrise/sunset (90°50'): the geometric horizon plus
/// atmospheric refraction and the sun's apparent radius.
const ZENITH_DEG: f64 = 90.833;

/// Sunrise and sunset for one date and location, in UTC.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SunTimes {
    pub sunrise: Option<DateTime<Utc>>,
    pub sunset: Option<DateTime<Utc>>,
}

/// Sunrise/sunset for `date` (civil date at the location) at `lat_deg`/`lon_deg`
/// (north / east positive).
pub fn sun_times(date: NaiveDate, lat_deg: f64, lon_deg: f64) -> SunTimes {
    SunTimes {
        sunrise: event(date, lat_deg, lon_deg, true),
        sunset: event(date, lat_deg, lon_deg, false),
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
    }
}
