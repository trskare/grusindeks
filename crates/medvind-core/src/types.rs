//! Domain types shared across scoring, drying, and the network/CLI layers.
//!
//! Everything here is plain data — `Serialize`/`Deserialize` for both the
//! disk cache and `--json` CLI output, `PartialEq` for snapshot tests.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::geo::Point;

/// Where the underlying forecast values came from. `api.met.no` provides
/// per-hour data for the first ~60 hours; beyond that, only 6-hour buckets
/// are published. Tracking this lets us downgrade confidence on long-range
/// days and skip best-window detection when the resolution is too coarse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Resolution {
    /// Sourced from a `next_1_hours` block — true hourly fidelity.
    #[default]
    Hourly,
    /// Synthesized by spreading a `next_6_hours` block across its 6 hours.
    /// Temperature/wind are still from the per-hour `instant`, but
    /// precipitation and probability are bucket-level.
    SixHourly,
}

/// What we know about the weather at one geographical point during one hour.
///
/// Required fields are the variables we can rely on from
/// `locationforecast/2.0/complete` for any forecast horizon. Optional fields
/// may be missing for long-term (>60h) forecasts or non-Nordic regions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HourlyConditions {
    /// Start of the hour, in UTC.
    pub time: DateTime<Utc>,
    /// Air temperature 2m above ground, °C.
    pub temperature_c: f64,
    /// 10-min average wind speed at 10m, m/s.
    pub wind_speed_ms: f64,
    /// Precipitation amount during this hour, mm.
    pub precipitation_mm: f64,

    /// Maximum gust during this hour, m/s.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wind_gust_ms: Option<f64>,
    /// Direction the wind is coming from, degrees clockwise from north.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wind_from_deg: Option<f64>,
    /// Probability of precipitation during this hour, 0–100.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probability_of_precip: Option<f64>,
    /// Relative humidity at 2m, 0–100.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relative_humidity: Option<f64>,
    /// Total cloud cover, 0–100.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloud_area_fraction: Option<f64>,
    /// UV index for clear-sky conditions, 0–11+.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uv_index_clear_sky: Option<f64>,

    /// Where the data originated. Defaults to `Hourly` for backward compat
    /// with serialized blobs predating the multi-day forecast feature.
    #[serde(default)]
    pub resolution: Resolution,
}

impl HourlyConditions {
    /// Convenience constructor for the four required fields. Tests and
    /// scoring use this; production parsers fill the optional fields too.
    pub fn minimal(
        time: DateTime<Utc>,
        temperature_c: f64,
        wind_speed_ms: f64,
        precipitation_mm: f64,
    ) -> Self {
        Self {
            time,
            temperature_c,
            wind_speed_ms,
            precipitation_mm,
            wind_gust_ms: None,
            wind_from_deg: None,
            probability_of_precip: None,
            relative_humidity: None,
            cloud_area_fraction: None,
            uv_index_clear_sky: None,
            resolution: Resolution::Hourly,
        }
    }
}

/// A time-ordered series of hourly conditions for a single geographical
/// point.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Forecast {
    pub point: Point,
    pub hours: Vec<HourlyConditions>,
}

/// A half-open `[start, end)` time window for which we want to compute a
/// score. The window is normally the user's intended ride.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RideWindow {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

impl RideWindow {
    /// Build a window of `hours` hours starting at `start`. Panics on
    /// non-positive `hours` — these would be a programming error.
    pub fn from_hours(start: DateTime<Utc>, hours: i64) -> Self {
        assert!(hours > 0, "RideWindow hours must be positive, got {hours}");
        Self {
            start,
            end: start + Duration::hours(hours),
        }
    }

    pub fn duration_hours(&self) -> i64 {
        (self.end - self.start).num_hours()
    }

    /// Inclusive of `start`, exclusive of `end`.
    pub fn contains(&self, t: DateTime<Utc>) -> bool {
        t >= self.start && t < self.end
    }
}

/// A named ride location: human label, center coordinates, and radius the
/// user wants covered. Comes from config (`places.<name>`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Location {
    pub name: String,
    pub center: Point,
    pub radius_km: f64,
}

