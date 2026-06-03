//! Plain data-transfer types for the settings server functions.
//!
//! These are wasm-safe (only `String`/numbers/bool/`Vec`) so they cross the
//! server-fn boundary and drive the settings forms on the client. Optional
//! fields use empty strings rather than `Option` to keep the HTML forms simple.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PrefsDto {
    pub user_agent_contact: String,
    /// Name of the default place (empty = none).
    pub default_place: String,
    /// "norwegian" | "swedish".
    pub language: String,
    pub daytime_start: String,
    pub daytime_end: String,
    pub show_rain_history: bool,
    pub show_window_stats: bool,
    pub frost_client_id: String,
    pub frost_source_id: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PlaceDto {
    /// `None` for a new place; `Some(id)` to update an existing one.
    pub id: Option<i64>,
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub radius_km: f64,
    pub frost_source_id: String,
}

/// One stored history sample for a sparkline (x = target time, y = mean score).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HistoryPoint {
    /// RFC3339 UTC; the score's target window/day start.
    pub target_time: String,
    pub mean: u8,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkHoursDto {
    pub enabled: bool,
    /// Lowercase weekday tokens: "mon", "tue", …
    pub days: Vec<String>,
    pub window_start: String,
    pub window_end: String,
}
