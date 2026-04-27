//! `api.met.no/weatherapi/locationforecast/2.0/complete` — 9-day forecast.
//!
//! Maps the upstream timeseries to a `Vec<HourlyConditions>` from the core
//! crate. We only emit entries that include `next_1_hours.details` so the
//! per-hour precipitation is real, not back-derived from a 6-hour bucket.
//!
//! We deliberately use `/complete` over `/compact`: the latter omits
//! `probability_of_precipitation`, `wind_speed_of_gust` and
//! `ultraviolet_index_clear_sky` — fields the Grusindeks score and the
//! drying model both depend on. Payload is ~2.4× larger but still tiny
//! (~90KB).

use chrono::{DateTime, Utc};
use grusindeks_core::geo::Point;
use grusindeks_core::types::{Forecast, HourlyConditions, Resolution};
use serde::Deserialize;

use crate::client::{ClientError, MetClient};

const PATH: &str = "/weatherapi/locationforecast/2.0/complete";

#[derive(Debug, Deserialize)]
struct CompactResponse {
    properties: CompactProps,
}

#[derive(Debug, Deserialize)]
struct CompactProps {
    timeseries: Vec<CompactSeries>,
}

#[derive(Debug, Deserialize)]
struct CompactSeries {
    time: DateTime<Utc>,
    data: CompactData,
}

#[derive(Debug, Deserialize)]
struct CompactData {
    instant: CompactInstant,
    #[serde(default)]
    next_1_hours: Option<CompactInterval>,
    #[serde(default)]
    next_6_hours: Option<CompactInterval>,
}

#[derive(Debug, Deserialize)]
struct CompactInstant {
    #[serde(default)]
    details: CompactInstantDetails,
}

