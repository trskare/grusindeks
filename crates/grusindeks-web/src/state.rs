//! Server-side application state, injected into Leptos server functions.
//!
//! Holds the SQLite pool and the shared [`MetClient`]. Config lives in the DB
//! (loaded per request via [`crate::db::load_config`]); the client is cached in
//! an [`ArcSwap`] and rebuilt only when the contact / Frost credentials that
//! feed its User-Agent change ([`AppState::rebuild_client`]).

use std::sync::Arc;

use anyhow::{Context, Result};
use arc_swap::ArcSwap;
use grusindeks_cli::config::Config;
use grusindeks_cli::windows::build_client;
use grusindeks_met::client::MetClient;
use sqlx::SqlitePool;

use crate::db::{self, DEFAULT_USER_ID};

const APP: &str = env!("CARGO_PKG_NAME");
const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub client: Arc<ArcSwap<MetClient>>,
}

impl AppState {
    /// Open the database (running migrations), apply the optional
    /// `GRUSINDEKS_CONTACT` startup override, and build the MET client from the
    /// stored config.
    pub async fn init() -> Result<Self> {
        let db_path =
            std::env::var("GRUSINDEKS_DB").unwrap_or_else(|_| "sqlite://grusindeks.db".to_string());
        let pool = db::init_pool(&db_path).await?;

        // Convenience for dev/deploy: if GRUSINDEKS_CONTACT is set, persist it
        // so a fresh DB doesn't ship the placeholder contact to MET. We never
        // fall back to a personal address.
        if let Ok(contact) = std::env::var("GRUSINDEKS_CONTACT") {
            if !contact.trim().is_empty() {
                sqlx::query("UPDATE prefs SET user_agent_contact = ? WHERE user_id = ?")
                    .bind(contact)
                    .bind(DEFAULT_USER_ID)
                    .execute(&pool)
                    .await
                    .context("apply GRUSINDEKS_CONTACT override")?;
            }
        }

        let cfg = db::load_config(&pool, DEFAULT_USER_ID).await?;
        let client = build_client_from_cfg(&cfg)?;
        Ok(Self {
            db: pool,
            client: Arc::new(ArcSwap::from_pointee(client)),
        })
    }

    /// Current MET client (cheap clone of the `reqwest`-backed handle).
    pub fn client(&self) -> MetClient {
        (*self.client.load_full()).clone()
    }

    /// Reload config from the DB and swap in a freshly built client. Call after
    /// a prefs change that affects the User-Agent contact or Frost credentials.
    pub async fn rebuild_client(&self) -> Result<()> {
        let cfg = db::load_config(&self.db, DEFAULT_USER_ID).await?;
        let client = build_client_from_cfg(&cfg)?;
        self.client.store(Arc::new(client));
        Ok(())
    }
}

fn build_client_from_cfg(cfg: &Config) -> Result<MetClient> {
    let api_base = std::env::var("GRUSINDEKS_API_BASE")
        .ok()
        .and_then(|s| s.parse().ok());
    let frost_base = std::env::var("GRUSINDEKS_FROST_BASE")
        .ok()
        .and_then(|s| s.parse().ok());
    // Keep the MET disk cache on the data volume so it survives restarts.
    let cache_dir = std::env::var("GRUSINDEKS_CACHE_DIR")
        .ok()
        .map(std::path::PathBuf::from);
    build_client(
        APP,
        VERSION,
        cfg,
        api_base.as_ref(),
        frost_base.as_ref(),
        cache_dir,
    )
}
