mod aggregate;
mod config;
mod output;
mod progress;
mod run;
mod theme;

use std::path::PathBuf;
use std::time::Duration as StdDuration;

use anyhow::{anyhow, bail, Context, Result};
use chrono::{
    DateTime, Datelike, Duration as ChronoDuration, Local, NaiveDate, NaiveTime, TimeZone, Utc,
};
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{generate, Shell};
use grusindeks_core::geo::Point;
use grusindeks_core::types::{Location, RideWindow};
use grusindeks_met::client::{MetClient, MetClientConfig, UserAgent};
use url::Url;

use grusindeks_core::daily::BestWindowConfig;

use crate::config::{Config, DaytimeWindow, WorkHoursConfig};
use crate::progress::TerminalProgress;
use crate::run::{
    run_forecast, run_hourly, run_score, DayWindow, ForecastInputs, HourlyInputs, ScoreInputs,
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
    let client = build_client(&cfg, cli.api_base.as_ref(), cli.frost_base.as_ref())?;

    // CLI --no-* flags trump the config booleans. The flag is *opt-out*
    // (presence = hide), so the effective state is "config says yes AND
    // user did not opt out".
    let chip_flags = output::ChipFlags {
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

/// Nowcast covers ~2 hours of radar-based extrapolation. Beyond that the
/// signal merges with locationforecast, so paying for an extra HTTP call
/// stops being worth it. Used to gate `fetch_nowcast` on each ride window.
const NOWCAST_HORIZON: ChronoDuration = ChronoDuration::hours(2);

/// True when the window we're about to score overlaps the nowcast's
/// reliable horizon. We require both: (a) the window has at least some
/// future-time component (`end > now`, otherwise we'd be scoring a past
/// ride), and (b) `start` is within `NOWCAST_HORIZON` of `now` so the
/// fetched radar series actually covers the early portion of the window.
fn window_starts_within_nowcast_horizon(window: RideWindow, now: DateTime<Utc>) -> bool {
    window.end > now && window.start <= now + NOWCAST_HORIZON
}

/// Default horizon when the user runs `grusindeks` with no time arguments
/// — today plus the next five days.
const DEFAULT_FORECAST_DAYS: u8 = 6;

/// Hard cap for `--days`. `api.met.no/locationforecast/2.0/complete`
/// publishes a 9-day horizon; anything beyond would render as duplicate
/// "·" placeholder days that look like real data.
const MAX_FORECAST_DAYS: u8 = 9;

/// Hard cap for `--hours` and `--best-window`. 24h is the longest single
/// ride window we'll honour; longer windows cross multiple local days,
/// at which point the multi-day path (`--days`) is the right tool and
/// the renderer's HH:MM-only endpoint would mislead.
const MAX_HOURS: i64 = 24;

/// Build `n` consecutive day windows starting at `start_date` (local).
///
/// Each day spans the configured `daytime` window in local time — the
/// hours someone might actually ride. Including the cold dawn / late
/// night would drag the daily mean toward unrepresentative values.
/// "Today" is additionally clipped to start at `now` instead of the
/// configured daytime start when that's already in the past — anything
/// before `now` is history. If today's daytime has fully ended by `now`,
/// today is dropped from the forecast.
/// Local-clock hour values that an hourly grid should label as columns,
/// derived from the configured daytime window. A 10:00–22:00 window
/// yields `[10, 11, …, 21]` — every hour bucket whose start is ≥ start
/// and whose end (start + 1 h) is ≤ end. Minute offsets in the config
/// (e.g. 09:30) are rounded *inward* so we never claim a column the
/// 1-hour bucket couldn't actually fill.
fn daytime_header_hours(daytime: DaytimeWindow) -> Vec<u8> {
    use chrono::Timelike;
    // First whole-hour bucket whose start is ≥ daytime.start.
    let first = if daytime.start.minute() == 0 {
        daytime.start.hour()
    } else {
        daytime.start.hour() + 1
    };
    // Last bucket end (= bucket_start + 1) must be ≤ daytime.end.
    let last_end_hour = if daytime.end.minute() == 0 {
        daytime.end.hour()
    } else {
        // 21:30 still allows a bucket [20, 21) but not [21, 22).
        daytime.end.hour()
    };
    if last_end_hour == 0 || first >= last_end_hour {
        return Vec::new();
    }
    (first..last_end_hour).map(|h| h as u8).collect()
}

fn build_day_windows(
    start_date: NaiveDate,
    n: u8,
    now: DateTime<Utc>,
    daytime: DaytimeWindow,
) -> Result<Vec<DayWindow>> {
    let mut out = Vec::with_capacity(n as usize);
    let max_offsets = if n < MAX_FORECAST_DAYS { n + 1 } else { n };
    for offset in 0..i64::from(max_offsets) {
        if out.len() >= n as usize {
            break;
        }
        let date = start_date + ChronoDuration::days(offset);
        let day_start = local_to_utc(date.and_time(daytime.start))?;
        let day_end = local_to_utc(date.and_time(daytime.end))?;
        if day_end <= now {
            // Day's daytime has fully ended (today already past 22:00).
            continue;
        }
        let start = if day_start < now { now } else { day_start };
        if !has_forecast_hour_in_window(start, day_end) {
            // MET forecast buckets are whole-hour timestamps. If the
            // clipped remainder of today is shorter than the next full
            // bucket (e.g. now 21:30, window ends 22:00), scoring would
            // only produce the "no data" placeholder 0. Hide that day
            // instead — the ride window is effectively over.
            continue;
        }
        out.push(DayWindow {
            date,
            window: RideWindow {
                start,
                end: day_end,
            },
        });
    }
    Ok(out)
}

fn build_work_hour_exclusions(
    start_date: NaiveDate,
    n: u8,
    work_hours: &WorkHoursConfig,
) -> Vec<RideWindow> {
    if !work_hours.enabled {
        return Vec::new();
    }
    let work_days: Vec<_> = work_hours.days.iter().copied().map(Into::into).collect();
    let max_offsets = if n < MAX_FORECAST_DAYS { n + 1 } else { n };
    (0..i64::from(max_offsets))
        .filter_map(|offset| {
            let date = start_date + ChronoDuration::days(offset);
            if !work_days.contains(&date.weekday()) {
                return None;
            }
            let start = local_to_utc(date.and_time(work_hours.window.start)).ok()?;
            let end = local_to_utc(date.and_time(work_hours.window.end)).ok()?;
            Some(RideWindow { start, end })
        })
        .collect()
}

fn has_forecast_hour_in_window(start: DateTime<Utc>, end: DateTime<Utc>) -> bool {
    next_whole_hour_at_or_after(start) < end
}

fn next_whole_hour_at_or_after(t: DateTime<Utc>) -> DateTime<Utc> {
    let ts = t.timestamp();
    let exactly_on_hour = ts.rem_euclid(3600) == 0 && t.timestamp_subsec_nanos() == 0;
    if exactly_on_hour {
        return t;
    }
    let next_ts = (ts.div_euclid(3600) + 1) * 3600;
    DateTime::from_timestamp(next_ts, 0).expect("rounded timestamp should be representable")
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
    use chrono::LocalResult;
    match Local.from_local_datetime(&naive) {
        LocalResult::Single(t) => Ok(t.with_timezone(&Utc)),
        // Spring-forward gap: the wall-clock time literally doesn't exist
        // (e.g. 02:30 on the morning Norway moves to summer time). Saying
        // "ambiguous" here is wrong — the clock skips this minute.
        LocalResult::None => bail!(
            "lokal tid {naive} finnes ikke (sommer-/vintertid-overgang); velg et tidspunkt før eller etter overgangen"
        ),
        // Fall-back overlap: 02:30 happens twice on the morning we wind
        // clocks back. We pick neither — let the user pin the right one.
        LocalResult::Ambiguous(_, _) => bail!(
            "lokal tid {naive} er tvetydig (forekommer to ganger ved sommer-/vintertid-overgang)"
        ),
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
    let api_base_overridden = api_base.is_some();
    if let Some(u) = api_base {
        mcfg.api_base = u.clone();
    }
    if let Some(u) = frost_base {
        mcfg.frost_base = u.clone();
    }
    mcfg.timeout = StdDuration::from_secs(15);
    // Cache only the anonymous api.met.no endpoints — Frost stays off the
    // disk cache because of basic auth and per-request time-range
    // semantics. Skip caching entirely when api_base is overridden
    // (typically a wiremock from the integration suite); otherwise the
    // tests would pollute the user's real cache directory and revalidation
    // traffic would surprise the mocks. A failure to derive the cache
    // dir is also non-fatal: drop back to the un-cached path with a
    // warning so the CLI still works on environments where ProjectDirs
    // can't resolve a home.
    if !api_base_overridden {
        match Config::default_cache_dir() {
            Ok(dir) => mcfg.cache_dir = Some(dir),
            Err(e) => tracing::warn!("disk cache disabled — could not resolve cache dir: {e}"),
        }
    }
    Ok(MetClient::new(mcfg)?)
}

#[cfg(test)]
mod build_day_windows_tests {
    use super::*;
    use chrono::NaiveDate;

    fn dt(date: NaiveDate, h: u32, m: u32) -> DateTime<Utc> {
        local_to_utc(date.and_hms_opt(h, m, 0).unwrap()).unwrap()
    }

    fn dw() -> DaytimeWindow {
        DaytimeWindow::default()
    }

    #[test]
    fn future_day_uses_full_daytime_window() {
        let today = NaiveDate::from_ymd_opt(2026, 4, 27).unwrap();
        let now = dt(today, 9, 0);
        let days = build_day_windows(today, 3, now, dw()).unwrap();
        let tomorrow = today.succ_opt().unwrap();
        let tomorrow_win = days.iter().find(|d| d.date == tomorrow).unwrap();
        assert_eq!(tomorrow_win.window.start, dt(tomorrow, 10, 0));
        assert_eq!(tomorrow_win.window.end, dt(tomorrow, 22, 0));
    }

    #[test]
    fn today_before_daytime_starts_at_daytime_start() {
        let today = NaiveDate::from_ymd_opt(2026, 4, 27).unwrap();
        let now = dt(today, 6, 0);
        let days = build_day_windows(today, 1, now, dw()).unwrap();
        assert_eq!(days.len(), 1);
        assert_eq!(days[0].window.start, dt(today, 10, 0));
        assert_eq!(days[0].window.end, dt(today, 22, 0));
    }

    #[test]
    fn today_during_daytime_is_clipped_to_now() {
        let today = NaiveDate::from_ymd_opt(2026, 4, 27).unwrap();
        let now = dt(today, 14, 30);
        let days = build_day_windows(today, 1, now, dw()).unwrap();
        assert_eq!(days[0].window.start, now);
        assert_eq!(days[0].window.end, dt(today, 22, 0));
    }

    #[test]
    fn today_dropped_when_past_daytime_end() {
        let today = NaiveDate::from_ymd_opt(2026, 4, 27).unwrap();
        let tomorrow = today.succ_opt().unwrap();
        let now = dt(today, 22, 30);
        let days = build_day_windows(today, 2, now, dw()).unwrap();
        // Today should be dropped, tomorrow should still appear in full.
        assert!(
            !days.iter().any(|d| d.date == today),
            "today should be dropped"
        );
        let tw = days.iter().find(|d| d.date == tomorrow).unwrap();
        assert_eq!(tw.window.start, dt(tomorrow, 10, 0));
    }

    #[test]
    fn today_dropped_when_no_whole_forecast_hour_remains() {
        let today = NaiveDate::from_ymd_opt(2026, 4, 27).unwrap();
        let tomorrow = today.succ_opt().unwrap();
        let now = dt(today, 21, 30);
        let days = build_day_windows(today, 2, now, dw()).unwrap();
        assert!(
            !days.iter().any(|d| d.date == today),
            "today should be hidden instead of rendered as no-data score 0"
        );
        let tw = days.iter().find(|d| d.date == tomorrow).unwrap();
        assert_eq!(tw.window.start, dt(tomorrow, 10, 0));
    }

    #[test]
    fn today_kept_when_next_whole_forecast_hour_fits() {
        let today = NaiveDate::from_ymd_opt(2026, 4, 27).unwrap();
        let now = dt(today, 20, 59);
        let days = build_day_windows(today, 1, now, dw()).unwrap();
        assert_eq!(days.len(), 1);
        assert_eq!(days[0].window.start, now);
        assert_eq!(days[0].window.end, dt(today, 22, 0));
    }

    #[test]
    fn custom_daytime_window_is_honored() {
        let today = NaiveDate::from_ymd_opt(2026, 4, 27).unwrap();
        let now = dt(today, 5, 0);
        let custom = DaytimeWindow {
            start: NaiveTime::from_hms_opt(7, 0, 0).unwrap(),
            end: NaiveTime::from_hms_opt(19, 0, 0).unwrap(),
        };
        let days = build_day_windows(today, 1, now, custom).unwrap();
        assert_eq!(days[0].window.start, dt(today, 7, 0));
        assert_eq!(days[0].window.end, dt(today, 19, 0));
    }

    #[test]
    fn work_hour_exclusions_follow_configured_weekdays() {
        let monday = NaiveDate::from_ymd_opt(2026, 4, 27).unwrap();
        let cfg = WorkHoursConfig {
            enabled: true,
            days: vec![crate::config::Workday::Mon, crate::config::Workday::Wed],
            window: DaytimeWindow {
                start: NaiveTime::from_hms_opt(8, 0, 0).unwrap(),
                end: NaiveTime::from_hms_opt(15, 0, 0).unwrap(),
            },
        };
        let exclusions = build_work_hour_exclusions(monday, 3, &cfg);
        assert_eq!(exclusions.len(), 2);
        assert_eq!(exclusions[0].start, dt(monday, 8, 0));
        assert_eq!(exclusions[0].end, dt(monday, 15, 0));
        let wednesday = monday + ChronoDuration::days(2);
        assert_eq!(exclusions[1].start, dt(wednesday, 8, 0));
    }

    #[test]
    fn work_hour_exclusions_are_empty_when_disabled() {
        let monday = NaiveDate::from_ymd_opt(2026, 4, 27).unwrap();
        let exclusions = build_work_hour_exclusions(monday, 5, &WorkHoursConfig::default());
        assert!(exclusions.is_empty());
    }

    #[test]
    fn daytime_header_hours_default_window_yields_10_to_21() {
        let hours = daytime_header_hours(DaytimeWindow::default());
        assert_eq!(
            hours,
            (10u8..22u8).collect::<Vec<_>>(),
            "10:00-22:00 should expose 12 hourly columns 10..21"
        );
    }

    #[test]
    fn daytime_header_hours_minute_offsets_are_clipped_inward() {
        // 09:30-22:30: leading bucket [09:30, 10:30) is partial → first
        // valid column is 10. Trailing bucket [22:00, 23:00) ends after
        // 22:30 → last column is 21.
        let dw = DaytimeWindow {
            start: NaiveTime::from_hms_opt(9, 30, 0).unwrap(),
            end: NaiveTime::from_hms_opt(22, 30, 0).unwrap(),
        };
        let hours = daytime_header_hours(dw);
        assert_eq!(hours.first(), Some(&10));
        assert_eq!(hours.last(), Some(&21));
    }

    #[test]
    fn daytime_header_hours_returns_empty_when_no_full_bucket_fits() {
        let dw = DaytimeWindow {
            start: NaiveTime::from_hms_opt(10, 0, 0).unwrap(),
            end: NaiveTime::from_hms_opt(10, 30, 0).unwrap(),
        };
        let hours = daytime_header_hours(dw);
        assert!(hours.is_empty());
    }
}
