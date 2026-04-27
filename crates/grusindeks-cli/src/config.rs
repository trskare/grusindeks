//! User config file (`~/.config/grusindeks/config.toml`).
//!
//! Holds the User-Agent contact (TOS-mandated), Frost credentials, and
//! named ride locations. The CLI reads this on every invocation.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use chrono::NaiveTime;
use directories::ProjectDirs;
use grusindeks_core::geo::Point;
use grusindeks_core::lang::Language;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Shown in the User-Agent header so MET can reach the operator if
    /// something goes wrong. **Required** by the TOS.
    pub user_agent_contact: String,

    #[serde(default)]
    pub default_place: Option<String>,

    /// UI language for human-readable output (labels, penalty messages,
    /// day labels). Defaults to Norwegian. Set to `"swedish"` for Swedish.
    #[serde(default)]
    pub language: Language,

    /// Local-time window the multi-day forecast averages over each day.
    /// Defaults to 10:00–22:00 — wide enough to cover most ride plans
    /// without dragging in the cold dawn / late-night hours that nobody
    /// rides through.
    #[serde(default)]
    pub daytime_window: DaytimeWindow,

    #[serde(default)]
    pub frost: FrostConfig,

    /// Named ride locations.
    #[serde(default)]
    pub places: std::collections::BTreeMap<String, PlaceConfig>,
}

/// Local-time window used to clip each day in the multi-day forecast.
/// Parsed from a `"HH:MM-HH:MM"` TOML string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct DaytimeWindow {
    pub start: NaiveTime,
    pub end: NaiveTime,
}

impl Default for DaytimeWindow {
    fn default() -> Self {
        Self {
            start: NaiveTime::from_hms_opt(10, 0, 0).expect("10:00 is a valid time"),
            end: NaiveTime::from_hms_opt(22, 0, 0).expect("22:00 is a valid time"),
        }
    }
}

impl TryFrom<String> for DaytimeWindow {
    type Error = String;

    fn try_from(s: String) -> Result<Self, String> {
        let (a, b) = s
            .split_once('-')
            .ok_or_else(|| format!("daytime_window must be HH:MM-HH:MM, got {s:?}"))?;
        let start = NaiveTime::parse_from_str(a.trim(), "%H:%M")
            .map_err(|e| format!("invalid start in daytime_window {s:?}: {e}"))?;
        let end = NaiveTime::parse_from_str(b.trim(), "%H:%M")
            .map_err(|e| format!("invalid end in daytime_window {s:?}: {e}"))?;
        if end <= start {
            return Err(format!("daytime_window end must be after start, got {s:?}"));
        }
        Ok(Self { start, end })
    }
}

