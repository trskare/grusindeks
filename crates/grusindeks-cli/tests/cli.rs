//! End-to-end tests for the `grusindeks` binary.
//!
//! Each test stands up a local `wiremock` server, points the binary at it
//! via `GRUSINDEKS_API_BASE` / `GRUSINDEKS_FROST_BASE`, and uses `assert_cmd`
//! to drive the CLI. No real network traffic.

use std::path::PathBuf;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;
use wiremock::matchers::{method, path as path_m};
use wiremock::{Mock, MockServer, ResponseTemplate};

const LOCATIONFORECAST_FIXTURE: &str = include_str!("../../../fixtures/locationforecast_oslo.json");

fn write_config(dir: &TempDir, body: &str) -> PathBuf {
    let p = dir.path().join("config.toml");
    std::fs::write(&p, body).unwrap();
    p
}

#[test]
fn help_works() {
    Command::cargo_bin("grusindeks")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Grusindeks"));
}

#[test]
fn config_init_writes_template_to_custom_path() {
    let dir = TempDir::new().unwrap();
    let cfg = dir.path().join("config.toml");
    Command::cargo_bin("grusindeks")
        .unwrap()
        .arg("--config")
        .arg(&cfg)
        .args(["config", "init"])
        .assert()
        .success();
    let body = std::fs::read_to_string(&cfg).unwrap();
    assert!(body.contains("user_agent_contact"));
    assert!(body.contains("[places.oslo]"));
}

