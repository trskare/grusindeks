//! `frost.met.no/observations/v0.jsonld` — historical observations.
//!
//! For the drying model we only need hourly precipitation totals. Frost
//! returns one `data` entry per `referenceTime`, each holding a list of
//! `observations`. We flatten that to a chronological `Vec<HourlyPrecip>`
//! that the drying model can replay. The parser is defensive: negative
//! and non-finite values (occasional sensor artefacts) are dropped rather
//! than fed straight into the drying step, which would otherwise either
//! get clipped to zero or — for NaN — corrupt the surface accumulator.
//!
//! Authentication is HTTP Basic with `client_id` as the username and an
//! empty password. Without a configured `client_id` this module returns
//! [`ClientError::MissingCredentials`] so the CLI can fall back to a
//! "no historic ground data" code path.

use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::client::{ClientError, MetClient};

const PATH: &str = "/observations/v0.jsonld";
pub const ELEMENT_HOURLY_PRECIP: &str = "sum(precipitation_amount PT1H)";
pub const ELEMENT_HOURLY_TEMP: &str = "mean(air_temperature PT1H)";
pub const ELEMENT_HOURLY_WIND: &str = "mean(wind_speed PT1H)";
pub const ELEMENT_HOURLY_HUMIDITY: &str = "mean(relative_humidity PT1H)";
/// Mean total cloud cover over the hour, in percent. Lets the drying-model
/// replay use real sky conditions for the solar drying term instead of a
/// hardcoded overcast fallback — a clear, sunny week now dries the surface
/// faster than a grey one.
pub const ELEMENT_HOURLY_CLOUD: &str = "mean(cloud_area_fraction PT1H)";

/// Comma-separated list of element IDs we ask Frost for in one request
/// when we want full observations (precip + temp + wind + humidity +
/// cloud). Going through one round-trip means the timestamps line up by
/// `referenceTime` and the parser can group them per-hour.
fn full_element_query() -> String {
    [
        ELEMENT_HOURLY_PRECIP,
        ELEMENT_HOURLY_TEMP,
        ELEMENT_HOURLY_WIND,
        ELEMENT_HOURLY_HUMIDITY,
        ELEMENT_HOURLY_CLOUD,
    ]
    .join(",")
}

/// One observed hour of precipitation. The `time` is the *end* of the
/// integration window per the upstream `referenceTime` semantics. Used
/// by the legacy precipitation-only path; the richer `HourlyObservation`
/// is preferred since it lets the drying model use real temp/wind/RH
/// instead of synthesised neutral conditions.
#[derive(Debug, Clone, PartialEq)]
pub struct HourlyPrecip {
    pub time: DateTime<Utc>,
    pub mm: f64,
}

