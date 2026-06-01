//! Server functions — the typed RPC boundary between the Leptos client and the
//! Axum server. Bodies run server-side and reuse `grusindeks-cli` orchestration
//! together with the SQLite layer; the macro generates client stubs that fetch
//! and deserialize the wasm-safe response types.

use grusindeks_core::aggregate::{AggregateScore, HourlyForecast, MultiDayForecast};
use leptos::prelude::*;

use crate::dto::{HistoryPoint, PlaceDto, PrefsDto, WorkHoursDto};

/// No-op progress reporter for the web path (every `ProgressSink` method has a
/// no-op default, so this is zero-cost).
#[cfg(feature = "ssr")]
pub struct NoopProgress;

#[cfg(feature = "ssr")]
impl grusindeks_cli::run::ProgressSink for NoopProgress {}

/// Map any server-side error into a `ServerFnError`.
#[cfg(feature = "ssr")]
fn err<E: std::fmt::Display>(e: E) -> ServerFnError {
    ServerFnError::new(e.to_string())
}

/// Score a place over the next `hours` hours. Empty `place` → configured
/// default place.
#[server]
pub async fn get_score(place: String, hours: i64) -> Result<AggregateScore, ServerFnError> {
    use crate::db::{load_config, DEFAULT_USER_ID};
    use crate::state::AppState;
    use chrono::Utc;
    use grusindeks_cli::run::{run_score, ScoreInputs};
    use grusindeks_cli::windows::{
        location_frost_source, resolve_location, resolve_window,
        window_starts_within_nowcast_horizon,
    };

    use crate::db::{log_score, place_id_by_name, HistoryInsert};
    use chrono::DurationRound;

    let state = expect_context::<AppState>();
    let cfg = load_config(&state.db, DEFAULT_USER_ID).await.map_err(err)?;

    let place_arg = (!place.is_empty()).then_some(place);
    let location = resolve_location(&cfg, None, None, place_arg, None).map_err(err)?;
    let frost_source_id = location_frost_source(&cfg, &location);
    let win = resolve_window(None, hours).map_err(err)?;
    let fetch_nowcast = window_starts_within_nowcast_horizon(win, Utc::now());

    let mut agg = run_score(
        &state.client(),
        ScoreInputs {
            center: location.center,
            radius_km: location.radius_km,
            window: win,
            frost_source_id: frost_source_id.as_deref(),
            history_hours: 168,
            lang: cfg.language,
            fetch_nowcast,
            progress: &NoopProgress,
        },
    )
    .await
    .map_err(err)?;

    // Stamp the real compute time and the centre's sunrise/sunset so the UI can
    // show an honest "oppdatert" and a daylight hint instead of guessing from
    // the client clock.
    agg.produced_at = Some(Utc::now());
    agg.sun = Some(grusindeks_core::sun::sun_times(
        win.start.date_naive(),
        location.center.lat,
        location.center.lon,
    ));

    // Best-effort history log, bucketed to the hour (named places only).
    if let Ok(Some(pid)) = place_id_by_name(&state.db, DEFAULT_USER_ID, &location.name).await {
        let center = agg
            .points
            .iter()
            .find(|p| p.is_center)
            .or_else(|| agg.points.first());
        let target_time = win
            .start
            .duration_trunc(chrono::TimeDelta::hours(1))
            .unwrap_or(win.start)
            .to_rfc3339();
        let _ = log_score(
            &state.db,
            DEFAULT_USER_ID,
            &HistoryInsert {
                place_id: pid,
                kind: "score",
                target_time,
                mean: agg.mean,
                min: agg.min,
                max: agg.max,
                breakdown: center.map(|c| c.score.breakdown),
                confidence: None,
            },
        )
        .await;
    }

    Ok(agg)
}

