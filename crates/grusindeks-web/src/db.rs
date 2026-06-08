//! SQLite persistence (server-only).
//!
//! Owns the connection pool, runs migrations, and bridges the stored rows to
//! the existing [`Config`] shape the orchestration layer consumes — so the web
//! path feeds `run_*` the exact same config the CLI would. Everything is
//! scoped by [`DEFAULT_USER_ID`] today; swap that for a session user when auth
//! lands (no schema change needed).

use std::collections::BTreeMap;
use std::str::FromStr;
use std::time::Duration;

use anyhow::{Context, Result};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::SqlitePool;

use grusindeks_cli::config::{
    Config, DaytimeWindow, FrostConfig, PlaceConfig, WorkHoursConfig, Workday,
};
use grusindeks_core::lang::Language;
use grusindeks_core::score::ScoreBreakdown;

use crate::dto::{HistoryPoint, PlaceDto, PrefsDto, WorkHoursDto};

/// Single-user id until auth is added. Every query is scoped by this.
pub const DEFAULT_USER_ID: i64 = 1;

/// Open (creating if needed) the SQLite database, set pragmas, run migrations.
///
/// WAL + a busy-timeout give smooth concurrent reads with the occasional small
/// write this app does; foreign keys are enforced for the cascade deletes.
pub async fn init_pool(db_path: &str) -> Result<SqlitePool> {
    let opts = SqliteConnectOptions::from_str(db_path)
        .with_context(|| format!("parse sqlite path {db_path:?}"))?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(5));

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(opts)
        .await
        .context("open sqlite pool")?;

    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .context("run migrations")?;

    Ok(pool)
}

// ---- row structs ----

#[derive(sqlx::FromRow)]
struct PrefsRow {
    user_agent_contact: String,
    default_place_id: Option<i64>,
    language: String,
    daytime_start: String,
    daytime_end: String,
    show_rain_history: i64,
    show_window_stats: i64,
    frost_client_id: Option<String>,
    frost_source_id: Option<String>,
}

#[derive(sqlx::FromRow)]
struct PlaceRow {
    id: i64,
    name: String,
    lat: f64,
    lon: f64,
    radius_km: f64,
    frost_source_id: Option<String>,
}

#[derive(sqlx::FromRow)]
struct WorkHoursRow {
    enabled: i64,
    days_csv: String,
    window_start: String,
    window_end: String,
}

// ---- load into the existing Config shape ----

