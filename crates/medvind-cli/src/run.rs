//! End-to-end "compute Grusindeks for a location" pipeline.
//!
//! Wired into the CLI by `commands::score`. Lives in its own module so the
//! integration tests can call it directly with a test client.

use anyhow::Result;
use chrono::{Duration, Utc};
use medvind_core::drying::{drying_step, DryingParams, DryingState};
use medvind_core::geo::{sample_around, Point};
use medvind_core::score::score;
use medvind_core::types::{HourlyConditions, RideWindow};
use medvind_met::client::MetClient;
use medvind_met::frost;
use medvind_met::locationforecast;

use crate::aggregate::AggregateScore;

pub struct ScoreInputs<'a> {
    pub center: Point,
    pub radius_km: f64,
    pub window: RideWindow,
    pub frost_source_id: Option<&'a str>,
    /// How many hours of past observations to feed into the drying model.
    pub history_hours: i64,
}

/// Fetch every input we need and compute one Grusindeks per sample point.
pub async fn run_score(client: &MetClient, inputs: ScoreInputs<'_>) -> Result<AggregateScore> {
    let points = sample_around(inputs.center, inputs.radius_km);

    // Historical precipitation is the same series for every point — Frost
    // is keyed by station, not coordinate. Fetch once.
    let ground_initial_mm = match inputs.frost_source_id {
        Some(src) if client.config().frost_client_id.is_some() => {
            let to = Utc::now();
            let from = to - Duration::hours(inputs.history_hours);
            match frost::fetch_hourly_precip(client, src, from, to).await {
                Ok(history) => replay_into_state(&history, &DryingParams::default()),
                Err(e) => {
                    tracing::warn!("frost lookup failed, assuming dry ground: {e}");
                    0.0
                }
            }
        }
        _ => 0.0,
    };

    let mut scored = Vec::with_capacity(points.len());
    for p in points {
        let forecast = locationforecast::fetch(client, p).await?;
        let s = score(&forecast.hours, inputs.window, ground_initial_mm);
        scored.push((p, s));
    }
    Ok(AggregateScore::from_points(inputs.center, scored))
}

/// Replay a list of `(time, mm)` precipitation observations through the
/// drying model. Uses neutral drying conditions for missing temp/wind/etc.,
/// since Frost-only history doesn't carry those for free.
fn replay_into_state(history: &[frost::HourlyPrecip], p: &DryingParams) -> f64 {
    // Conservative neutral assumption: 10°C, 3 m/s wind, 70% humidity, 70%
    // cloud, no UV. Tunable later if it turns out to be too pessimistic.
    let mut state = DryingState::default();
    for h in history {
        let synth = HourlyConditions {
            time: h.time,
            temperature_c: 10.0,
            wind_speed_ms: 3.0,
            precipitation_mm: h.mm,
            wind_gust_ms: None,
            wind_from_deg: None,
            probability_of_precip: None,
            relative_humidity: Some(70.0),
            cloud_area_fraction: Some(70.0),
            uv_index_clear_sky: None,
        };
        state = drying_step(state, &synth, p);
    }
    state.accumulated_mm
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_history_yields_dry_ground() {
        let mm = replay_into_state(&[], &DryingParams::default());
        assert_eq!(mm, 0.0);
    }

    #[test]
    fn heavy_recent_history_leaves_water_on_ground() {
        let now = Utc::now();
        let history: Vec<frost::HourlyPrecip> = (0..6)
            .map(|i| frost::HourlyPrecip {
                time: now - Duration::hours(6 - i),
                mm: 3.0,
            })
            .collect();
        let mm = replay_into_state(&history, &DryingParams::default());
        assert!(mm > 0.0, "expected some accumulation, got {mm}");
    }
}