/// Multi-day forecast for a place (empty `place` → default). Surfaces the best
/// ~2-hour sub-window per day, avoiding configured work hours.
#[server]
pub async fn get_forecast(place: String, days: u8) -> Result<MultiDayForecast, ServerFnError> {
    use crate::db::{load_config, DEFAULT_USER_ID};
    use crate::db::{log_score, place_id_by_name, HistoryInsert};
    use crate::state::AppState;
    use chrono::{Local, Utc};
    use grusindeks_cli::run::{run_forecast, ForecastInputs};
    use grusindeks_cli::windows::{
        build_day_windows, build_work_hour_exclusions, location_frost_source, resolve_location,
        window_starts_within_nowcast_horizon,
    };
    use grusindeks_core::daily::{BestWindowConfig, Confidence};

    let state = expect_context::<AppState>();
    let cfg = load_config(&state.db, DEFAULT_USER_ID).await.map_err(err)?;

    let place_arg = (!place.is_empty()).then_some(place);
    let location = resolve_location(&cfg, None, None, place_arg, None).map_err(err)?;
    let frost_source_id = location_frost_source(&cfg, &location);

    let today = Local::now().date_naive();
    let day_windows =
        build_day_windows(today, days, Utc::now(), cfg.daytime_window).map_err(err)?;
    let excluded = build_work_hour_exclusions(today, days, &cfg.work_hours);
    let best_window = BestWindowConfig {
        length_hours: 2,
        min_improvement: 0,
        excluded_windows: excluded,
    };
    let fetch_nowcast = day_windows
        .first()
        .is_some_and(|d| window_starts_within_nowcast_horizon(d.window, Utc::now()));

    let mf = run_forecast(
        &state.client(),
        ForecastInputs {
            center: location.center,
            radius_km: location.radius_km,
            days: day_windows,
            frost_source_id: frost_source_id.as_deref(),
            history_hours: 168,
            lang: cfg.language,
            best_window,
            fetch_nowcast,
            progress: &NoopProgress,
        },
    )
    .await
    .map_err(err)?;

    // Best-effort: log each day's rollup (latest forecast wins per day).
    if let Ok(Some(pid)) = place_id_by_name(&state.db, DEFAULT_USER_ID, &location.name).await {
        for day in &mf.days {
            let conf = match day.confidence {
                Confidence::Hoy => "hoy",
                Confidence::Middels => "middels",
                Confidence::Lav => "lav",
            };
            let _ = log_score(
                &state.db,
                DEFAULT_USER_ID,
                &HistoryInsert {
                    place_id: pid,
                    kind: "forecast_day",
                    target_time: day.window.start.to_rfc3339(),
                    mean: day.mean,
                    min: day.min,
                    max: day.max,
                    breakdown: Some(day.center().score.breakdown),
                    confidence: Some(conf),
                },
            )
            .await;
        }
    }

    Ok(mf)
}

/// Hour-by-hour scores for a *full local day* (00:00–24:00) at a place (empty
/// `place` → default), carrying each hour's raw centre-point values
/// (°C / m/s / mm / %) plus the day's sunrise/sunset so the dashboard can draw a
/// whole-day timeline with a daylight arc. The *day* is picked the same way as
/// the multi-day view (today, or tomorrow once today's ride window is over) so
/// the timeline and the recommendation card agree; lane data only fills from
/// `now` onward (MET publishes no past hours), while the time axis and daylight
/// arc span the whole day. Best-window/history are owned by
/// `get_forecast`/`get_score`, so this does no extra logging.
#[server]
pub async fn get_hourly(place: String) -> Result<HourlyForecast, ServerFnError> {
    use crate::db::{load_config, DEFAULT_USER_ID};
    use crate::state::AppState;
    use chrono::{DurationRound, Local, TimeDelta, Utc};
    use grusindeks_cli::run::{run_hourly, DayWindow, HourlyInputs};
    use grusindeks_cli::windows::{
        local_to_utc, location_frost_source, resolve_location, window_starts_within_nowcast_horizon,
    };
    use grusindeks_core::types::RideWindow;

    let state = expect_context::<AppState>();
    let cfg = load_config(&state.db, DEFAULT_USER_ID).await.map_err(err)?;

    let place_arg = (!place.is_empty()).then_some(place);
    let location = resolve_location(&cfg, None, None, place_arg, None).map_err(err)?;
    let frost_source_id = location_frost_source(&cfg, &location);

    let now = Utc::now();
    // The timeline is "the rest of today" — from the current hour to local
    // midnight — so it stays on *today* until midnight (unlike the daytime-window
    // ride score, which rolls to tomorrow once the 10–22 window is nearly over).
    let date = Local::now().date_naive();
    let day_end = local_to_utc(
        date.succ_opt()
            .expect("date has a successor")
            .and_hms_opt(0, 0, 0)
            .expect("valid midnight"),
    )
    .map_err(err)?;
    // Floor `now` to the current hour so the in-progress hour bucket is included
    // (the timeline starts at the live hour, no empty sliver before the next
    // whole hour).
    let now_hour = now.duration_trunc(TimeDelta::hours(1)).unwrap_or(now);
    let window = RideWindow {
        start: now_hour,
        end: day_end,
    };
    let day_windows = vec![DayWindow { date, window }];
    let header_hours: Vec<u8> = (0..24).collect();
    let fetch_nowcast = window_starts_within_nowcast_horizon(window, now);

    let mut hf = run_hourly(
        &state.client(),
        HourlyInputs {
            center: location.center,
            radius_km: location.radius_km,
            days: day_windows,
            frost_source_id: frost_source_id.as_deref(),
            history_hours: 168,
            lang: cfg.language,
            header_hours,
            fetch_nowcast,
            progress: &NoopProgress,
        },
    )
    .await
    .map_err(err)?;

    // Stamp the shown day's sunrise/sunset at the centre so the timeline can
    // draw the daylight arc for the *correct* day (today or tomorrow).
    hf.sun = Some(grusindeks_core::sun::sun_times(
        date,
        location.center.lat,
        location.center.lon,
    ));
    Ok(hf)
}

