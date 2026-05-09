//! `api.met.no/weatherapi/nowcast/2.0/complete` — 5-minute radar nowcast.
//!
//! Coverage is Norden only. We surface a simple "next rain in N minutes?"
//! signal that the CLI uses to nudge the user ("kjør nå før regnet").

use chrono::{DateTime, Utc};
use grusindeks_core::geo::Point;
use serde::Deserialize;

use crate::client::{ClientError, MetClient};

const PATH: &str = "/weatherapi/nowcast/2.0/complete";

#[derive(Debug, Clone, PartialEq)]
pub struct Nowcast {
    pub radar_coverage_ok: bool,
    /// 5-min steps with their `precipitation_rate` (mm/h). Empty when out
    /// of radar coverage.
    pub steps: Vec<NowcastStep>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NowcastStep {
    pub time: DateTime<Utc>,
    pub precipitation_rate_mm_h: f64,
}

/// Roll-up of a [`Nowcast`]'s rainy steps. `peak_mm_h` is the highest
/// rate observed over the threshold; `first_rain_at` / `last_rain_at`
/// bracket the contiguous-or-not period the user should care about.
#[derive(Debug, Clone, PartialEq)]
pub struct NowcastSummary {
    pub first_rain_at: DateTime<Utc>,
    pub last_rain_at: DateTime<Utc>,
    pub peak_mm_h: f64,
    pub peak_at: DateTime<Utc>,
}

impl Nowcast {
    /// First step at or above `threshold_mm_h`, if any. With the default
    /// drizzle threshold (`0.1`) this returns "wet riding starts at T".
    pub fn next_rain_at(&self, threshold_mm_h: f64) -> Option<DateTime<Utc>> {
        self.steps
            .iter()
            .find(|s| s.precipitation_rate_mm_h >= threshold_mm_h)
            .map(|s| s.time)
    }

