//! `api.met.no/weatherapi/metalerts/2.0` — official MET weather warnings
//! (the CAP alerts yr renders as "Pågår: Mye lyn" banners).
//!
//! We hit the `all.json` variant (current *and* upcoming alerts) filtered by
//! lat/lon server-side, so the response only contains alerts whose polygon
//! covers the requested point — no geometry handling needed here.

use chrono::{DateTime, Utc};
use grusindeks_core::aggregate::{AlertLevel, WeatherAlert};
use grusindeks_core::geo::Point;
use serde::Deserialize;

use crate::client::{ClientError, MetClient};

const PATH: &str = "/weatherapi/metalerts/2.0/all.json";

#[derive(Debug, Deserialize)]
struct AlertsResponse {
    #[serde(default)]
    features: Vec<AlertFeature>,
}

#[derive(Debug, Deserialize)]
struct AlertFeature {
    properties: AlertProps,
    /// `when.interval` = `[start, end]`, a sibling of `properties` in the
    /// MetAlerts GeoJSON dialect.
    #[serde(default)]
    when: Option<AlertWhen>,
}

#[derive(Debug, Deserialize)]
struct AlertWhen {
    #[serde(default)]
    interval: Vec<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AlertProps {
    /// CAP identifier, e.g. `2.49.0.1.578.0.20260610080652.055` — the
    /// 14-digit segment is the issue timestamp.
    #[serde(default)]
    id: Option<String>,
    /// CAP msgType: "Alert" | "Update" | "Cancel". The feed has no
    /// `references` field, so supersession is inferred from this plus the
    /// issue timestamp (see [`parse`]).
    #[serde(default, rename = "type")]
    msg_type: Option<String>,
    #[serde(default)]
    event: String,
    #[serde(default)]
    event_awareness_name: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: String,
    #[serde(default)]
    consequences: Option<String>,
    #[serde(default)]
    instruction: Option<String>,
    #[serde(default)]
    area: String,
    /// "Yellow" | "Orange" | "Red" — the level yr displays. Preferred over
    /// CAP `severity`, which uses a different scale.
    #[serde(default)]
    risk_matrix_color: Option<String>,
    #[serde(default)]
    severity: Option<String>,
}

fn parse_level(props: &AlertProps) -> AlertLevel {
    if let Some(c) = props.risk_matrix_color.as_deref() {
        match c.to_ascii_lowercase().as_str() {
            "red" => return AlertLevel::Red,
            "orange" => return AlertLevel::Orange,
            "yellow" => return AlertLevel::Yellow,
            _ => {}
        }
    }
    // CAP severity fallback for features missing the colour.
    match props.severity.as_deref() {
        Some("Extreme") => AlertLevel::Red,
        Some("Severe") => AlertLevel::Orange,
        _ => AlertLevel::Yellow,
    }
}

/// The CAP issue timestamp embedded in an alert id
/// (`2.49.0.1.578.0.20260610080652.055` → `20260610080652`). It's the only
/// long numeric segment; `0` when the id is missing or oddly shaped.
fn issue_stamp(id: &str) -> u64 {
    id.split('.')
        .filter(|s| s.len() >= 12)
        .filter_map(|s| s.parse().ok())
        .max()
        .unwrap_or(0)
}

/// One parsed feature plus the CAP metadata that decides which message wins
/// when several describe the same warning episode.
struct Candidate {
    alert: WeatherAlert,
    cancel: bool,
    /// Newer messages outrank older: issue timestamp, then Update over
    /// Alert, then feed order as a deterministic tie-break.
    rank: (u64, u8, usize),
}

/// Pure parser. Features without a usable time interval are skipped (an
/// alert we can't place in time can't be shown honestly).
///
/// `all.json` keeps the whole message chain for a warning — the original
/// `Alert` *and* later `Update`s coexist (and the feed carries no
/// `references` to link them), which showed up as duplicate chips in the UI.
/// Messages for the same event + area with overlapping intervals are treated
/// as one episode: only the newest message survives, and a `Cancel` kills
/// the episode entirely. The result is sorted worst-level first, then by
/// start time.
pub fn parse(body: &str) -> Result<Vec<WeatherAlert>, ClientError> {
    let raw: AlertsResponse =
        serde_json::from_str(body).map_err(|e| ClientError::Decode(e.to_string()))?;
    let candidates: Vec<Candidate> = raw
        .features
        .into_iter()
        .enumerate()
        .filter_map(|(i, f)| {
            let interval = f.when.as_ref()?.interval.as_slice();
            let (&starts, &ends) = match interval {
                [s, e] => (s, e),
                _ => return None,
            };
            if ends <= starts {
                return None;
            }
            let level = parse_level(&f.properties);
            let p = f.properties;
            let issued = p.id.as_deref().map_or(0, issue_stamp);
            let msg_type = p.msg_type.as_deref().unwrap_or("Alert");
            let event_name = p
                .event_awareness_name
                .or(p.title)
                .unwrap_or_else(|| p.event.clone());
            Some(Candidate {
                cancel: msg_type.eq_ignore_ascii_case("cancel"),
                rank: (issued, msg_type.eq_ignore_ascii_case("update") as u8, i),
                alert: WeatherAlert {
                    event: p.event,
                    event_name,
                    level,
                    description: p.description,
                    consequences: p.consequences.filter(|s| !s.trim().is_empty()),
                    instruction: p.instruction.filter(|s| !s.trim().is_empty()),
                    area: p.area,
                    starts,
                    ends,
                },
            })
        })
        .collect();
    let superseded = |me: &Candidate| {
        candidates.iter().any(|o| {
            o.rank > me.rank
                && o.alert.event == me.alert.event
                && o.alert.area == me.alert.area
                && o.alert.overlaps(me.alert.starts, me.alert.ends)
        })
    };
    let mut alerts: Vec<WeatherAlert> = candidates
        .iter()
        .filter(|c| !c.cancel && !superseded(c))
        .map(|c| c.alert.clone())
        .collect();
    alerts.sort_by(|a, b| b.level.cmp(&a.level).then(a.starts.cmp(&b.starts)));
    Ok(alerts)
}

/// Fetch every current/upcoming warning covering `point`. Coordinates are
/// truncated to 4 decimals (TOS). Goes through [`MetClient::fetch_text`] so
/// the disk cache (when configured) revalidates with `If-Modified-Since`.
pub async fn fetch(client: &MetClient, point: Point) -> Result<Vec<WeatherAlert>, ClientError> {
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

    fn feature_msg(
        level_color: &str,
        start: &str,
        end: &str,
        id: &str,
        msg_type: &str,
    ) -> String {
        format!(
            r#"{{
                "properties": {{
                    "id": "{id}",
                    "type": "{msg_type}",
                    "event": "lightning",
                    "eventAwarenessName": "Mye lyn",
                    "title": "Mye lyn, gult nivå, Deler av Østlandet",
                    "description": "Det er fare for tordenvær med mye lyn.",
                    "consequences": "Lynnedslag kan føre til brann.",
                    "instruction": "Unngå utendørsaktiviteter på utsatte områder.",
                    "area": "Deler av Østlandet",
                    "riskMatrixColor": "{level_color}",
                    "severity": "Moderate"
                }},
                "when": {{ "interval": ["{start}", "{end}"] }}
            }}"#
        )
    }

    fn feature(level_color: &str, start: &str, end: &str) -> String {
        feature_msg(level_color, start, end, "", "Alert")
    }

    fn collection(features: &[String]) -> String {
        format!(
            r#"{{ "type": "FeatureCollection", "features": [{}] }}"#,
            features.join(",")
        )
    }

    #[test]
    fn parser_maps_cap_fields() {
        let body = collection(&[feature(
            "Yellow",
            "2026-06-10T13:00:00+00:00",
            "2026-06-10T21:00:00+00:00",
        )]);
        let alerts = parse(&body).unwrap();
        assert_eq!(alerts.len(), 1);
        let a = &alerts[0];
        assert_eq!(a.event, "lightning");
        assert_eq!(a.event_name, "Mye lyn");
        assert_eq!(a.level, AlertLevel::Yellow);
        assert_eq!(a.area, "Deler av Østlandet");
        assert!(a.consequences.is_some());
        assert!(a.instruction.is_some());
        assert_eq!(a.starts, Utc.with_ymd_and_hms(2026, 6, 10, 13, 0, 0).unwrap());
        assert_eq!(a.ends, Utc.with_ymd_and_hms(2026, 6, 10, 21, 0, 0).unwrap());
    }

    #[test]
    fn parser_sorts_worst_level_first() {
        let body = collection(&[
            feature("Yellow", "2026-06-10T10:00:00Z", "2026-06-10T12:00:00Z"),
            feature("Red", "2026-06-11T10:00:00Z", "2026-06-11T12:00:00Z"),
            feature("Orange", "2026-06-10T08:00:00Z", "2026-06-10T09:00:00Z"),
        ]);
        let alerts = parse(&body).unwrap();
        let levels: Vec<AlertLevel> = alerts.iter().map(|a| a.level).collect();
        assert_eq!(
            levels,
            vec![AlertLevel::Red, AlertLevel::Orange, AlertLevel::Yellow]
        );
    }

    #[test]
    fn parser_skips_features_without_interval() {
        let body = r#"{
            "type": "FeatureCollection",
            "features": [
                { "properties": { "event": "wind", "description": "", "area": "" } },
                { "properties": { "event": "wind", "description": "", "area": "" },
                  "when": { "interval": ["2026-06-10T10:00:00Z"] } }
            ]
        }"#;
        assert!(parse(body).unwrap().is_empty());
    }

    #[test]
    fn parser_handles_empty_collection() {
        assert!(parse(r#"{ "type": "FeatureCollection", "features": [] }"#)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn update_supersedes_overlapping_alert() {
        // Real-world shape from the live feed: all.json keeps the whole
        // message chain, so the original Alert and the later Update for the
        // same event + area coexist — only the Update may surface.
        let body = collection(&[
            feature_msg(
                "Yellow",
                "2026-06-10T12:00:00Z",
                "2026-06-10T17:00:00Z",
                "2.49.0.1.578.0.20260609073947.006",
                "Alert",
            ),
            feature_msg(
                "Yellow",
                "2026-06-10T13:00:00Z",
                "2026-06-10T21:00:00Z",
                "2.49.0.1.578.0.20260610080652.055",
                "Update",
            ),
        ]);
        let alerts = parse(&body).unwrap();
        assert_eq!(alerts.len(), 1);
        assert_eq!(
            alerts[0].ends,
            Utc.with_ymd_and_hms(2026, 6, 10, 21, 0, 0).unwrap()
        );
    }

    #[test]
    fn distinct_episodes_are_both_kept() {
        // Same event + area but disjoint periods = two separate warnings
        // (e.g. lightning today and again in three days), not an update.
        let body = collection(&[
            feature_msg(
                "Yellow",
                "2026-06-10T12:00:00Z",
                "2026-06-10T17:00:00Z",
                "2.49.0.1.578.0.20260609073947.006",
                "Alert",
            ),
            feature_msg(
                "Yellow",
                "2026-06-13T10:00:00Z",
                "2026-06-13T18:00:00Z",
                "2.49.0.1.578.0.20260610090000.001",
                "Alert",
            ),
        ]);
        assert_eq!(parse(&body).unwrap().len(), 2);
    }

    #[test]
    fn cancel_kills_the_episode() {
        let body = collection(&[
            feature_msg(
                "Yellow",
                "2026-06-10T12:00:00Z",
                "2026-06-10T17:00:00Z",
                "2.49.0.1.578.0.20260609073947.006",
                "Alert",
            ),
            feature_msg(
                "Yellow",
                "2026-06-10T12:00:00Z",
                "2026-06-10T17:00:00Z",
                "2.49.0.1.578.0.20260610100000.002",
                "Cancel",
            ),
        ]);
        assert!(parse(&body).unwrap().is_empty());
    }

    #[test]
    fn level_falls_back_to_cap_severity() {
        let body = collection(&[feature("", "2026-06-10T10:00:00Z", "2026-06-10T12:00:00Z")])
            .replace(r#""riskMatrixColor": "","#, "")
            .replace(r#""severity": "Moderate""#, r#""severity": "Extreme""#);
        let alerts = parse(&body).unwrap();
        assert_eq!(alerts[0].level, AlertLevel::Red);
    }
}