impl From<DaytimeWindow> for String {
    fn from(d: DaytimeWindow) -> Self {
        format!("{}-{}", d.start.format("%H:%M"), d.end.format("%H:%M"))
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct FrostConfig {
    pub client_id: Option<String>,
    /// Frost station id, e.g. `"SN18700"` for Blindern, Oslo. Optional —
    /// without it we skip historical lookups and assume dry ground.
    pub source_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaceConfig {
    pub lat: f64,
    pub lon: f64,
    #[serde(default = "default_radius_km")]
    pub radius_km: f64,
    /// Optional Frost station id to use for *this* place's historical
    /// observations. Falls back to `frost.source_id`.
    #[serde(default)]
    pub frost_source_id: Option<String>,
}

impl PlaceConfig {
    pub fn point(&self) -> Point {
        Point::new(self.lat, self.lon)
    }
}

const fn default_radius_km() -> f64 {
    20.0
}

impl Config {
    /// Default file location: `~/.config/grusindeks/config.toml` on Linux/macOS.
    pub fn default_path() -> Result<PathBuf> {
        let dirs = ProjectDirs::from("", "", "grusindeks")
            .ok_or_else(|| anyhow!("could not determine config directory for this OS"))?;
        Ok(dirs.config_dir().join("config.toml"))
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading config from {}", path.display()))?;
        let cfg: Config = toml::from_str(&raw)
            .with_context(|| format!("parsing config at {}", path.display()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<()> {
        if self.user_agent_contact.trim().is_empty() {
            bail!("user_agent_contact must not be empty (required by api.met.no TOS)");
        }
        Ok(())
    }

    /// Render the example config used by `grusindeks config init`.
    pub fn example_template() -> &'static str {
        EXAMPLE_TEMPLATE
    }
}

const EXAMPLE_TEMPLATE: &str = r#"# grusindeks configuration
# Required: a way for api.met.no to reach you (email or URL).
user_agent_contact = "you@example.com"

# Optional: a default place name used when --lat/--lon/--place is omitted.
default_place = "oslo"

# Optional: UI language. "norwegian" (default) or "swedish".
# language = "swedish"

# Optional: local-time window the multi-day forecast averages over each
# day. Default 10:00-22:00. Narrow it if you only ride mornings, widen
# it if you also want pre-dawn rides factored in.
# daytime_window = "10:00-22:00"

[frost]
# Register a free client_id at https://frost.met.no/auth/requestCredentials.html
# Without it, grusindeks skips historical observations and assumes dry ground.
# client_id = "00000000-0000-0000-0000-000000000000"
# source_id = "SN18700"  # default Frost station; e.g. SN18700 for Blindern, Oslo

[places.oslo]
lat = 59.9139
lon = 10.7522
radius_km = 20.0
# frost_source_id = "SN18700"  # overrides the global Frost station for this place
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_cfg(dir: &TempDir, body: &str) -> PathBuf {
        let p = dir.path().join("config.toml");
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn loads_minimal_config() {
        let dir = TempDir::new().unwrap();
        let p = write_cfg(&dir, "user_agent_contact = \"dev@example.invalid\"\n");
        let cfg = Config::load_from(&p).unwrap();
        assert_eq!(cfg.user_agent_contact, "dev@example.invalid");
        assert!(cfg.places.is_empty());
        // Default language is Norwegian.
        assert_eq!(cfg.language, Language::Norwegian);
    }

    #[test]
    fn loads_swedish_language_setting() {
        let dir = TempDir::new().unwrap();
        let p = write_cfg(
            &dir,
            "user_agent_contact = \"dev@example.invalid\"\nlanguage = \"swedish\"\n",
        );
        let cfg = Config::load_from(&p).unwrap();
        assert_eq!(cfg.language, Language::Swedish);
    }

    #[test]
    fn rejects_empty_contact() {
        let dir = TempDir::new().unwrap();
        let p = write_cfg(&dir, "user_agent_contact = \"\"\n");
        assert!(Config::load_from(&p).is_err());
    }

    #[test]
    fn loads_places_with_default_radius() {
        let dir = TempDir::new().unwrap();
        let p = write_cfg(
            &dir,
            r#"
user_agent_contact = "dev@example.invalid"
[places.oslo]
lat = 59.9139
lon = 10.7522
"#,
        );
        let cfg = Config::load_from(&p).unwrap();
        let oslo = &cfg.places["oslo"];
        assert_eq!(oslo.lat, 59.9139);
        assert_eq!(oslo.radius_km, 20.0);
    }

    #[test]
    fn loads_places_with_overridden_radius_and_frost_id() {
        let dir = TempDir::new().unwrap();
        let p = write_cfg(
            &dir,
            r#"
user_agent_contact = "dev@example.invalid"
[places.bergen]
lat = 60.39
lon = 5.32
radius_km = 12.5
frost_source_id = "SN50540"
"#,
        );
        let cfg = Config::load_from(&p).unwrap();
        let bergen = &cfg.places["bergen"];
        assert_eq!(bergen.radius_km, 12.5);
        assert_eq!(bergen.frost_source_id.as_deref(), Some("SN50540"));
    }

    #[test]
    fn example_template_round_trips() {
        // Ensure the template we hand to users actually parses.
        let cfg: Config = toml::from_str(Config::example_template()).unwrap();
        assert!(!cfg.user_agent_contact.is_empty());
        assert!(cfg.places.contains_key("oslo"));
    }

    #[test]
    fn default_daytime_window_is_10_to_22() {
        let dir = TempDir::new().unwrap();
        let p = write_cfg(&dir, "user_agent_contact = \"dev@example.invalid\"\n");
        let cfg = Config::load_from(&p).unwrap();
        assert_eq!(
            cfg.daytime_window.start,
            NaiveTime::from_hms_opt(10, 0, 0).unwrap()
        );
        assert_eq!(
            cfg.daytime_window.end,
            NaiveTime::from_hms_opt(22, 0, 0).unwrap()
        );
    }

    #[test]
    fn loads_overridden_daytime_window() {
        let dir = TempDir::new().unwrap();
        let p = write_cfg(
            &dir,
            "user_agent_contact = \"dev@example.invalid\"\ndaytime_window = \"08:00-20:00\"\n",
        );
        let cfg = Config::load_from(&p).unwrap();
        assert_eq!(
            cfg.daytime_window.start,
            NaiveTime::from_hms_opt(8, 0, 0).unwrap()
        );
        assert_eq!(
            cfg.daytime_window.end,
            NaiveTime::from_hms_opt(20, 0, 0).unwrap()
        );
    }

    #[test]
    fn rejects_daytime_window_with_end_before_start() {
        let dir = TempDir::new().unwrap();
        let p = write_cfg(
            &dir,
            "user_agent_contact = \"dev@example.invalid\"\ndaytime_window = \"20:00-08:00\"\n",
        );
        let err = Config::load_from(&p).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.to_lowercase().contains("daytime_window"), "got {msg}");
    }

    #[test]
    fn rejects_daytime_window_with_bad_format() {
        let dir = TempDir::new().unwrap();
        let p = write_cfg(
            &dir,
            "user_agent_contact = \"dev@example.invalid\"\ndaytime_window = \"morning\"\n",
        );
        assert!(Config::load_from(&p).is_err());
    }

    #[test]
    fn missing_file_errors_clearly() {
        let err = Config::load_from(Path::new("/no/such/path/grusindeks.toml")).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("reading config"), "got {msg}");
    }
}