/// A full hour of Frost observations. Every weather field is `Option`
/// since Frost stations don't all carry every sensor — a temperature-only
/// station will yield `mm: None, temp_c: Some(_), wind_ms: None, ...`.
/// Drying can still proceed: missing fields get the same neutral
/// fallbacks the locationforecast parser uses.
#[derive(Debug, Clone, PartialEq)]
pub struct HourlyObservation {
    pub time: DateTime<Utc>,
    pub precip_mm: Option<f64>,
    pub temp_c: Option<f64>,
    pub wind_ms: Option<f64>,
    pub humidity_pct: Option<f64>,
    /// Total cloud cover, 0–100 %. `None` when the station carries no
    /// cloud sensor — the drying replay then falls back to a neutral
    /// overcast value rather than pretending the sky was clear.
    pub cloud_pct: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct ObsResponse {
    data: Vec<ObsEntry>,
}

#[derive(Debug, Deserialize)]
struct ObsEntry {
    #[serde(rename = "referenceTime")]
    reference_time: DateTime<Utc>,
    observations: Vec<ObsValue>,
}

#[derive(Debug, Deserialize)]
struct ObsValue {
    #[serde(rename = "elementId")]
    element_id: String,
    value: f64,
}

/// Parse a Frost observations JSON body into a chronological list of
/// hourly precipitation values. Entries that don't carry the hourly
/// precipitation element are ignored. Non-finite or negative readings
/// are also dropped — Frost will occasionally surface sensor artefacts
/// like `-99.9` or `NaN`, which would either misrepresent the rain
/// (negative clipped to 0 silently inflates "drying time") or corrupt
/// the drying-model accumulator (NaN poisons every later step).
pub fn parse_hourly_precip(body: &str) -> Result<Vec<HourlyPrecip>, ClientError> {
    let raw: ObsResponse =
        serde_json::from_str(body).map_err(|e| ClientError::Decode(e.to_string()))?;
    let mut out: Vec<HourlyPrecip> = raw
        .data
        .into_iter()
        .flat_map(|entry| {
            let t = entry.reference_time;
            entry.observations.into_iter().filter_map(move |o| {
                if o.element_id != ELEMENT_HOURLY_PRECIP {
                    return None;
                }
                if !o.value.is_finite() || o.value < 0.0 {
                    return None;
                }
                Some(HourlyPrecip {
                    time: t,
                    mm: o.value,
                })
            })
        })
        .collect();
    out.sort_by_key(|p| p.time);
    Ok(out)
}

/// Group a Frost response by `referenceTime` and project each known
/// element into the matching field on `HourlyObservation`. Per-field
/// validation is the same as the precipitation-only parser: non-finite
/// and out-of-domain values (negative precip, RH outside [0,100], etc.)
/// are dropped to keep the drying model honest.
pub fn parse_hourly_observations(body: &str) -> Result<Vec<HourlyObservation>, ClientError> {
    let raw: ObsResponse =
        serde_json::from_str(body).map_err(|e| ClientError::Decode(e.to_string()))?;
    let mut out: Vec<HourlyObservation> = raw
        .data
        .into_iter()
        .map(|entry| {
            let mut obs = HourlyObservation {
                time: entry.reference_time,
                precip_mm: None,
                temp_c: None,
                wind_ms: None,
                humidity_pct: None,
                cloud_pct: None,
            };
            for o in entry.observations {
                if !o.value.is_finite() {
                    continue;
                }
                match o.element_id.as_str() {
                    ELEMENT_HOURLY_PRECIP if o.value >= 0.0 => obs.precip_mm = Some(o.value),
                    ELEMENT_HOURLY_TEMP => obs.temp_c = Some(o.value),
                    ELEMENT_HOURLY_WIND if o.value >= 0.0 => obs.wind_ms = Some(o.value),
                    ELEMENT_HOURLY_HUMIDITY if (0.0..=100.0).contains(&o.value) => {
                        obs.humidity_pct = Some(o.value)
                    }
                    ELEMENT_HOURLY_CLOUD if (0.0..=100.0).contains(&o.value) => {
                        obs.cloud_pct = Some(o.value)
                    }
                    _ => {}
                }
            }
            obs
        })
        .collect();
    out.sort_by_key(|o| o.time);
    Ok(out)
}

/// Fetch hourly precipitation observations for `source_id` (e.g.
/// `"SN18700"`) over `[from, to)`. Requires `frost_client_id` in the
/// client config; returns [`ClientError::MissingCredentials`] when it's
/// not set so callers can surface a precise error or fall back without
/// confusing "missing credentials" with the User-Agent validator.
///
/// Prefer [`fetch_hourly_observations`] when you also need temperature,
/// wind, or humidity — it pulls all four element types in a single
/// round-trip and keeps the hourly grouping consistent.
pub async fn fetch_hourly_precip(
    client: &MetClient,
    source_id: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Vec<HourlyPrecip>, ClientError> {
    let body = fetch_observations_body(client, source_id, ELEMENT_HOURLY_PRECIP, from, to).await?;
    parse_hourly_precip(&body)
}

/// Fetch full hourly observations (precip, temperature, wind, humidity)
/// for `source_id` over `[from, to)`. Same auth/error semantics as
/// [`fetch_hourly_precip`]. Use this whenever you're going to feed the
/// readings into the drying model — it lets the simulation honour the
/// actual temperature/wind/RH at each historic hour instead of falling
/// back to synthesised neutral conditions.
pub async fn fetch_hourly_observations(
    client: &MetClient,
    source_id: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Vec<HourlyObservation>, ClientError> {
    let body = fetch_observations_body(client, source_id, &full_element_query(), from, to).await?;
    parse_hourly_observations(&body)
}

/// Shared HTTP plumbing for the two parsing variants. Keeps the URL
/// build, basic-auth, and error mapping in one place so the two public
/// functions only differ in *what* they ask for and *how* they parse it.
async fn fetch_observations_body(
    client: &MetClient,
    source_id: &str,
    elements: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<String, ClientError> {
    let client_id = client
        .config()
        .frost_client_id
        .as_ref()
        .ok_or(ClientError::MissingCredentials("frost_client_id"))?;

    let mut url = client.frost_url(PATH)?;
    url.query_pairs_mut()
        .clear()
        .append_pair("sources", source_id)
        .append_pair("elements", elements)
        .append_pair(
            "referencetime",
            &format!("{}/{}", from.to_rfc3339(), to.to_rfc3339()),
        );

    let resp = client
        .http()
        .get(url)
        .basic_auth(client_id, Some(""))
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().await.ok();
        return Err(ClientError::Http { status, body });
    }
    Ok(resp.text().await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use url::Url;
    use wiremock::matchers::{basic_auth, header, method, path as path_m, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::client::{MetClientConfig, UserAgent};

    const FIXTURE: &str = include_str!("../../../fixtures/frost_precip_oslo_48h.json");

    fn ua() -> UserAgent {
        UserAgent::new("grusindeks-test", "0.1", "dev@example.invalid").unwrap()
    }

    // ---- Pure parser ----

    #[test]
    fn parser_extracts_chronological_precip_series() {
        let series = parse_hourly_precip(FIXTURE).unwrap();
        // Fixture covers a full 48 h window of Oslo / Blindern data.
        assert_eq!(series.len(), 48);
        for w in series.windows(2) {
            assert!(w[0].time < w[1].time);
        }
        // Spot-check the rain-burst hours on the evening of the 24th —
        // these are the entries the older 4-element fixture exposed.
        let total_mm: f64 = series.iter().map(|p| p.mm).sum();
        assert!(
            (total_mm - 6.0).abs() < 1e-6,
            "total mm should sum to 6.0; got {total_mm}",
        );
        let max = series
            .iter()
            .max_by(|a, b| a.mm.partial_cmp(&b.mm).unwrap())
            .unwrap();
        assert_eq!(max.mm, 2.6, "peak rain hour should be 2.6 mm");
    }

    #[test]
    fn parser_skips_unknown_element_ids() {
        let body = r#"{
            "data": [
                {"referenceTime": "2026-04-25T00:00:00.000Z",
                 "observations": [
                    {"elementId": "air_temperature", "value": 12.0},
                    {"elementId": "sum(precipitation_amount PT1H)", "value": 0.5}
                 ]}
            ]
        }"#;
        let series = parse_hourly_precip(body).unwrap();
        assert_eq!(series.len(), 1);
        assert_eq!(series[0].mm, 0.5);
    }

    #[test]
    fn parser_handles_empty_data() {
        let body = r#"{"data": []}"#;
        assert!(parse_hourly_precip(body).unwrap().is_empty());
    }

    #[test]
    fn parser_returns_decode_error_on_garbage() {
        let err = parse_hourly_precip("not json").unwrap_err();
        matches!(err, ClientError::Decode(_));
    }

    // ---- HTTP integration ----

    fn cfg(server: &MockServer, with_client_id: bool) -> MetClientConfig {
        MetClientConfig {
            user_agent: ua(),
            api_base: Url::parse(&format!("{}/", server.uri())).unwrap(),
            frost_base: Url::parse(&format!("{}/", server.uri())).unwrap(),
            frost_client_id: with_client_id.then_some("test-client-id".into()),
            timeout: Duration::from_secs(5),
            cache_dir: None,
        }
    }

    #[tokio::test]
    async fn fetch_sends_basic_auth_and_correct_query() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path_m("/observations/v0.jsonld"))
            .and(query_param("sources", "SN18700"))
            .and(query_param("elements", ELEMENT_HOURLY_PRECIP))
            .and(header(
                "user-agent",
                "grusindeks-test/0.1 dev@example.invalid",
            ))
            .and(basic_auth("test-client-id", ""))
            .respond_with(ResponseTemplate::new(200).set_body_string(FIXTURE))
            .mount(&server)
            .await;

        let client = MetClient::new(cfg(&server, true)).unwrap();
        let from: DateTime<Utc> = "2026-04-24T10:00:00Z".parse().unwrap();
        let to: DateTime<Utc> = "2026-04-26T10:00:00Z".parse().unwrap();

        let series = fetch_hourly_precip(&client, "SN18700", from, to)
            .await
            .unwrap();
        assert_eq!(series.len(), 48);
    }

    #[tokio::test]
    async fn fetch_errors_without_client_id() {
        let server = MockServer::start().await;
        let client = MetClient::new(cfg(&server, false)).unwrap();
        let from: DateTime<Utc> = "2026-04-24T10:00:00Z".parse().unwrap();
        let to: DateTime<Utc> = "2026-04-26T10:00:00Z".parse().unwrap();

        let err = fetch_hourly_precip(&client, "SN18700", from, to)
            .await
            .unwrap_err();
        assert!(
            matches!(err, ClientError::MissingCredentials("frost_client_id")),
            "expected MissingCredentials, got {err:?}",
        );
    }

    // ---- Defensive parsing ----

    #[test]
    fn parser_drops_negative_and_nan_values() {
        // Frost occasionally surfaces sensor-error sentinels; the parser
        // must not let them reach the drying model.
        let body = r#"{
            "data": [
                {"referenceTime": "2026-04-25T00:00:00.000Z",
                 "observations": [
                    {"elementId": "sum(precipitation_amount PT1H)", "value": -99.9}
                 ]},
                {"referenceTime": "2026-04-25T01:00:00.000Z",
                 "observations": [
                    {"elementId": "sum(precipitation_amount PT1H)", "value": 1.2}
                 ]}
            ]
        }"#;
        let series = parse_hourly_precip(body).unwrap();
        assert_eq!(series.len(), 1, "negative sensor reading should be dropped");
        assert_eq!(series[0].mm, 1.2);
    }

    // ---- Multi-element parser ----

    #[test]
    fn observations_parser_groups_elements_per_hour() {
        let body = r#"{
            "data": [
                {"referenceTime": "2026-04-25T00:00:00.000Z",
                 "observations": [
                    {"elementId": "sum(precipitation_amount PT1H)", "value": 0.4},
                    {"elementId": "mean(air_temperature PT1H)",     "value": 9.5},
                    {"elementId": "mean(wind_speed PT1H)",          "value": 3.2},
                    {"elementId": "mean(relative_humidity PT1H)",   "value": 78.0},
                    {"elementId": "mean(cloud_area_fraction PT1H)", "value": 35.0}
                 ]},
                {"referenceTime": "2026-04-25T01:00:00.000Z",
                 "observations": [
                    {"elementId": "sum(precipitation_amount PT1H)", "value": 0.0},
                    {"elementId": "mean(air_temperature PT1H)",     "value": 9.0}
                 ]}
            ]
        }"#;
        let series = parse_hourly_observations(body).unwrap();
        assert_eq!(series.len(), 2);
        let first = &series[0];
        assert_eq!(first.precip_mm, Some(0.4));
        assert_eq!(first.temp_c, Some(9.5));
        assert_eq!(first.wind_ms, Some(3.2));
        assert_eq!(first.humidity_pct, Some(78.0));
        assert_eq!(first.cloud_pct, Some(35.0));
        let second = &series[1];
        assert_eq!(second.precip_mm, Some(0.0));
        assert_eq!(second.temp_c, Some(9.0));
        assert_eq!(second.wind_ms, None, "missing wind must stay None");
        assert_eq!(second.humidity_pct, None);
        assert_eq!(second.cloud_pct, None, "missing cloud must stay None");
    }