#[test]
fn config_init_refuses_to_overwrite() {
    let dir = TempDir::new().unwrap();
    let cfg = write_config(&dir, "user_agent_contact = \"x@y.io\"\n");
    Command::cargo_bin("grusindeks")
        .unwrap()
        .arg("--config")
        .arg(&cfg)
        .args(["config", "init"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists"));
}

#[test]
fn score_errors_with_helpful_message_when_config_missing() {
    let dir = TempDir::new().unwrap();
    let cfg = dir.path().join("does-not-exist.toml");
    Command::cargo_bin("grusindeks")
        .unwrap()
        .arg("--config")
        .arg(&cfg)
        .args(["--lat", "59.9139", "--lon", "10.7522"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("grusindeks config init"));
}

#[tokio::test]
async fn score_against_mocked_api_emits_grusindeks_text() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_m("/weatherapi/locationforecast/2.0/complete"))
        .respond_with(ResponseTemplate::new(200).set_body_string(LOCATIONFORECAST_FIXTURE))
        .mount(&server)
        .await;

    let dir = TempDir::new().unwrap();
    let cfg = write_config(
        &dir,
        r#"user_agent_contact = "dev@example.invalid"
[places.oslo]
lat = 59.9139
lon = 10.7522
radius_km = 20.0
"#,
    );

    Command::cargo_bin("grusindeks")
        .unwrap()
        .env("GRUSINDEKS_API_BASE", format!("{}/", server.uri()))
        .env("GRUSINDEKS_FROST_BASE", format!("{}/", server.uri()))
        .arg("--config")
        .arg(&cfg)
        .args(["--place", "oslo", "--hours", "3"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Grusindeks for oslo"))
        .stdout(predicate::str::contains("Total:"))
        .stdout(predicate::str::contains("Temperatur"));
}

#[tokio::test]
async fn score_json_output_is_valid_json() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_m("/weatherapi/locationforecast/2.0/complete"))
        .respond_with(ResponseTemplate::new(200).set_body_string(LOCATIONFORECAST_FIXTURE))
        .mount(&server)
        .await;

    let dir = TempDir::new().unwrap();
    let cfg = write_config(&dir, "user_agent_contact = \"dev@example.invalid\"\n");

    let out = Command::cargo_bin("grusindeks")
        .unwrap()
        .env("GRUSINDEKS_API_BASE", format!("{}/", server.uri()))
        .env("GRUSINDEKS_FROST_BASE", format!("{}/", server.uri()))
        .arg("--config")
        .arg(&cfg)
        .arg("--json")
        // --hours forces single-day mode; the default is now the
        // six-day forecast which yields a different JSON shape.
        .args(["--lat", "59.9139", "--lon", "10.7522", "--hours", "3"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let parsed: serde_json::Value = serde_json::from_slice(&out).expect("output must be JSON");
    assert!(parsed.get("location").is_some(), "json missing 'location'");
    assert!(
        parsed.get("aggregate").is_some(),
        "json missing 'aggregate'"
    );
    let total = &parsed["aggregate"]["mean"];
    assert!(total.is_u64(), "aggregate.mean must be a number");
}

#[tokio::test]
async fn score_with_days_emits_multi_day_view() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_m("/weatherapi/locationforecast/2.0/complete"))
        .respond_with(ResponseTemplate::new(200).set_body_string(LOCATIONFORECAST_FIXTURE))
        .mount(&server)
        .await;

    let dir = TempDir::new().unwrap();
    let cfg = write_config(
        &dir,
        r#"user_agent_contact = "dev@example.invalid"
[places.oslo]
lat = 59.9139
lon = 10.7522
radius_km = 20.0
"#,
    );

    Command::cargo_bin("grusindeks")
        .unwrap()
        .env("GRUSINDEKS_API_BASE", format!("{}/", server.uri()))
        .env("GRUSINDEKS_FROST_BASE", format!("{}/", server.uri()))
        .arg("--config")
        .arg(&cfg)
        .args(["--place", "oslo", "--days", "5"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Grusindeks · oslo"))
        .stdout(predicate::str::contains("dager"))
        // The bucket legend chip lives once in the footer; pin against
        // it instead of the old per-row "ⓘ" glyph that has been removed.
        .stdout(predicate::str::contains("Skala"));
}

#[tokio::test]
async fn score_with_hourly_emits_hour_grid_with_header() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_m("/weatherapi/locationforecast/2.0/complete"))
        .respond_with(ResponseTemplate::new(200).set_body_string(LOCATIONFORECAST_FIXTURE))
        .mount(&server)
        .await;

    let dir = TempDir::new().unwrap();
    let cfg = write_config(
        &dir,
        r#"user_agent_contact = "dev@example.invalid"
[places.oslo]
lat = 59.9139
lon = 10.7522
radius_km = 20.0
"#,
    );

    Command::cargo_bin("grusindeks")
        .unwrap()
        .env("GRUSINDEKS_API_BASE", format!("{}/", server.uri()))
        .env("GRUSINDEKS_FROST_BASE", format!("{}/", server.uri()))
        .arg("--config")
        .arg(&cfg)
        .args(["--place", "oslo", "--days", "2", "--hourly"])
        .assert()
        .success()
        .stdout(predicate::str::contains("time-for-time"))
        .stdout(predicate::str::contains("Skala"));
}

#[tokio::test]
async fn score_hourly_rejects_combination_with_window() {
    let dir = TempDir::new().unwrap();
    let cfg = write_config(&dir, "user_agent_contact = \"dev@example.invalid\"\n");
    Command::cargo_bin("grusindeks")
        .unwrap()
        .arg("--config")
        .arg(&cfg)
        .args([
            "--lat",
            "59.9139",
            "--lon",
            "10.7522",
            "--hours",
            "3",
            "--hourly",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--hourly"));
}

#[tokio::test]
async fn score_hourly_json_carries_hourly_field() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_m("/weatherapi/locationforecast/2.0/complete"))
        .respond_with(ResponseTemplate::new(200).set_body_string(LOCATIONFORECAST_FIXTURE))
        .mount(&server)
        .await;

    let dir = TempDir::new().unwrap();
    let cfg = write_config(
        &dir,
        r#"user_agent_contact = "dev@example.invalid"
[places.oslo]
lat = 59.9139
lon = 10.7522
radius_km = 20.0
"#,
    );

    let out = Command::cargo_bin("grusindeks")
        .unwrap()
        .env("GRUSINDEKS_API_BASE", format!("{}/", server.uri()))
        .env("GRUSINDEKS_FROST_BASE", format!("{}/", server.uri()))
        .arg("--config")
        .arg(&cfg)
        .args(["--place", "oslo", "--days", "2", "--hourly", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let body = String::from_utf8(out).unwrap();
    let v: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");
    let hourly = v.get("hourly").expect("hourly field");
    assert!(hourly.get("days").and_then(|d| d.as_array()).is_some());
    assert!(hourly
        .get("header_hours")
        .and_then(|h| h.as_array())
        .is_some());
}

#[tokio::test]
async fn score_with_best_window_surfaces_a_window_per_day() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_m("/weatherapi/locationforecast/2.0/complete"))
        .respond_with(ResponseTemplate::new(200).set_body_string(LOCATIONFORECAST_FIXTURE))
        .mount(&server)
        .await;

    let dir = TempDir::new().unwrap();
    let cfg = write_config(
        &dir,
        r#"user_agent_contact = "dev@example.invalid"
[places.oslo]
lat = 59.9139
lon = 10.7522
radius_km = 20.0
"#,
    );

    let out = Command::cargo_bin("grusindeks")
        .unwrap()
        .env("GRUSINDEKS_API_BASE", format!("{}/", server.uri()))
        .env("GRUSINDEKS_FROST_BASE", format!("{}/", server.uri()))
        .arg("--config")
        .arg(&cfg)
        .args(["--place", "oslo", "--days", "3", "--best-window", "2"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let body = String::from_utf8(out).unwrap();
    // Either label is fine — both indicate that a sub-window surfaced.
    // What we assert is that *at least one* day in the multi-day output
    // shows a sub-window suggestion, which is the contract of
    // `--best-window`.
    assert!(
        body.contains("Beste vindu") || body.contains("Beste luke"),
        "expected a sub-window suggestion in --best-window output: {body}"
    );
}

#[tokio::test]
async fn score_best_window_rejects_zero_hours() {
    let dir = TempDir::new().unwrap();
    let cfg = write_config(&dir, "user_agent_contact = \"dev@example.invalid\"\n");
    Command::cargo_bin("grusindeks")
        .unwrap()
        .arg("--config")
        .arg(&cfg)
        .args(["--lat", "59.9139", "--lon", "10.7522", "--best-window", "0"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--best-window").and(predicate::str::contains("0")));
}

#[tokio::test]
async fn score_best_window_rejects_combination_with_single_day_modes() {
    let dir = TempDir::new().unwrap();
    let cfg = write_config(&dir, "user_agent_contact = \"dev@example.invalid\"\n");
    Command::cargo_bin("grusindeks")
        .unwrap()
        .arg("--config")
        .arg(&cfg)
        .args([
            "--lat",
            "59.9139",
            "--lon",
            "10.7522",
            "--hours",
            "3",
            "--best-window",
            "2",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "--best-window gjelder bare multi-dags-prognosen",
        ));
}

#[tokio::test]
async fn score_with_days_and_window_errors_clearly() {
    let dir = TempDir::new().unwrap();
    let cfg = write_config(&dir, "user_agent_contact = \"dev@example.invalid\"\n");
    Command::cargo_bin("grusindeks")
        .unwrap()
        .arg("--config")
        .arg(&cfg)
        .args([
            "--lat",
            "59.9139",
            "--lon",
            "10.7522",
            "--days",
            "3",
            "--window",
            "14:00-17:00",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "kan ikke brukes sammen med --days",
        ));
}

#[tokio::test]
async fn score_with_days_json_carries_forecast_field() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_m("/weatherapi/locationforecast/2.0/complete"))
        .respond_with(ResponseTemplate::new(200).set_body_string(LOCATIONFORECAST_FIXTURE))
        .mount(&server)
        .await;

    let dir = TempDir::new().unwrap();
    let cfg = write_config(&dir, "user_agent_contact = \"dev@example.invalid\"\n");

    let out = Command::cargo_bin("grusindeks")
        .unwrap()
        .env("GRUSINDEKS_API_BASE", format!("{}/", server.uri()))
        .env("GRUSINDEKS_FROST_BASE", format!("{}/", server.uri()))
        .arg("--config")
        .arg(&cfg)
        .arg("--json")
        .args(["--lat", "59.9139", "--lon", "10.7522", "--days", "3"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let parsed: serde_json::Value = serde_json::from_slice(&out).expect("output must be JSON");
    let days = parsed["forecast"]["days"].as_array().expect("days array");
    assert_eq!(days.len(), 3, "expected 3 days, got {days:?}");
    for d in days {
        assert!(d["mean"].is_u64());
        assert!(d["confidence"].is_string());
    }
}

#[tokio::test]
async fn no_subcommand_runs_six_day_forecast_against_default_place() {
    // `grusindeks` with no subcommand and no time args should hit the
    // multi-day forecast for the configured `default_place`.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_m("/weatherapi/locationforecast/2.0/complete"))
        .respond_with(ResponseTemplate::new(200).set_body_string(LOCATIONFORECAST_FIXTURE))
        .mount(&server)
        .await;

    let dir = TempDir::new().unwrap();
    let cfg = write_config(
        &dir,
        r#"user_agent_contact = "dev@example.invalid"
default_place = "oslo"
[places.oslo]
lat = 59.9139
lon = 10.7522
radius_km = 20.0
"#,
    );

    let out = Command::cargo_bin("grusindeks")
        .unwrap()
        .env("GRUSINDEKS_API_BASE", format!("{}/", server.uri()))
        .env("GRUSINDEKS_FROST_BASE", format!("{}/", server.uri()))
        .arg("--config")
        .arg(&cfg)
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let parsed: serde_json::Value = serde_json::from_slice(&out).expect("output must be JSON");
    let days = parsed["forecast"]["days"]
        .as_array()
        .expect("default invocation should produce the multi-day forecast shape");
    assert_eq!(
        days.len(),
        6,
        "default should be 6 days, got {}",
        days.len()
    );
}

#[tokio::test]
async fn score_with_frost_configured_calls_observations_endpoint() {
    // End-to-end Frost path: when both `frost.client_id` and
    // `frost.source_id` are configured, the binary must hit the
    // observations endpoint (with the full multi-element query) and
    // honour the historic data when scoring.
    let server = MockServer::start().await;
    let frost_body = r#"{
        "data": [
            {"referenceTime": "2026-04-25T22:00:00.000Z",
             "observations": [
                {"elementId": "sum(precipitation_amount PT1H)", "value": 1.4},
                {"elementId": "mean(air_temperature PT1H)",     "value": 8.0},
                {"elementId": "mean(wind_speed PT1H)",          "value": 3.0},
                {"elementId": "mean(relative_humidity PT1H)",   "value": 85.0}
             ]},
            {"referenceTime": "2026-04-25T23:00:00.000Z",
             "observations": [
                {"elementId": "sum(precipitation_amount PT1H)", "value": 2.6},
                {"elementId": "mean(air_temperature PT1H)",     "value": 8.0},
                {"elementId": "mean(wind_speed PT1H)",          "value": 3.0},
                {"elementId": "mean(relative_humidity PT1H)",   "value": 90.0}
             ]}
        ]
    }"#;

    Mock::given(method("GET"))
        .and(path_m("/weatherapi/locationforecast/2.0/complete"))
        .respond_with(ResponseTemplate::new(200).set_body_string(LOCATIONFORECAST_FIXTURE))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path_m("/observations/v0.jsonld"))
        .respond_with(ResponseTemplate::new(200).set_body_string(frost_body))
        .mount(&server)
        .await;

    let dir = TempDir::new().unwrap();
    let cfg = write_config(
        &dir,
        r#"user_agent_contact = "dev@example.invalid"
[frost]
client_id = "test-client-id"
source_id = "SN18700"
[places.oslo]
lat = 59.9139
lon = 10.7522
radius_km = 20.0
"#,
    );

    Command::cargo_bin("grusindeks")
        .unwrap()
        .env("GRUSINDEKS_API_BASE", format!("{}/", server.uri()))
        .env("GRUSINDEKS_FROST_BASE", format!("{}/", server.uri()))
        .arg("--config")
        .arg(&cfg)
        .args(["--place", "oslo", "--hours", "3"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Grusindeks for oslo"));

    // Now verify Frost was actually hit with the full multi-element
    // query. If we ever silently regress to precip-only, this assertion
    // catches it before the drying model degrades again.
    let received = server.received_requests().await.unwrap();
    let frost_hit = received
        .iter()
        .find(|r| r.url.path() == "/observations/v0.jsonld");
    let req = frost_hit.expect("frost observations endpoint was never called");
    let qs: std::collections::HashMap<_, _> = req.url.query_pairs().into_owned().collect();
    let elements = qs.get("elements").expect("missing elements query param");
    assert!(
        elements.contains("precipitation_amount")
            && elements.contains("air_temperature")
            && elements.contains("wind_speed")
            && elements.contains("relative_humidity"),
        "expected full multi-element query, got: {elements}",
    );
}

#[tokio::test]
async fn score_falls_back_when_frost_returns_500() {
    // Frost outage: locationforecast still works, but the observations
    // endpoint returns 500. The CLI must surface a score (no panic, no
    // hang) — the ground axis just becomes "unknown" instead of being
    // computed from observations.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_m("/weatherapi/locationforecast/2.0/complete"))
        .respond_with(ResponseTemplate::new(200).set_body_string(LOCATIONFORECAST_FIXTURE))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path_m("/observations/v0.jsonld"))
        .respond_with(ResponseTemplate::new(500).set_body_string("upstream broke"))
        .mount(&server)
        .await;

    let dir = TempDir::new().unwrap();
    let cfg = write_config(
        &dir,
        r#"user_agent_contact = "dev@example.invalid"
[frost]
client_id = "test-client-id"
source_id = "SN18700"
[places.oslo]
lat = 59.9139
lon = 10.7522
radius_km = 20.0
"#,
    );

    Command::cargo_bin("grusindeks")
        .unwrap()
        .env("GRUSINDEKS_API_BASE", format!("{}/", server.uri()))
        .env("GRUSINDEKS_FROST_BASE", format!("{}/", server.uri()))
        .arg("--config")
        .arg(&cfg)
        .args(["--place", "oslo", "--hours", "3"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Grusindeks for oslo"));
}

#[tokio::test]
async fn score_truncates_coordinates_in_query_string() {
    // High-precision input (6 decimals) must be truncated to 4 decimals
    // before hitting api.met.no. Verified by introspecting *all* requests
    // wiremock saw — every one of the 9 sample points must have ≤4 decimals.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_m("/weatherapi/locationforecast/2.0/complete"))
        .respond_with(ResponseTemplate::new(200).set_body_string(LOCATIONFORECAST_FIXTURE))
        .mount(&server)
        .await;

    let dir = TempDir::new().unwrap();
    let cfg = write_config(&dir, "user_agent_contact = \"dev@example.invalid\"\n");

    Command::cargo_bin("grusindeks")
        .unwrap()
        .env("GRUSINDEKS_API_BASE", format!("{}/", server.uri()))
        .env("GRUSINDEKS_FROST_BASE", format!("{}/", server.uri()))
        .arg("--config")
        .arg(&cfg)
        .args(["--lat", "59.913918", "--lon", "10.752230"])
        .assert()
        .success();

    let received = server.received_requests().await.unwrap();
    assert!(!received.is_empty(), "wiremock saw no requests");
    let mut center_seen = false;
    for r in received {
        let qs: std::collections::HashMap<_, _> = r.url.query_pairs().into_owned().collect();
        let lat = qs.get("lat").expect("lat query param");
        let lon = qs.get("lon").expect("lon query param");
        for (name, v) in [("lat", lat), ("lon", lon)] {
            let frac = v.split('.').nth(1).unwrap_or("");
            assert!(frac.len() <= 4, "{name}={v} has more than 4 decimals");
        }
        if lat == "59.9139" && lon == "10.7522" {
            center_seen = true;
        }
    }
    assert!(
        center_seen,
        "the truncated center coords were never requested"
    );
}

// ---- Regression tests for fixes from the 2026-04 review ----

/// B1: --hours 0 used to panic in RideWindow::from_hours with exit 101.
/// Now: clean error from clap's range validator.
#[test]
fn score_hours_zero_is_rejected_cleanly() {
    let dir = TempDir::new().unwrap();
    let cfg = write_config(&dir, "user_agent_contact = \"dev@example.invalid\"\n");
    Command::cargo_bin("grusindeks")
        .unwrap()
        .arg("--config")
        .arg(&cfg)
        .args(["--lat", "59.9139", "--lon", "10.7522", "--hours", "0"])
        .assert()
        .failure()
        // clap's range error: "0 is not in 1..=24". Pin on the value
        // and the flag name so future range adjustments don't break this.
        .stderr(predicate::str::contains("--hours").and(predicate::str::contains("0")));
}

/// B3: an empty `timeseries: []` upstream response used to score 71/100
/// "Bra" with phantom -0 °C means. Now: a Critical NoData penalty
/// surfaces and the total drops.
#[tokio::test]
async fn score_against_empty_forecast_surfaces_no_data_not_phantom_score() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_m("/weatherapi/locationforecast/2.0/complete"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(r#"{"properties":{"timeseries":[]}}"#),
        )
        .mount(&server)
        .await;

    let dir = TempDir::new().unwrap();
    let cfg = write_config(&dir, "user_agent_contact = \"dev@example.invalid\"\n");

    let out = Command::cargo_bin("grusindeks")
        .unwrap()
        .env("GRUSINDEKS_API_BASE", format!("{}/", server.uri()))
        .env("GRUSINDEKS_FROST_BASE", format!("{}/", server.uri()))
        .arg("--config")
        .arg(&cfg)
        .arg("--json")
        .args(["--lat", "59.9139", "--lon", "10.7522", "--hours", "3"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let parsed: serde_json::Value = serde_json::from_slice(&out).expect("output must be JSON");
    let mean = parsed["aggregate"]["mean"].as_u64().unwrap();
    assert_eq!(mean, 0, "expected 0 from a no-data response, got {mean}");
    let label = parsed["aggregate"]["points"][0]["score"]["label"]
        .as_str()
        .expect("label string");
    assert_eq!(label, "Ingen data");
    let penalties = parsed["aggregate"]["points"][0]["score"]["penalties"]
        .as_array()
        .expect("penalties array");
    assert!(
        penalties
            .iter()
            .any(|p| p["component"] == "no_data" && p["severity"] == "critical"),
        "expected a Critical NoData penalty, got {penalties:?}"
    );
}

/// B2: identical fixture, two runs, identical JSON. Locks down the
/// post-fan-out sort by (lat, lon).
#[tokio::test]
async fn json_output_is_deterministic_across_runs() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_m("/weatherapi/locationforecast/2.0/complete"))
        .respond_with(ResponseTemplate::new(200).set_body_string(LOCATIONFORECAST_FIXTURE))
        .mount(&server)
        .await;

    let dir = TempDir::new().unwrap();
    let cfg = write_config(&dir, "user_agent_contact = \"dev@example.invalid\"\n");

    let run = || {
        Command::cargo_bin("grusindeks")
            .unwrap()
            .env("GRUSINDEKS_API_BASE", format!("{}/", server.uri()))
            .env("GRUSINDEKS_FROST_BASE", format!("{}/", server.uri()))
            .arg("--config")
            .arg(&cfg)
            .arg("--json")
            .args(["--lat", "59.9139", "--lon", "10.7522", "--hours", "3"])
            .output()
            .unwrap()
            .stdout
    };
    let a = run();
    let b = run();
    let pa: serde_json::Value = serde_json::from_slice(&a).unwrap();
    let pb: serde_json::Value = serde_json::from_slice(&b).unwrap();
    // Compare just the aggregate (point ordering, totals) — the top-level
    // location is identical, but pinning aggregate is the meaningful bit.
    assert_eq!(
        pa["aggregate"], pb["aggregate"],
        "non-deterministic aggregate output between identical runs"
    );
}

/// `grusindeks config path` must agree with the platform-appropriate
/// directory MetClientConfig writes to. README claimed
/// `~/.config/grusindeks` on every OS — wrong on macOS/Windows.
#[test]
fn config_path_returns_a_grusindeks_directory() {
    let out = Command::cargo_bin("grusindeks")
        .unwrap()
        .args(["config", "path"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let s = String::from_utf8(out).unwrap();
    assert!(
        s.trim().contains("grusindeks"),
        "config path should mention 'grusindeks', got {s:?}"
    );
    assert!(
        s.trim().ends_with("config.toml"),
        "config path should end with config.toml, got {s:?}"
    );
}

/// `--days 1` must route through the multi-day path and honour the
/// configured daytime_window — used to fall through to single-day with
/// a 3h-from-now window.
#[tokio::test]
async fn days_one_routes_through_multi_day_path() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_m("/weatherapi/locationforecast/2.0/complete"))
        .respond_with(ResponseTemplate::new(200).set_body_string(LOCATIONFORECAST_FIXTURE))
        .mount(&server)
        .await;

    let dir = TempDir::new().unwrap();
    let cfg = write_config(&dir, "user_agent_contact = \"dev@example.invalid\"\n");

    Command::cargo_bin("grusindeks")
        .unwrap()
        .env("GRUSINDEKS_API_BASE", format!("{}/", server.uri()))
        .env("GRUSINDEKS_FROST_BASE", format!("{}/", server.uri()))
        .arg("--config")
        .arg(&cfg)
        .args(["--lat", "59.9139", "--lon", "10.7522", "--days", "1"])
        .assert()
        .success()
        // Multi-day output prints the "·" header bullet; single-day
        // output prints the "Grusindeks for ..." line. Pin on the
        // multi-day shape to confirm the correct path was taken.
        .stdout(predicate::str::contains("Grusindeks ·"));
}

/// --days beyond MET's published horizon must be rejected by clap, not
/// silently rendered as duplicate "·" placeholder days.
#[test]
fn days_above_horizon_is_rejected() {
    let dir = TempDir::new().unwrap();
    let cfg = write_config(&dir, "user_agent_contact = \"dev@example.invalid\"\n");
    Command::cargo_bin("grusindeks")
        .unwrap()
        .arg("--config")
        .arg(&cfg)
        .args(["--lat", "59.9139", "--lon", "10.7522", "--days", "30"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--days").and(predicate::str::contains("30")));
}