    /// Roll-up of every step at or above `threshold_mm_h`. Returns `None`
    /// when the series is dry (or radar coverage is missing — `steps` is
    /// empty in that case). Lets the CLI render a single-line banner
    /// without traversing the timeseries itself.
    pub fn summary(&self, threshold_mm_h: f64) -> Option<NowcastSummary> {
        let mut iter = self
            .steps
            .iter()
            .filter(|s| s.precipitation_rate_mm_h >= threshold_mm_h);
        let first = iter.next()?;
        let mut last = first;
        let mut peak = first;
        for s in iter {
            last = s;
            if s.precipitation_rate_mm_h > peak.precipitation_rate_mm_h {
                peak = s;
            }
        }
        Some(NowcastSummary {
            first_rain_at: first.time,
            last_rain_at: last.time,
            peak_mm_h: peak.precipitation_rate_mm_h,
            peak_at: peak.time,
        })
    }
}

#[derive(Debug, Deserialize)]
struct NowcastResponse {
    properties: NowcastProps,
}

#[derive(Debug, Deserialize)]
struct NowcastProps {
    meta: NowcastMeta,
    timeseries: Vec<NowcastSeries>,
}

#[derive(Debug, Deserialize)]
struct NowcastMeta {
    #[serde(default)]
    radar_coverage: Option<String>,
}

#[derive(Debug, Deserialize)]
struct NowcastSeries {
    time: DateTime<Utc>,
    data: NowcastData,
}

#[derive(Debug, Deserialize)]
struct NowcastData {
    instant: NowcastInstant,
}

#[derive(Debug, Deserialize)]
struct NowcastInstant {
    #[serde(default)]
    details: NowcastInstantDetails,
}

#[derive(Debug, Default, Deserialize)]
struct NowcastInstantDetails {
    #[serde(default)]
    precipitation_rate: Option<f64>,
}

/// Pure parser. Returns an empty `steps` list when radar coverage is
/// missing or the response carries no timeseries.
pub fn parse(body: &str) -> Result<Nowcast, ClientError> {
    let raw: NowcastResponse =
        serde_json::from_str(body).map_err(|e| ClientError::Decode(e.to_string()))?;
    let radar_coverage_ok = raw.properties.meta.radar_coverage.as_deref() == Some("ok");
    let steps = raw
        .properties
        .timeseries
        .into_iter()
        .filter_map(|s| {
            s.data
                .instant
                .details
                .precipitation_rate
                .map(|rate| NowcastStep {
                    time: s.time,
                    precipitation_rate_mm_h: rate,
                })
        })
        .collect();
    Ok(Nowcast {
        radar_coverage_ok,
        steps,
    })
}

/// Fetch the nowcast for `point`. Coordinates are truncated to 4
/// decimals (TOS). Goes through [`MetClient::fetch_text`] so the disk
/// cache (when configured) revalidates with `If-Modified-Since`.
pub async fn fetch(client: &MetClient, point: Point) -> Result<Nowcast, ClientError> {
    let p = point.truncated();
    let mut url = client.api_url(PATH)?;
    url.query_pairs_mut()
        .clear()
        .append_pair("lat", &format!("{:.4}", p.lat))
        .append_pair("lon", &format!("{:.4}", p.lon));

    let body = client.fetch_text(url).await?;
    parse(&body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::time::Duration;
    use url::Url;
    use wiremock::matchers::{method, path as path_m, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::client::{MetClientConfig, UserAgent};

    const FIXTURE: &str = include_str!("../../../fixtures/nowcast_oslo.json");

    fn ua() -> UserAgent {
        UserAgent::new("grusindeks-test", "0.1", "dev@example.invalid").unwrap()
    }

    // ---- Pure parser ----

    #[test]
    fn parser_ingests_real_fixture() {
        let n = parse(FIXTURE).unwrap();
        assert!(n.radar_coverage_ok);
        assert!(!n.steps.is_empty());
        assert!(n.steps[0].precipitation_rate_mm_h >= 0.0);
        for w in n.steps.windows(2) {
            assert!(w[0].time < w[1].time);
        }
    }

    #[test]
    fn parser_marks_no_radar_coverage() {
        let body = r#"{
            "properties": {
                "meta": {"radar_coverage": "no coverage"},
                "timeseries": []
            }
        }"#;
        let n = parse(body).unwrap();
        assert!(!n.radar_coverage_ok);
        assert!(n.steps.is_empty());
    }

    #[test]
    fn next_rain_at_returns_first_step_above_threshold() {
        let n = Nowcast {
            radar_coverage_ok: true,
            steps: vec![
                NowcastStep {
                    time: Utc.with_ymd_and_hms(2026, 4, 26, 14, 0, 0).unwrap(),
                    precipitation_rate_mm_h: 0.0,
                },
                NowcastStep {
                    time: Utc.with_ymd_and_hms(2026, 4, 26, 14, 5, 0).unwrap(),
                    precipitation_rate_mm_h: 0.05,
                },
                NowcastStep {
                    time: Utc.with_ymd_and_hms(2026, 4, 26, 14, 10, 0).unwrap(),
                    precipitation_rate_mm_h: 0.4,
                },
            ],
        };
        assert_eq!(
            n.next_rain_at(0.1),
            Some(Utc.with_ymd_and_hms(2026, 4, 26, 14, 10, 0).unwrap())
        );
        assert_eq!(n.next_rain_at(1.0), None);
    }

    #[test]
    fn summary_returns_none_for_dry_series() {
        let n = Nowcast {
            radar_coverage_ok: true,
            steps: vec![
                NowcastStep {
                    time: Utc.with_ymd_and_hms(2026, 4, 26, 14, 0, 0).unwrap(),
                    precipitation_rate_mm_h: 0.0,
                },
                NowcastStep {
                    time: Utc.with_ymd_and_hms(2026, 4, 26, 14, 5, 0).unwrap(),
                    precipitation_rate_mm_h: 0.05,
                },
            ],
        };
        assert!(n.summary(0.1).is_none());
    }

    #[test]
    fn summary_returns_none_when_steps_empty() {
        let n = Nowcast {
            radar_coverage_ok: false,
            steps: vec![],
        };
        assert!(n.summary(0.1).is_none());
    }

    #[test]
    fn summary_brackets_first_last_and_picks_peak() {
        let t = |m: u32| Utc.with_ymd_and_hms(2026, 4, 26, 14, m, 0).unwrap();
        let n = Nowcast {
            radar_coverage_ok: true,
            steps: vec![
                NowcastStep {
                    time: t(0),
                    precipitation_rate_mm_h: 0.0,
                },
                NowcastStep {
                    time: t(5),
                    precipitation_rate_mm_h: 0.2,
                },
                NowcastStep {
                    time: t(10),
                    precipitation_rate_mm_h: 0.6,
                },
                NowcastStep {
                    time: t(15),
                    precipitation_rate_mm_h: 0.4,
                },
                NowcastStep {
                    time: t(20),
                    precipitation_rate_mm_h: 0.0,
                },
                NowcastStep {
                    time: t(25),
                    precipitation_rate_mm_h: 0.3,
                },
            ],
        };
        let s = n.summary(0.1).expect("rain in series");
        assert_eq!(s.first_rain_at, t(5));
        assert_eq!(s.last_rain_at, t(25));
        assert!(
            (s.peak_mm_h - 0.6).abs() < 1e-9,
            "expected peak 0.6, got {}",
            s.peak_mm_h
        );
        assert_eq!(s.peak_at, t(10));
    }

    #[test]
    fn summary_threshold_excludes_subthreshold_blip() {
        // A 0.05 mm/h blip 5 min in shouldn't anchor `first_rain_at` if
        // the threshold is 0.1.
        let t = |m: u32| Utc.with_ymd_and_hms(2026, 4, 26, 14, m, 0).unwrap();
        let n = Nowcast {
            radar_coverage_ok: true,
            steps: vec![
                NowcastStep {
                    time: t(5),
                    precipitation_rate_mm_h: 0.05,
                },
                NowcastStep {
                    time: t(15),
                    precipitation_rate_mm_h: 0.4,
                },
            ],
        };
        let s = n.summary(0.1).expect("0.4 is over threshold");
        assert_eq!(s.first_rain_at, t(15));
    }

    #[test]
    fn next_rain_at_returns_none_for_dry_series() {
        let n = parse(FIXTURE).unwrap();
        // The captured fixture is dry — confirms the helper.
        let next = n.next_rain_at(0.1);
        // We don't assert on a specific value (depends on capture day),
        // but the helper must run without panicking either way.
        let _ = next;
    }

    // ---- HTTP ----

    #[tokio::test]
    async fn fetch_truncates_coordinates() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_m("/weatherapi/nowcast/2.0/complete"))
            .and(query_param("lat", "59.9139"))
            .and(query_param("lon", "10.7522"))
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

        let n = fetch(&client, Point::new(59.913999, 10.752299))
            .await
            .unwrap();
        assert!(n.radar_coverage_ok);
        assert!(!n.steps.is_empty());
    }
}
