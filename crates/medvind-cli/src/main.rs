mod aggregate;
mod config;
mod output;
mod run;

use std::path::PathBuf;
use std::time::Duration as StdDuration;

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Local, NaiveTime, TimeZone, Utc};
use clap::{Parser, Subcommand};
use medvind_core::geo::Point;
use medvind_core::types::{Location, RideWindow};
use medvind_met::client::{MetClient, MetClientConfig, UserAgent};
use url::Url;

use crate::config::Config;
use crate::run::{run_score, ScoreInputs};

const APP: &str = env!("CARGO_PKG_NAME");
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// medvind — Grusindeks for sykling på grus.
#[derive(Debug, Parser)]
#[command(name = "medvind", version, about, long_about = None)]
struct Cli {
    /// Path to config file. Defaults to ~/.config/medvind/config.toml.
    #[arg(long, global = true, env = "MEDVIND_CONFIG")]
    config: Option<PathBuf>,

    /// Override api.met.no base URL — useful for tests against a wiremock.
    #[arg(long, global = true, env = "MEDVIND_API_BASE", hide = true)]
    api_base: Option<Url>,

    /// Override frost.met.no base URL.
    #[arg(long, global = true, env = "MEDVIND_FROST_BASE", hide = true)]
    frost_base: Option<Url>,

    /// Print sub-score breakdown and per-point detail.
    #[arg(long, short, global = true)]
    verbose: bool,

    /// Emit machine-readable JSON instead of the formatted human view.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Compute the Grusindeks for a location and time window.
    Score {
        #[arg(long)]
        lat: Option<f64>,
        #[arg(long)]
        lon: Option<f64>,
        /// Named place from config (`places.<name>`). Mutually exclusive
        /// with --lat/--lon.
        #[arg(long)]
        place: Option<String>,
        /// Sample radius in km. Defaults to the place's radius_km, else 20.
        #[arg(long = "radius-km")]
        radius_km: Option<f64>,
        /// Window like "14:00-17:00" in local time today.
        #[arg(long)]
        window: Option<String>,
        /// Window length in hours when --window is omitted.
        #[arg(long, default_value_t = 3)]
        hours: i64,
    },
    /// Manage the configuration file.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
}

#[derive(Debug, Subcommand)]
enum ConfigAction {
    /// Write a starter config to ~/.config/medvind/config.toml.
    Init,
    /// Print the resolved config path.
    Path,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    match &cli.command {
        Command::Config {
            action: ConfigAction::Init,
        } => cmd_config_init(cli.config.as_deref()),
        Command::Config {
            action: ConfigAction::Path,
        } => {
            let p = cli
                .config
                .clone()
                .map(Ok)
                .unwrap_or_else(Config::default_path)?;
            println!("{}", p.display());
            Ok(())
        }
        Command::Score {
            lat,
            lon,
            place,
            radius_km,
            window,
            hours,
        } => {
            cmd_score(
                &cli,
                *lat,
                *lon,
                place.clone(),
                *radius_km,
                window.clone(),
                *hours,
            )
            .await
        }
    }
}