// `Point` lives in `geo`, but we need it in our serialized payloads; provide
// the impls here to avoid a hard dep on `serde` from `geo` in the public API.
impl Serialize for Point {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("Point", 2)?;
        st.serialize_field("lat", &self.lat)?;
        st.serialize_field("lon", &self.lon)?;
        st.end()
    }
}

impl<'de> Deserialize<'de> for Point {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            lat: f64,
            lon: f64,
        }
        let r = Raw::deserialize(d)?;
        Ok(Point::new(r.lat, r.lon))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t(h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 4, 26, h, 0, 0).unwrap()
    }

    #[test]
    fn hourly_minimal_constructor_leaves_optional_fields_none() {
        let h = HourlyConditions::minimal(t(12), 16.0, 4.0, 0.0);
        assert_eq!(h.temperature_c, 16.0);
        assert!(h.wind_gust_ms.is_none());
        assert!(h.uv_index_clear_sky.is_none());
        assert_eq!(h.resolution, Resolution::Hourly);
    }

    #[test]
    fn hourly_resolution_defaults_to_hourly_when_missing_from_json() {
        // Older serialized blobs (cache files, fixtures) won't carry the
        // field. They must keep deserializing as `Hourly`.
        let json = serde_json::json!({
            "time": "2026-04-26T12:00:00Z",
            "temperature_c": 16.0,
            "wind_speed_ms": 4.0,
            "precipitation_mm": 0.0,
        });
        let h: HourlyConditions = serde_json::from_value(json).unwrap();
        assert_eq!(h.resolution, Resolution::Hourly);
    }

    #[test]
    fn hourly_serde_roundtrip_skips_none() {
        let h = HourlyConditions::minimal(t(12), 16.0, 4.0, 0.0);
        let json = serde_json::to_string(&h).unwrap();
        // None fields are omitted from the JSON payload.
        assert!(!json.contains("wind_gust_ms"), "got {json}");
        let back: HourlyConditions = serde_json::from_str(&json).unwrap();
        assert_eq!(back, h);
    }

    #[test]
    fn hourly_serde_keeps_some() {
        let h = HourlyConditions {
            wind_gust_ms: Some(7.5),
            ..HourlyConditions::minimal(t(12), 16.0, 4.0, 0.0)
        };
        let json = serde_json::to_string(&h).unwrap();
        assert!(json.contains("wind_gust_ms"), "got {json}");
        let back: HourlyConditions = serde_json::from_str(&json).unwrap();
        assert_eq!(back, h);
    }

    #[test]
    fn ride_window_contains_is_half_open() {
        let w = RideWindow::from_hours(t(14), 3); // 14:00..17:00
        assert!(w.contains(t(14)));
        assert!(w.contains(t(16)));
        assert!(!w.contains(t(17))); // exclusive end
        assert!(!w.contains(t(13)));
    }

    #[test]
    fn ride_window_duration() {
        let w = RideWindow::from_hours(t(14), 3);
        assert_eq!(w.duration_hours(), 3);
    }

    #[test]
    #[should_panic]
    fn ride_window_rejects_zero_hours() {
        let _ = RideWindow::from_hours(t(14), 0);
    }

    #[test]
    fn forecast_serde_roundtrip() {
        let f = Forecast {
            point: Point::new(59.9139, 10.7522),
            hours: vec![HourlyConditions::minimal(t(12), 16.0, 4.0, 0.0)],
        };
        let json = serde_json::to_string(&f).unwrap();
        let back: Forecast = serde_json::from_str(&json).unwrap();
        assert_eq!(back, f);
    }

    #[test]
    fn location_serde_roundtrip() {
        let l = Location {
            name: "oslo".into(),
            center: Point::new(59.9139, 10.7522),
            radius_km: 20.0,
        };
        let json = serde_json::to_string(&l).unwrap();
        let back: Location = serde_json::from_str(&json).unwrap();
        assert_eq!(back, l);
    }
}