#[derive(Debug, Default, Deserialize)]
struct CompactInstantDetails {
    air_temperature: Option<f64>,
    wind_speed: Option<f64>,
    wind_speed_of_gust: Option<f64>,
    wind_from_direction: Option<f64>,
    relative_humidity: Option<f64>,
    cloud_area_fraction: Option<f64>,
    ultraviolet_index_clear_sky: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct CompactInterval {
    #[serde(default)]
    details: CompactIntervalDetails,
}

#[derive(Debug, Default, Deserialize)]
struct CompactIntervalDetails {
    precipitation_amount: Option<f64>,
    probability_of_precipitation: Option<f64>,
}

/// Convert the upstream JSON into a `Forecast`.
///
/// `api.met.no` publishes per-hour data (`next_1_hours`) for the first
/// ~60 hours and only 6-hour buckets (`next_6_hours`) beyond that. We emit:
///
/// * one `Resolution::Hourly` entry per `next_1_hours` block, and
/// * six `Resolution::SixHourly` entries per `next_6_hours` block — one for
///   each hour `t..t+6h` — with `precipitation_mm = bucket_total / 6` and
///   the bucket's probability carried as-is. Instant-grade fields
///   (temperature, wind) are reused across the six hours, since MET only
///   gives them at the bucket start.
///
/// When a 6h bucket overlaps an already-emitted hourly entry, hourly wins
/// — it's the higher-fidelity source.
fn into_forecast(point: Point, raw: CompactResponse) -> Forecast {
    use std::collections::BTreeMap;
    let mut by_time: BTreeMap<DateTime<Utc>, HourlyConditions> = BTreeMap::new();

    for s in raw.properties.timeseries {
        let Some(temp) = s.data.instant.details.air_temperature else {
            continue;
        };
        let Some(wind) = s.data.instant.details.wind_speed else {
            continue;
        };

        if let Some(next1) = s.data.next_1_hours {
            let precip = next1.details.precipitation_amount.unwrap_or(0.0);
            by_time.insert(
                s.time,
                HourlyConditions {
                    time: s.time,
                    temperature_c: temp,
                    wind_speed_ms: wind,
                    precipitation_mm: precip,
                    wind_gust_ms: s.data.instant.details.wind_speed_of_gust,
                    wind_from_deg: s.data.instant.details.wind_from_direction,
                    probability_of_precip: next1.details.probability_of_precipitation,
                    relative_humidity: s.data.instant.details.relative_humidity,
                    cloud_area_fraction: s.data.instant.details.cloud_area_fraction,
                    uv_index_clear_sky: s.data.instant.details.ultraviolet_index_clear_sky,
                    resolution: Resolution::Hourly,
                },
            );
            continue;
        }

        if let Some(next6) = s.data.next_6_hours {
            let bucket_total = next6.details.precipitation_amount.unwrap_or(0.0);
            let per_hour_mm = bucket_total / 6.0;
            for offset in 0..6 {
                let t = s.time + chrono::Duration::hours(offset);
                // Hourly entries already emitted win — never overwrite them.
                by_time.entry(t).or_insert(HourlyConditions {
                    time: t,
                    temperature_c: temp,
                    wind_speed_ms: wind,
                    precipitation_mm: per_hour_mm,
                    wind_gust_ms: s.data.instant.details.wind_speed_of_gust,
                    wind_from_deg: s.data.instant.details.wind_from_direction,
                    probability_of_precip: next6.details.probability_of_precipitation,
                    relative_humidity: s.data.instant.details.relative_humidity,
                    cloud_area_fraction: s.data.instant.details.cloud_area_fraction,
                    uv_index_clear_sky: s.data.instant.details.ultraviolet_index_clear_sky,
                    resolution: Resolution::SixHourly,
                });
            }
        }
    }

    Forecast {
        point,
        hours: by_time.into_values().collect(),
    }
}

/// Parse a `compact` JSON body for a known `point`. Useful in tests and
/// for the disk-cache replay path.
pub fn parse_compact(point: Point, body: &str) -> Result<Forecast, ClientError> {
    let raw: CompactResponse =
        serde_json::from_str(body).map_err(|e| ClientError::Decode(e.to_string()))?;
    Ok(into_forecast(point, raw))
}

/// Fetch the 9-day forecast for `point` from `api.met.no`.
///
/// `point` is truncated to 4 decimals before being sent (TOS).
/// Routes through [`MetClient::fetch_text`] so the disk cache (when
/// configured) honours `Expires` / `If-Modified-Since` per MET TOS.
pub async fn fetch(client: &MetClient, point: Point) -> Result<Forecast, ClientError> {
    let p = point.truncated();
    let mut url = client.api_url(PATH)?;
    url.query_pairs_mut()
        .clear()
        .append_pair("lat", &format!("{:.4}", p.lat))
        .append_pair("lon", &format!("{:.4}", p.lon));

    let body = client.fetch_text(url).await?;
    parse_compact(p, &body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use url::Url;
    use wiremock::matchers::{header, method, path as path_m, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::client::{MetClientConfig, UserAgent};

    const FIXTURE: &str = include_str!("../../../fixtures/locationforecast_oslo.json");

    fn ua() -> UserAgent {
        UserAgent::new("grusindeks-test", "0.1", "dev@example.invalid").unwrap()
    }

    fn oslo() -> Point {
        Point::new(59.9139, 10.7522)
    }

    // ---- Pure parser ----

    #[test]
    fn parser_ingests_real_fixture() {
        let f = parse_compact(oslo(), FIXTURE).expect("fixture parses");
        assert!(!f.hours.is_empty(), "expected at least one hour");
        let first = &f.hours[0];
        assert!(
            (first.temperature_c - 10.1).abs() < 1e-6,
            "temp {}",
            first.temperature_c
        );
        assert!(
            (first.wind_speed_ms - 1.9).abs() < 1e-6,
            "wind {}",
            first.wind_speed_ms
        );
        assert_eq!(first.precipitation_mm, 0.0);
        assert_eq!(first.wind_from_deg, Some(261.0));
        assert_eq!(first.relative_humidity, Some(30.9));
        assert_eq!(first.cloud_area_fraction, Some(90.0));
        // Time matches the first timeseries entry.
        assert_eq!(first.time.to_rfc3339(), "2026-04-26T10:00:00+00:00");
    }

    #[test]
    fn parser_returns_hours_in_chronological_order() {
        let f = parse_compact(oslo(), FIXTURE).unwrap();
        for w in f.hours.windows(2) {
            assert!(
                w[0].time < w[1].time,
                "out of order at {} vs {}",
                w[0].time,
                w[1].time
            );
        }
    }

    #[test]
    fn parser_emits_well_formed_hours_for_every_emitted_entry() {
        // Some entries are hourly, others are expanded from 6h buckets.
        // All must have non-negative precipitation, regardless of source.
        let f = parse_compact(oslo(), FIXTURE).unwrap();
        for h in &f.hours {
            assert!(h.precipitation_mm >= 0.0);
        }
    }

    #[test]
    fn parser_expands_six_hour_buckets_into_hourly_entries() {
        // The fixture contains both `next_1_hours` (≤60h) and `next_6_hours`
        // (later) entries. The parser should yield both kinds, with the
        // 6h-derived ones marked `Resolution::SixHourly`.
        use grusindeks_core::types::Resolution;
        let f = parse_compact(oslo(), FIXTURE).unwrap();
        let hourly_count = f
            .hours
            .iter()
            .filter(|h| h.resolution == Resolution::Hourly)
            .count();
        let six_count = f
            .hours
            .iter()
            .filter(|h| h.resolution == Resolution::SixHourly)
            .count();
        assert!(hourly_count > 0, "no hourly entries");
        assert!(six_count > 0, "no 6h-derived entries");
        // 6h-derived hours should outnumber the hourly ones since the
        // long-range portion of the forecast is much larger.
        assert!(
            six_count > hourly_count,
            "expected more 6h-derived entries than hourly, got hourly={hourly_count} six={six_count}"
        );
    }

    #[test]
    fn parser_prefers_hourly_over_six_hourly_when_both_present() {
        // A timeseries entry can carry both `next_1_hours` and
        // `next_6_hours`. We must keep the hourly version of that timestamp.
        use grusindeks_core::types::Resolution;
        let f = parse_compact(oslo(), FIXTURE).unwrap();
        // The first emitted hour is from `next_1_hours` per the fixture.
        assert_eq!(f.hours[0].resolution, Resolution::Hourly);
    }

    #[test]
    fn parser_returns_strictly_unique_hours_after_six_hour_expansion() {
        // Expansion must not introduce duplicate timestamps even when
        // adjacent 6h buckets touch hourly windows.
        let f = parse_compact(oslo(), FIXTURE).unwrap();
        let mut seen = std::collections::HashSet::new();
        for h in &f.hours {
            assert!(seen.insert(h.time), "duplicate hour {}", h.time);
        }
    }

    /// Contract test: the *endpoint* we use must populate every field the
    /// scoring layer depends on. Catches the class of bug where the fixture
    /// is captured from the wrong endpoint variant (e.g. `/compact` strips
    /// `probability_of_precipitation`, `wind_speed_of_gust` and
    /// `ultraviolet_index_clear_sky` — fields the score and drying model
    /// both consume). If this ever fails, either the fixture was refreshed
    /// from `/compact` by mistake, or MET changed the endpoint contract.
    #[test]
    fn fixture_populates_every_field_the_score_consumes() {
        let f = parse_compact(oslo(), FIXTURE).unwrap();
        assert!(
            f.hours.iter().any(|h| h.probability_of_precip.is_some()),
            "no hour has probability_of_precip — likely captured from /compact"
        );
        assert!(
            f.hours.iter().any(|h| h.wind_gust_ms.is_some()),
            "no hour has wind_gust_ms — likely captured from /compact"
        );
        assert!(
            f.hours.iter().any(|h| h.uv_index_clear_sky.is_some()),
            "no hour has uv_index_clear_sky — likely captured from /compact"
        );
        // These are present in both endpoints; assert anyway so any future
        // endpoint regression is caught here too.
        assert!(f.hours.iter().any(|h| h.relative_humidity.is_some()));
        assert!(f.hours.iter().any(|h| h.cloud_area_fraction.is_some()));
    }

    // ---- HTTP-level wiremock test ----

    #[tokio::test]
    async fn fetch_truncates_coordinates_to_4_decimals() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path_m("/weatherapi/locationforecast/2.0/complete"))
            // 5+ decimals would be a TOS violation. The mock only matches 4.
            .and(query_param("lat", "59.9139"))
            .and(query_param("lon", "10.7522"))
            .and(header(
                "user-agent",
                "grusindeks-test/0.1 dev@example.invalid",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_string(FIXTURE))
            .mount(&server)
            .await;

        let cfg = MetClientConfig {
            user_agent: ua(),
            api_base: Url::parse(&format!("{}/", server.uri())).unwrap(),
            frost_base: Url::parse(&format!("{}/", server.uri())).unwrap(),
            frost_client_id: None,
            timeout: Duration::from_secs(5),
            cache_dir: None,
        };
        let client = MetClient::new(cfg).unwrap();

        // Caller passes 6-decimal precision; client must truncate to 4.
        let f = fetch(&client, Point::new(59.913918, 10.752230))
            .await
            .unwrap();
        assert!(!f.hours.is_empty());
        assert_eq!(f.point, Point::new(59.9139, 10.7522));
    }

    #[tokio::test]
    async fn fetch_surfaces_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_m("/weatherapi/locationforecast/2.0/complete"))
            .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
            .mount(&server)
            .await;

        let cfg = MetClientConfig {
            user_agent: ua(),
            api_base: Url::parse(&format!("{}/", server.uri())).unwrap(),
            frost_base: Url::parse(&format!("{}/", server.uri())).unwrap(),
            frost_client_id: None,
            timeout: Duration::from_secs(5),
            cache_dir: None,
        };
        let client = MetClient::new(cfg).unwrap();

        let err = fetch(&client, oslo()).await.unwrap_err();
        match err {
            ClientError::Http { status: 403, .. } => {}
            other => panic!("expected HTTP 403 error, got {other:?}"),
        }
    }
}