fn cmd_config_init(config_arg: Option<&std::path::Path>) -> Result<()> {
    let path = match config_arg {
        Some(p) => p.to_path_buf(),
        None => Config::default_path()?,
    };
    if path.exists() {
        bail!("{} already exists — refusing to overwrite", path.display());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(&path, Config::example_template())
        .with_context(|| format!("writing {}", path.display()))?;
    println!("Wrote {}", path.display());
    println!("Edit it to set user_agent_contact and (optionally) Frost client_id.");
    Ok(())
}

async fn cmd_score(
    cli: &Cli,
    lat: Option<f64>,
    lon: Option<f64>,
    place: Option<String>,
    radius_km: Option<f64>,
    window: Option<String>,
    hours: i64,
) -> Result<()> {
    let cfg_path = match cli.config.clone() {
        Some(p) => p,
        None => Config::default_path()?,
    };
    let cfg = Config::load_from(&cfg_path).with_context(|| {
        format!(
            "load config from {}\n\nRun `medvind config init` to scaffold one.",
            cfg_path.display()
        )
    })?;

    let location = resolve_location(&cfg, lat, lon, place, radius_km)?;
    let win = resolve_window(window.as_deref(), hours)?;
    let frost_source_id = location_frost_source(&cfg, &location);

    let client = build_client(&cfg, cli.api_base.as_ref(), cli.frost_base.as_ref())?;
    let agg = run_score(
        &client,
        ScoreInputs {
            center: location.center,
            radius_km: location.radius_km,
            window: win,
            frost_source_id: frost_source_id.as_deref(),
            history_hours: 48,
        },
    )
    .await?;

    if cli.json {
        let v = serde_json::json!({
            "location": location,
            "window": win,
            "aggregate": agg,
        });
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else {
        let body = output::render_human(&location.name, location.radius_km, win, &agg);
        print!("{body}");
    }
    Ok(())
}

fn resolve_location(
    cfg: &Config,
    lat: Option<f64>,
    lon: Option<f64>,
    place: Option<String>,
    radius_km: Option<f64>,
) -> Result<Location> {
    if (lat.is_some() || lon.is_some()) && place.is_some() {
        bail!("--lat/--lon and --place are mutually exclusive");
    }

    let place_name = place.or_else(|| cfg.default_place.clone());

    if let (Some(lat), Some(lon)) = (lat, lon) {
        return Ok(Location {
            name: format!("{lat:.4},{lon:.4}"),
            center: Point::new(lat, lon),
            radius_km: radius_km.unwrap_or(20.0),
        });
    }
    if lat.is_some() || lon.is_some() {
        bail!("--lat and --lon must be given together");
    }
    if let Some(name) = place_name {
        let p = cfg.places.get(&name).ok_or_else(|| {
            anyhow!(
                "no place named '{name}' in config — known: {:?}",
                cfg.places.keys().collect::<Vec<_>>()
            )
        })?;
        return Ok(Location {
            name,
            center: p.point(),
            radius_km: radius_km.unwrap_or(p.radius_km),
        });
    }
    bail!("specify --lat and --lon, or --place, or set default_place in config")
}

fn location_frost_source(cfg: &Config, loc: &Location) -> Option<String> {
    cfg.places
        .get(&loc.name)
        .and_then(|p| p.frost_source_id.clone())
        .or_else(|| cfg.frost.source_id.clone())
}

fn resolve_window(window: Option<&str>, hours: i64) -> Result<RideWindow> {
    if let Some(w) = window {
        let (start_s, end_s) = w
            .split_once('-')
            .ok_or_else(|| anyhow!("--window must be HH:MM-HH:MM, got {w:?}"))?;
        let start_t = NaiveTime::parse_from_str(start_s.trim(), "%H:%M")
            .with_context(|| format!("parsing window start {start_s:?}"))?;
        let end_t = NaiveTime::parse_from_str(end_s.trim(), "%H:%M")
            .with_context(|| format!("parsing window end {end_s:?}"))?;
        let today = Local::now().date_naive();
        let start_local = today.and_time(start_t);
        let end_local = today.and_time(end_t);
        let start = local_to_utc(start_local)?;
        let end = local_to_utc(end_local)?;
        if end <= start {
            bail!("--window end must be after start");
        }
        return Ok(RideWindow { start, end });
    }
    Ok(RideWindow::from_hours(Utc::now(), hours))
}

fn local_to_utc(naive: chrono::NaiveDateTime) -> Result<DateTime<Utc>> {
    match Local.from_local_datetime(&naive).single() {
        Some(t) => Ok(t.with_timezone(&Utc)),
        None => bail!("ambiguous local time {naive}"),
    }
}

fn build_client(
    cfg: &Config,
    api_base: Option<&Url>,
    frost_base: Option<&Url>,
) -> Result<MetClient> {
    let ua = UserAgent::new(APP, VERSION, &cfg.user_agent_contact)
        .map_err(|e| anyhow!("invalid User-Agent (check user_agent_contact): {e}"))?;
    let mut mcfg = MetClientConfig::production(ua, cfg.frost.client_id.clone());
    if let Some(u) = api_base {
        mcfg.api_base = u.clone();
    }
    if let Some(u) = frost_base {
        mcfg.frost_base = u.clone();
    }
    mcfg.timeout = StdDuration::from_secs(15);
    Ok(MetClient::new(mcfg)?)
}