/// Assemble the [`Config`] the orchestration layer expects from the stored
/// prefs/places/work-hours rows.
pub async fn load_config(pool: &SqlitePool, user_id: i64) -> Result<Config> {
    let prefs: PrefsRow = sqlx::query_as(
        "SELECT user_agent_contact, default_place_id, language, daytime_start, daytime_end, \
         show_rain_history, show_window_stats, frost_client_id, frost_source_id \
         FROM prefs WHERE user_id = ?",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await
    .context("load prefs")?;

    let place_rows: Vec<PlaceRow> = sqlx::query_as(
        "SELECT id, name, lat, lon, radius_km, frost_source_id FROM places \
         WHERE user_id = ? ORDER BY name",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
    .context("load places")?;

    let wh: WorkHoursRow = sqlx::query_as(
        "SELECT enabled, days_csv, window_start, window_end FROM work_hours WHERE user_id = ?",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await
    .context("load work_hours")?;

    let mut places = BTreeMap::new();
    let mut default_place = None;
    for r in &place_rows {
        if Some(r.id) == prefs.default_place_id {
            default_place = Some(r.name.clone());
        }
        places.insert(
            r.name.clone(),
            PlaceConfig {
                lat: r.lat,
                lon: r.lon,
                radius_km: r.radius_km,
                frost_source_id: r.frost_source_id.clone(),
                // Per-place drainage is configurable via the CLI TOML config;
                // the web UI doesn't expose it yet, so places fall back to the
                // config-level default below.
                drainage: None,
            },
        );
    }

    let daytime_window =
        DaytimeWindow::try_from(format!("{}-{}", prefs.daytime_start, prefs.daytime_end))
            .map_err(|e| anyhow::anyhow!("invalid daytime window: {e}"))?;
    let work_window = DaytimeWindow::try_from(format!("{}-{}", wh.window_start, wh.window_end))
        .map_err(|e| anyhow::anyhow!("invalid work-hours window: {e}"))?;

    let config = Config {
        user_agent_contact: prefs.user_agent_contact,
        default_place,
        language: parse_language(&prefs.language),
        daytime_window,
        work_hours: WorkHoursConfig {
            enabled: wh.enabled != 0,
            days: parse_days(&wh.days_csv),
            window: work_window,
        },
        frost: FrostConfig {
            client_id: prefs.frost_client_id,
            source_id: prefs.frost_source_id,
        },
        // Global drainage default for the web; per-place / per-config tuning
        // lives in the CLI TOML for now.
        drainage: Default::default(),
        show_rain_history: prefs.show_rain_history != 0,
        show_window_stats: prefs.show_window_stats != 0,
        places,
    };
    Ok(config)
}

// ---- prefs CRUD ----

pub async fn get_prefs(pool: &SqlitePool, user_id: i64) -> Result<PrefsDto> {
    let prefs: PrefsRow = sqlx::query_as(
        "SELECT user_agent_contact, default_place_id, language, daytime_start, daytime_end, \
         show_rain_history, show_window_stats, frost_client_id, frost_source_id \
         FROM prefs WHERE user_id = ?",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await?;

    let default_place = match prefs.default_place_id {
        Some(id) => sqlx::query_scalar::<_, String>("SELECT name FROM places WHERE id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await?
            .unwrap_or_default(),
        None => String::new(),
    };

    Ok(PrefsDto {
        user_agent_contact: prefs.user_agent_contact,
        default_place,
        language: prefs.language,
        daytime_start: prefs.daytime_start,
        daytime_end: prefs.daytime_end,
        show_rain_history: prefs.show_rain_history != 0,
        show_window_stats: prefs.show_window_stats != 0,
        frost_client_id: prefs.frost_client_id.unwrap_or_default(),
        frost_source_id: prefs.frost_source_id.unwrap_or_default(),
    })
}

pub async fn update_prefs(pool: &SqlitePool, user_id: i64, dto: &PrefsDto) -> Result<()> {
    let default_place_id: Option<i64> = if dto.default_place.is_empty() {
        None
    } else {
        sqlx::query_scalar::<_, i64>("SELECT id FROM places WHERE user_id = ? AND name = ?")
            .bind(user_id)
            .bind(&dto.default_place)
            .fetch_optional(pool)
            .await?
    };

    sqlx::query(
        "UPDATE prefs SET user_agent_contact = ?, default_place_id = ?, language = ?, \
         daytime_start = ?, daytime_end = ?, show_rain_history = ?, show_window_stats = ?, \
         frost_client_id = ?, frost_source_id = ? WHERE user_id = ?",
    )
    .bind(&dto.user_agent_contact)
    .bind(default_place_id)
    .bind(&dto.language)
    .bind(&dto.daytime_start)
    .bind(&dto.daytime_end)
    .bind(dto.show_rain_history as i64)
    .bind(dto.show_window_stats as i64)
    .bind(opt(&dto.frost_client_id))
    .bind(opt(&dto.frost_source_id))
    .bind(user_id)
    .execute(pool)
    .await?;
    Ok(())
}

// ---- places CRUD ----

pub async fn list_places(pool: &SqlitePool, user_id: i64) -> Result<Vec<PlaceDto>> {
    let rows: Vec<PlaceRow> = sqlx::query_as(
        "SELECT id, name, lat, lon, radius_km, frost_source_id FROM places \
         WHERE user_id = ? ORDER BY name",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| PlaceDto {
            id: Some(r.id),
            name: r.name,
            lat: r.lat,
            lon: r.lon,
            radius_km: r.radius_km,
            frost_source_id: r.frost_source_id.unwrap_or_default(),
        })
        .collect())
}

pub async fn upsert_place(pool: &SqlitePool, user_id: i64, dto: &PlaceDto) -> Result<i64> {
    let frost = opt(&dto.frost_source_id);
    match dto.id {
        Some(id) => {
            sqlx::query(
                "UPDATE places SET name = ?, lat = ?, lon = ?, radius_km = ?, frost_source_id = ? \
                 WHERE id = ? AND user_id = ?",
            )
            .bind(&dto.name)
            .bind(dto.lat)
            .bind(dto.lon)
            .bind(dto.radius_km)
            .bind(frost)
            .bind(id)
            .bind(user_id)
            .execute(pool)
            .await?;
            Ok(id)
        }
        None => {
            let id: i64 = sqlx::query_scalar(
                "INSERT INTO places (user_id, name, lat, lon, radius_km, frost_source_id) \
                 VALUES (?, ?, ?, ?, ?, ?) RETURNING id",
            )
            .bind(user_id)
            .bind(&dto.name)
            .bind(dto.lat)
            .bind(dto.lon)
            .bind(dto.radius_km)
            .bind(frost)
            .fetch_one(pool)
            .await?;
            Ok(id)
        }
    }
}

pub async fn delete_place(pool: &SqlitePool, user_id: i64, id: i64) -> Result<()> {
    sqlx::query("DELETE FROM places WHERE id = ? AND user_id = ?")
        .bind(id)
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(())
}

// ---- work hours ----

pub async fn get_work_hours(pool: &SqlitePool, user_id: i64) -> Result<WorkHoursDto> {
    let wh: WorkHoursRow = sqlx::query_as(
        "SELECT enabled, days_csv, window_start, window_end FROM work_hours WHERE user_id = ?",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await?;
    Ok(WorkHoursDto {
        enabled: wh.enabled != 0,
        days: wh
            .days_csv
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        window_start: wh.window_start,
        window_end: wh.window_end,
    })
}

pub async fn update_work_hours(pool: &SqlitePool, user_id: i64, dto: &WorkHoursDto) -> Result<()> {
    // Keep only recognised weekday tokens, in canonical order.
    let days_csv = canonical_days_csv(&dto.days);
    sqlx::query(
        "UPDATE work_hours SET enabled = ?, days_csv = ?, window_start = ?, window_end = ? \
         WHERE user_id = ?",
    )
    .bind(dto.enabled as i64)
    .bind(days_csv)
    .bind(&dto.window_start)
    .bind(&dto.window_end)
    .bind(user_id)
    .execute(pool)
    .await?;
    Ok(())
}

// ---- score history ----

/// A row to upsert into `score_history`.
pub struct HistoryInsert<'a> {
    pub place_id: i64,
    pub kind: &'a str,
    /// RFC3339 UTC. The unique key (with user/place/kind) — re-logging the same
    /// target overwrites, so repeated page loads don't accumulate duplicates.
    pub target_time: String,
    pub mean: u8,
    pub min: u8,
    pub max: u8,
    pub breakdown: Option<ScoreBreakdown>,
    pub confidence: Option<&'a str>,
}

/// Look up a place id by name (None for ad-hoc lat/lon locations).
pub async fn place_id_by_name(pool: &SqlitePool, user_id: i64, name: &str) -> Result<Option<i64>> {
    Ok(
        sqlx::query_scalar("SELECT id FROM places WHERE user_id = ? AND name = ?")
            .bind(user_id)
            .bind(name)
            .fetch_optional(pool)
            .await?,
    )
}

/// Idempotent upsert of a score sample.
pub async fn log_score(pool: &SqlitePool, user_id: i64, h: &HistoryInsert<'_>) -> Result<()> {
    let bd = |f: fn(&ScoreBreakdown) -> u8| h.breakdown.as_ref().map(|b| f(b) as i64);
    sqlx::query(
        "INSERT INTO score_history \
         (user_id, place_id, kind, target_time, mean, \"min\", \"max\", \
          bd_temperature, bd_wind, bd_precipitation, bd_precip_probability, bd_ground, confidence) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(user_id, place_id, kind, target_time) DO UPDATE SET \
          computed_at = datetime('now'), mean = excluded.mean, \"min\" = excluded.\"min\", \
          \"max\" = excluded.\"max\", bd_temperature = excluded.bd_temperature, \
          bd_wind = excluded.bd_wind, bd_precipitation = excluded.bd_precipitation, \
          bd_precip_probability = excluded.bd_precip_probability, bd_ground = excluded.bd_ground, \
          confidence = excluded.confidence",
    )
    .bind(user_id)
    .bind(h.place_id)
    .bind(h.kind)
    .bind(&h.target_time)
    .bind(h.mean as i64)
    .bind(h.min as i64)
    .bind(h.max as i64)
    .bind(bd(|b| b.temperature))
    .bind(bd(|b| b.wind))
    .bind(bd(|b| b.precipitation))
    .bind(bd(|b| b.precip_probability))
    .bind(bd(|b| b.ground))
    .bind(h.confidence)
    .execute(pool)
    .await?;
    Ok(())
}

/// Most recent `limit` samples for a place+kind, returned oldest-first for
/// left-to-right plotting.
pub async fn list_history(
    pool: &SqlitePool,
    user_id: i64,
    place_id: i64,
    kind: &str,
    limit: i64,
) -> Result<Vec<HistoryPoint>> {
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT target_time, mean FROM score_history \
         WHERE user_id = ? AND place_id = ? AND kind = ? \
         ORDER BY target_time DESC LIMIT ?",
    )
    .bind(user_id)
    .bind(place_id)
    .bind(kind)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .rev()
        .map(|(t, m)| HistoryPoint {
            target_time: t,
            mean: m.clamp(0, 100) as u8,
        })
        .collect())
}

// ---- helpers ----

/// Empty string → SQL NULL.
fn opt(s: &str) -> Option<&str> {
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn parse_language(s: &str) -> Language {
    match s.trim().to_ascii_lowercase().as_str() {
        "swedish" | "svenska" | "sv" | "se" => Language::Swedish,
        _ => Language::Norwegian,
    }
}

const WEEKDAYS: [(&str, Workday); 7] = [
    ("mon", Workday::Mon),
    ("tue", Workday::Tue),
    ("wed", Workday::Wed),
    ("thu", Workday::Thu),
    ("fri", Workday::Fri),
    ("sat", Workday::Sat),
    ("sun", Workday::Sun),
];

fn parse_days(csv: &str) -> Vec<Workday> {
    csv.split(',')
        .filter_map(|tok| {
            let t = tok.trim().to_ascii_lowercase();
            WEEKDAYS.iter().find(|(k, _)| *k == t).map(|(_, d)| *d)
        })
        .collect()
}

/// Filter to known tokens and emit them in Mon→Sun order (dedup + validate).
fn canonical_days_csv(days: &[String]) -> String {
    let lower: Vec<String> = days.iter().map(|d| d.trim().to_ascii_lowercase()).collect();
    WEEKDAYS
        .iter()
        .filter(|(k, _)| lower.iter().any(|d| d == k))
        .map(|(k, _)| *k)
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn temp_pool() -> (tempfile::TempDir, SqlitePool) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        let url = format!("sqlite://{}", path.display());
        let pool = init_pool(&url).await.unwrap();
        (dir, pool)
    }

    #[tokio::test]
    async fn migrations_seed_a_usable_default_config() {
        let (_dir, pool) = temp_pool().await;
        let cfg = load_config(&pool, DEFAULT_USER_ID).await.unwrap();
        assert_eq!(cfg.user_agent_contact, "CHANGE_ME@example.com");
        assert_eq!(cfg.default_place.as_deref(), Some("oslo"));
        assert_eq!(cfg.language, Language::Norwegian);
        assert!(cfg.places.contains_key("oslo"));
        // Default daytime window is 10:00–22:00 (720 minutes).
        assert_eq!(cfg.daytime_window.duration_minutes(), 720);
        assert!(!cfg.work_hours.enabled);
    }

    #[tokio::test]
    async fn place_crud_and_default_place_round_trip() {
        let (_dir, pool) = temp_pool().await;
        // Add Bergen and make it the default.
        let id = upsert_place(
            &pool,
            DEFAULT_USER_ID,
            &PlaceDto {
                id: None,
                name: "bergen".into(),
                lat: 60.39,
                lon: 5.32,
                radius_km: 12.5,
                frost_source_id: "SN50540".into(),
            },
        )
        .await
        .unwrap();
        assert!(id > 0);

        let places = list_places(&pool, DEFAULT_USER_ID).await.unwrap();
        assert!(places
            .iter()
            .any(|p| p.name == "bergen" && p.radius_km == 12.5));

        let mut prefs = get_prefs(&pool, DEFAULT_USER_ID).await.unwrap();
        prefs.default_place = "bergen".into();
        prefs.language = "swedish".into();
        update_prefs(&pool, DEFAULT_USER_ID, &prefs).await.unwrap();

        let cfg = load_config(&pool, DEFAULT_USER_ID).await.unwrap();
        assert_eq!(cfg.default_place.as_deref(), Some("bergen"));
        assert_eq!(cfg.language, Language::Swedish);
        assert_eq!(
            cfg.places
                .get("bergen")
                .and_then(|p| p.frost_source_id.as_deref()),
            Some("SN50540")
        );

        delete_place(&pool, DEFAULT_USER_ID, id).await.unwrap();
        let places = list_places(&pool, DEFAULT_USER_ID).await.unwrap();
        assert!(!places.iter().any(|p| p.name == "bergen"));
    }

    #[tokio::test]
    async fn work_hours_round_trip() {
        let (_dir, pool) = temp_pool().await;
        update_work_hours(
            &pool,
            DEFAULT_USER_ID,
            &WorkHoursDto {
                enabled: true,
                // Out of order + an unknown token — should canonicalise + drop.
                days: vec!["wed".into(), "mon".into(), "xyz".into()],
                window_start: "09:00".into(),
                window_end: "16:00".into(),
            },
        )
        .await
        .unwrap();

        let wh = get_work_hours(&pool, DEFAULT_USER_ID).await.unwrap();
        assert!(wh.enabled);
        assert_eq!(wh.days, vec!["mon".to_string(), "wed".to_string()]);
        assert_eq!(wh.window_start, "09:00");

        let cfg = load_config(&pool, DEFAULT_USER_ID).await.unwrap();
        assert!(cfg.work_hours.enabled);
        assert_eq!(cfg.work_hours.days.len(), 2);
    }
}
