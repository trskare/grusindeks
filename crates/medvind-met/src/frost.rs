//! `frost.met.no/observations/v0.jsonld` — historical observations.
//!
//! For the drying model we only need hourly precipitation totals. Frost
//! returns one `data` entry per `referenceTime`, each holding a list of
//! `observations`. We flatten that to a chronological `Vec<HourlyPrecip>`
//! that the drying model can replay.
//!
//! Authentication is HTTP Basic with `client_id` as the username and an
//! empty password. Without a configured client_id this module returns
//! `Error::MissingCredentials` so the CLI can fall back to "assume dry".

use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::client::{ClientError, MetClient};

const PATH: &str = "/observations/v0.jsonld";
pub const ELEMENT_HOURLY_PRECIP: &str = "sum(precipitation_amount PT1H)";

/// One observed hour of precipitation. The `time` is the *end* of the
/// integration window per the upstream `referenceTime` semantics.
#[derive(Debug, Clone, PartialEq)]
pub struct HourlyPrecip {
    pub time: DateTime<Utc>,
    pub mm: f64,
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
/// precipitation element are ignored.
pub fn parse_hourly_precip(body: &str) -> Result<Vec<HourlyPrecip>, ClientError> {
    let raw: ObsResponse =
        serde_json::from_str(body).map_err(|e| ClientError::Decode(e.to_string()))?;
    let mut out: Vec<HourlyPrecip> = raw
        .data
        .into_iter()
        .flat_map(|entry| {
            let t = entry.reference_time;
            entry.observations.into_iter().filter_map(move |o| {
                (o.element_id == ELEMENT_HOURLY_PRECIP).then_some(HourlyPrecip {
                    time: t,
                    mm: o.value,
                })
            })
        })
        .collect();
    out.sort_by_key(|p| p.time);
    Ok(out)
}

/// Fetch hourly precipitation observations for `source_id` (e.g.
/// `"SN18700"`) over `[from, to)`. Requires `frost_client_id` in the
/// client config; returns `ClientError::InvalidUserAgent` (re-purposed
/// as a config error) if missing — easier than introducing a new variant
/// just for this case.
pub async fn fetch_hourly_precip(
    client: &MetClient,
    source_id: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Vec<HourlyPrecip>, ClientError> {
    let client_id =
        client
            .config()
            .frost_client_id
            .as_ref()
            .ok_or(ClientError::InvalidUserAgent(
                "frost_client_id not configured",
            ))?;

    let mut url = client.frost_url(PATH)?;
    url.query_pairs_mut()
        .clear()
        .append_pair("sources", source_id)
        .append_pair("elements", ELEMENT_HOURLY_PRECIP)
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
    let body = resp.text().await?;
    parse_hourly_precip(&body)
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
        UserAgent::new("medvind-test", "0.1", "dev@example.invalid").unwrap()
    }

    // ---- Pure parser ----

    #[test]
    fn parser_extracts_chronological_precip_series() {
        let series = parse_hourly_precip(FIXTURE).unwrap();
        assert_eq!(series.len(), 4);
        // Sorted ascending and values match the fixture.
        assert_eq!(series[0].mm, 1.4);
        assert_eq!(series[1].mm, 2.6);
        assert_eq!(series[2].mm, 0.0);
        assert_eq!(series[3].mm, 0.1);
        for w in series.windows(2) {
            assert!(w[0].time < w[1].time);
        }
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
        }
    }

    #[tokio::test]
    async fn fetch_sends_basic_auth_and_correct_query() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path_m("/observations/v0.jsonld"))
            .and(query_param("sources", "SN18700"))
            .and(query_param("elements", ELEMENT_HOURLY_PRECIP))
            .and(header("user-agent", "medvind-test/0.1 dev@example.invalid"))
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
        assert_eq!(series.len(), 4);
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
        matches!(err, ClientError::InvalidUserAgent(_));
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