/// History samples for a place + kind ("score" | "forecast_day"), oldest-first.
#[server]
pub async fn get_history(
    place: String,
    kind: String,
    limit: i64,
) -> Result<Vec<HistoryPoint>, ServerFnError> {
    use crate::db::{list_history, load_config, place_id_by_name, DEFAULT_USER_ID};
    use crate::state::AppState;
    use grusindeks_cli::windows::resolve_location;

    let state = expect_context::<AppState>();
    let cfg = load_config(&state.db, DEFAULT_USER_ID).await.map_err(err)?;
    let place_arg = (!place.is_empty()).then_some(place);
    let location = resolve_location(&cfg, None, None, place_arg, None).map_err(err)?;
    let Some(pid) = place_id_by_name(&state.db, DEFAULT_USER_ID, &location.name)
        .await
        .map_err(err)?
    else {
        return Ok(Vec::new());
    };
    list_history(&state.db, DEFAULT_USER_ID, pid, &kind, limit)
        .await
        .map_err(err)
}

// ---- settings: prefs ----

#[server]
pub async fn get_prefs() -> Result<PrefsDto, ServerFnError> {
    use crate::db::{get_prefs, DEFAULT_USER_ID};
    use crate::state::AppState;
    let state = expect_context::<AppState>();
    get_prefs(&state.db, DEFAULT_USER_ID).await.map_err(err)
}

#[server]
pub async fn save_prefs(prefs: PrefsDto) -> Result<(), ServerFnError> {
    use crate::db::{update_prefs, DEFAULT_USER_ID};
    use crate::state::AppState;
    let state = expect_context::<AppState>();
    update_prefs(&state.db, DEFAULT_USER_ID, &prefs)
        .await
        .map_err(err)?;
    // Contact / Frost creds feed the MET client's User-Agent — rebuild it.
    state.rebuild_client().await.map_err(err)
}

// ---- settings: places ----

#[server]
pub async fn list_places() -> Result<Vec<PlaceDto>, ServerFnError> {
    use crate::db::{list_places, DEFAULT_USER_ID};
    use crate::state::AppState;
    let state = expect_context::<AppState>();
    list_places(&state.db, DEFAULT_USER_ID).await.map_err(err)
}

#[server]
pub async fn save_place(place: PlaceDto) -> Result<i64, ServerFnError> {
    use crate::db::{upsert_place, DEFAULT_USER_ID};
    use crate::state::AppState;
    let state = expect_context::<AppState>();
    upsert_place(&state.db, DEFAULT_USER_ID, &place)
        .await
        .map_err(err)
}

#[server]
pub async fn remove_place(id: i64) -> Result<(), ServerFnError> {
    use crate::db::{delete_place, DEFAULT_USER_ID};
    use crate::state::AppState;
    let state = expect_context::<AppState>();
    delete_place(&state.db, DEFAULT_USER_ID, id)
        .await
        .map_err(err)
}

// ---- settings: work hours ----

#[server]
pub async fn get_work_hours() -> Result<WorkHoursDto, ServerFnError> {
    use crate::db::{get_work_hours, DEFAULT_USER_ID};
    use crate::state::AppState;
    let state = expect_context::<AppState>();
    get_work_hours(&state.db, DEFAULT_USER_ID)
        .await
        .map_err(err)
}

#[server]
pub async fn save_work_hours(work_hours: WorkHoursDto) -> Result<(), ServerFnError> {
    use crate::db::{update_work_hours, DEFAULT_USER_ID};
    use crate::state::AppState;
    let state = expect_context::<AppState>();
    update_work_hours(&state.db, DEFAULT_USER_ID, &work_hours)
        .await
        .map_err(err)
}
