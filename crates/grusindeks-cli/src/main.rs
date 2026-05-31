use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use chrono::{Local, Utc};
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{generate, Shell};
use url::Url;

use grusindeks_core::daily::BestWindowConfig;

use grusindeks_cli::config::Config;
use grusindeks_cli::output::{self, ChipFlags};
use grusindeks_cli::progress::TerminalProgress;
use grusindeks_cli::run::{
    run_forecast, run_hourly, run_score, ForecastInputs, HourlyInputs, ScoreInputs,
};
use grusindeks_cli::windows::{
    build_client, build_day_windows, build_work_hour_exclusions, daytime_header_hours,
    location_frost_source, resolve_location, resolve_window, window_starts_within_nowcast_horizon,
    DEFAULT_FORECAST_DAYS, MAX_FORECAST_DAYS, MAX_HOURS,
};

const APP: &str = env!("CARGO_PKG_NAME");
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// grusindeks — Grusindeks for sykling på grus.
///
/// Kjør `grusindeks` uten argumenter for å se en seks-dagers oversikt fra og
/// med i dag. Spesifiser `--window` / `--hours` for et enkelt tidsvindu, eller
/// kjør `grusindeks config init` for å sette opp.
///
/// Defaulting:
/// * No `--window` and no `--hours` set → multi-day (`--days` or 6).
/// * `--window` set → single-day with that window.
/// * `--hours` set (without `--window`) → single-day with `now..now+hours`.
#[derive(Debug, Default, Parser)]
#[command(name = "grusindeks", version, about, long_about = None)]
struct Cli {
    /// Path to config file. Defaults to ~/.config/grusindeks/config.toml.
    #[arg(long, global = true, env = "GRUSINDEKS_CONFIG")]
    config: Option<PathBuf>,

    /// Override api.met.no base URL — useful for tests against a wiremock.
    #[arg(long, global = true, env = "GRUSINDEKS_API_BASE", hide = true)]
    api_base: Option<Url>,

    /// Override frost.met.no base URL.
    #[arg(long, global = true, env = "GRUSINDEKS_FROST_BASE", hide = true)]
    frost_base: Option<Url>,

    /// Print sub-score breakdown and per-point detail.
    #[arg(long, short, global = true)]
    verbose: bool,

    /// Emit machine-readable JSON instead of the formatted human view.
    #[arg(long, global = true)]
    json: bool,

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
    /// Window length in hours. Implies single-day mode. Must be 1–24
    /// — anything longer would cross more than one local day and the
    /// renderer's HH:MM endpoint format would silently swallow the
    /// extra dates.
    #[arg(long, value_parser = clap::value_parser!(i64).range(1..=MAX_HOURS))]
    hours: Option<i64>,
    /// Antall dager fremover i prognosen (default 6: i dag + 5).
    /// Tvinger fram dag-for-dag-sammendrag; --window og --hours kan
    /// ikke kombineres med dette. Maks 9 — det er den publiserte
    /// horisonten på api.met.no/locationforecast.
    #[arg(long, value_parser = clap::value_parser!(u8).range(1..=MAX_FORECAST_DAYS as i64))]
    days: Option<u8>,
    /// Vis det beste N-timers vinduet for hver dag i multi-dags-prognosen
    /// (uavhengig av hvor mye bedre det er enn dagsgjennomsnittet).
    /// Uten verdi: 2 timer.
    #[arg(long = "best-window", num_args = 0..=1, default_missing_value = "2", value_name = "TIMER", value_parser = clap::value_parser!(i64).range(1..=MAX_HOURS))]
    best_window: Option<i64>,
    /// Ignorer work_hours fra config når --best-window velger vinduer.
    #[arg(long = "include-work-hours")]
    include_work_hours: bool,
    /// Vis time-for-time-score for alle dagene i prognosen, begrenset til
    /// dag-vinduet i config (default 10:00–22:00). Kan kombineres med
    /// --days; ikke med --window/--hours/--best-window.
    #[arg(long)]
    hourly: bool,

    /// Skjul "Regn 7d"-footer-chipen for denne kjøringen.
    /// Overstyrer `show_rain_history = true` i config.
    #[arg(long = "no-rain-history")]
    no_rain_history: bool,