    #[test]
    fn observations_parser_drops_out_of_domain_values() {
        let body = r#"{
            "data": [{
                "referenceTime": "2026-04-25T00:00:00.000Z",
                "observations": [
                    {"elementId": "sum(precipitation_amount PT1H)", "value": -10.0},
                    {"elementId": "mean(wind_speed PT1H)",          "value": -2.0},
                    {"elementId": "mean(relative_humidity PT1H)",   "value": 250.0},
                    {"elementId": "mean(cloud_area_fraction PT1H)", "value": 150.0}
                ]
            }]
        }"#;
        let obs = &parse_hourly_observations(body).unwrap()[0];
        assert_eq!(obs.precip_mm, None, "negative precip dropped");
        assert_eq!(obs.wind_ms, None, "negative wind dropped");
        assert_eq!(obs.humidity_pct, None, "RH > 100 dropped");
        assert_eq!(obs.cloud_pct, None, "cloud > 100 dropped");
    }

    #[tokio::test]
    async fn fetch_observations_requests_all_five_elements() {
        let server = MockServer::start().await;
        let body = r#"{
            "data": [{
                "referenceTime": "2026-04-25T00:00:00.000Z",
                "observations": [
                    {"elementId": "sum(precipitation_amount PT1H)", "value": 0.4},
                    {"elementId": "mean(air_temperature PT1H)",     "value": 9.5}
                ]
            }]
        }"#;

        // Pin every element name in the query string. If we forget one in
        // production, the mock returns 404 and the test fails loudly.
        Mock::given(method("GET"))
            .and(path_m("/observations/v0.jsonld"))
            .and(query_param(
                "elements",
                "sum(precipitation_amount PT1H),mean(air_temperature PT1H),mean(wind_speed PT1H),mean(relative_humidity PT1H),mean(cloud_area_fraction PT1H)",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;

        let client = MetClient::new(cfg(&server, true)).unwrap();
        let from: DateTime<Utc> = "2026-04-24T10:00:00Z".parse().unwrap();
        let to: DateTime<Utc> = "2026-04-26T10:00:00Z".parse().unwrap();
        let series = fetch_hourly_observations(&client, "SN18700", from, to)
            .await
            .unwrap();
        assert_eq!(series.len(), 1);
        assert_eq!(series[0].temp_c, Some(9.5));
    }

    #[test]
    fn parser_sorts_out_of_order_input() {
        // Verifies the sort step actually does something — fixture data
        // is naturally ordered, so this exercises the inverse.
        let body = r#"{
            "data": [
                {"referenceTime": "2026-04-25T03:00:00.000Z",
                 "observations": [
                    {"elementId": "sum(precipitation_amount PT1H)", "value": 0.5}
                 ]},
                {"referenceTime": "2026-04-25T01:00:00.000Z",
                 "observations": [
                    {"elementId": "sum(precipitation_amount PT1H)", "value": 0.2}
                 ]},
                {"referenceTime": "2026-04-25T02:00:00.000Z",
                 "observations": [
                    {"elementId": "sum(precipitation_amount PT1H)", "value": 0.3}
                 ]}
            ]
        }"#;
        let series = parse_hourly_precip(body).unwrap();
        let times: Vec<_> = series.iter().map(|p| p.time).collect();
        assert!(
            times.windows(2).all(|w| w[0] < w[1]),
            "expected ascending order, got {times:?}",
        );
        assert_eq!(series[0].mm, 0.2);
        assert_eq!(series[1].mm, 0.3);
        assert_eq!(series[2].mm, 0.5);
    }

    #[tokio::test]
    async fn fetch_surfaces_unauthorized() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_m("/observations/v0.jsonld"))
            .respond_with(ResponseTemplate::new(401).set_body_string(r#"{"error":{"code":401}}"#))
            .mount(&server)
            .await;

        let client = MetClient::new(cfg(&server, true)).unwrap();
        let from: DateTime<Utc> = "2026-04-24T10:00:00Z".parse().unwrap();
        let to: DateTime<Utc> = "2026-04-26T10:00:00Z".parse().unwrap();

        let err = fetch_hourly_precip(&client, "SN18700", from, to)
            .await
            .unwrap_err();
        match err {
            ClientError::Http { status: 401, .. } => {}
            other => panic!("expected 401, got {other:?}"),
        }
    }
}