    /// Skjul "Tall"-footer-chipen for denne kjøringen.
    /// Overstyrer `show_window_stats = true` i config.
    #[arg(long = "no-window-stats")]
    no_window_stats: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Manage the configuration file.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Generate shell completion script.
    Completions {
        /// Shell to generate completions for.
        shell: Shell,
    },
}

#[derive(Debug, Subcommand)]
enum ConfigAction {
    /// Write a starter config to ~/.config/grusindeks/config.toml.
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
        Some(Command::Config {
            action: ConfigAction::Init,
        }) => cmd_config_init(cli.config.as_deref()),
        Some(Command::Config {
            action: ConfigAction::Path,
        }) => {
            let p = cli
                .config
                .clone()
                .map(Ok)
                .unwrap_or_else(Config::default_path)?;
            println!("{}", p.display());
            Ok(())
        }
        Some(Command::Completions { shell }) => {
            let mut cmd = Cli::command();
            let name = cmd.get_name().to_owned();
            generate(*shell, &mut cmd, name, &mut std::io::stdout());
            Ok(())
        }
        // No subcommand → score the configured `default_place` (or whatever
        // location the user passed via --lat/--lon/--place). Default with
        // no time arguments is the six-day forecast.
        None => cmd_score(&cli).await,
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

async fn cmd_score(cli: &Cli) -> Result<()> {
    // Resolve which mode (single-day window vs multi-day forecast) the
    // user is asking for. `--window` and `--hours` both imply single-day;
    // anything else falls into the new default (six-day forecast).
    let single_day = cli.window.is_some() || cli.hours.is_some();
    if cli.hourly && single_day {
        bail!("--hourly kan ikke brukes sammen med --window/--hours");
    }
    if cli.hourly && cli.best_window.is_some() {
        bail!("--hourly kan ikke brukes sammen med --best-window");
    }
    let days = match (single_day, cli.days) {
        (true, Some(_)) => bail!("--window/--hours kan ikke brukes sammen med --days"),
        (true, None) => 1,
        (false, Some(d)) => d,
        (false, None) => DEFAULT_FORECAST_DAYS,
    };
    if days > 1 && cli.window.is_some() {
        bail!("--window kan ikke brukes sammen med --days > 1");
    }
    if single_day && cli.best_window.is_some() {
        bail!("--best-window gjelder bare multi-dags-prognosen — kan ikke kombineres med --window/--hours");
    }
    let mut best_window = match cli.best_window {
        Some(h) => BestWindowConfig {
            length_hours: h,
            // Override the default improvement filter so every day surfaces
            // its top-scoring sub-window, even on uniform days where the
            // window only matches the day mean.
            min_improvement: 0,
            excluded_windows: Vec::new(),
        },
        None => BestWindowConfig::default(),
    };

    let cfg_path = match cli.config.clone() {
        Some(p) => p,
        None => Config::default_path()?,
    };
    let cfg = Config::load_from(&cfg_path).with_context(|| {
        format!(
            "load config from {}\n\nRun `grusindeks config init` to scaffold one.",
            cfg_path.display()
        )
    })?;

    if cli.best_window.is_some() && !cli.include_work_hours {
        best_window.excluded_windows =
            build_work_hour_exclusions(Local::now().date_naive(), days, &cfg.work_hours);
    }

    let location = resolve_location(&cfg, cli.lat, cli.lon, cli.place.clone(), cli.radius_km)?;
    let frost_source_id = location_frost_source(&cfg, &location);
    let client = build_client(
        APP,
        VERSION,
        &cfg,
        cli.api_base.as_ref(),
        cli.frost_base.as_ref(),
        None,
    )?;

    // CLI --no-* flags trump the config booleans. The flag is *opt-out*
    // (presence = hide), so the effective state is "config says yes AND
    // user did not opt out".
    let chip_flags = ChipFlags {
        rain_history: cfg.show_rain_history && !cli.no_rain_history,
        window_stats: cfg.show_window_stats && !cli.no_window_stats,
    };

    // Warn loudly when --best-window can never fit inside the configured
    // daytime window. Without this the renderer just omits the line and
    // the user is left wondering why nothing showed up.
    if let Some(bw) = cli.best_window {
        let daytime_minutes = cfg.daytime_window.duration_minutes();
        let bw_minutes = bw * 60;
        if bw_minutes > daytime_minutes {
            let h = daytime_minutes / 60;
            let m = daytime_minutes % 60;
            eprintln!(
                "warning: --best-window {bw}t er lengre enn dag-vinduet ({h}t {m:02}m). \
                 Ingen vindu vil bli foreslått. Juster `daytime_window` i config eller velg et kortere --best-window."
            );
        }
    }

    // Hourly mode reuses the multi-day data path but scores 1-h buckets
    // inside each clipped daytime window. Stays separate from the regular
    // multi-day path so the two views render independently.
    if cli.hourly {
        let day_windows = build_day_windows(
            Local::now().date_naive(),
            days,
            Utc::now(),
            cfg.daytime_window,
        )?;
        let header_hours = daytime_header_hours(cfg.daytime_window);
        let fetch_nowcast = day_windows
            .first()
            .is_some_and(|d| window_starts_within_nowcast_horizon(d.window, Utc::now()));
        let progress = TerminalProgress::new();
        let result = run_hourly(
            &client,
            HourlyInputs {
                center: location.center,
                radius_km: location.radius_km,
                days: day_windows,
                frost_source_id: frost_source_id.as_deref(),
                history_hours: 168,
                lang: cfg.language,
                header_hours,
                fetch_nowcast,
                progress: &progress,
            },
        )
        .await;
        progress.finish();
        let hourly = result?;
        if cli.json {
            let v = serde_json::json!({
                "location": location,
                "hourly": hourly,
            });
            println!("{}", serde_json::to_string_pretty(&v)?);
        } else {
            let body = output::render_hourly(
                &location.name,
                location.radius_km,
                &hourly,
                cli.verbose,
                chip_flags,
                cfg.language,
            );
            print!("{body}");
        }
        return Ok(());
    }

    // Route through the multi-day path whenever the user didn't pass
    // --window/--hours. `--days 1` is a legitimate "today only, full
    // daytime window" request and falling through to single-day would
    // silently ignore both the configured daytime_window and
    // --best-window (single-day defaults to a 3h window starting at now).
    if !single_day {
        let day_windows = build_day_windows(
            Local::now().date_naive(),
            days,
            Utc::now(),
            cfg.daytime_window,
        )?;
        let fetch_nowcast = day_windows
            .first()
            .is_some_and(|d| window_starts_within_nowcast_horizon(d.window, Utc::now()));
        let progress = TerminalProgress::new();
        let result = run_forecast(
            &client,
            ForecastInputs {
                center: location.center,
                radius_km: location.radius_km,
                days: day_windows,
                frost_source_id: frost_source_id.as_deref(),
                history_hours: 168,
                lang: cfg.language,
                best_window,
                fetch_nowcast,
                progress: &progress,
            },
        )
        .await;
        progress.finish();
        let forecast = result?;
        if cli.json {
            let v = serde_json::json!({
                "location": location,
                "forecast": forecast,
            });
            println!("{}", serde_json::to_string_pretty(&v)?);
        } else {
            let body = output::render_multi_day(
                &location.name,
                location.radius_km,
                &forecast,
                cli.verbose,
                chip_flags,
                cfg.language,
            );
            print!("{body}");
        }
        return Ok(());
    }

    let win = resolve_window(cli.window.as_deref(), cli.hours.unwrap_or(3))?;
    let fetch_nowcast = window_starts_within_nowcast_horizon(win, Utc::now());
    let progress = TerminalProgress::new();
    let result = run_score(
        &client,
        ScoreInputs {
            center: location.center,
            radius_km: location.radius_km,
            window: win,
            frost_source_id: frost_source_id.as_deref(),
            history_hours: 168,
            lang: cfg.language,
            fetch_nowcast,
            progress: &progress,
        },
    )
    .await;
    progress.finish();
    let agg = result?;

    if cli.json {
        let v = serde_json::json!({
            "location": location,
            "window": win,
            "aggregate": agg,
        });
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else {
        let body = output::render_human(
            &location.name,
            location.radius_km,
            win,
            &agg,
            cli.verbose,
            chip_flags,
            cfg.language,
        );
        print!("{body}");
    }
    Ok(())
}
