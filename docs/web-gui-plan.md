# Leptos Web GUI for `grusindeks`

## Context

`grusindeks` is a Rust CLI that computes a 0–100 gravel-cycling weather score from MET forecasts. The goal is a **web GUI** that:
- Is **all-Rust** (decided: **Leptos** SSR + WASM + Tailwind, after researching that Rust animation libs like `leptos-motion` now exist but are less mature than JS; this is a data dashboard where CSS / View-Transitions-level animation is plenty).
- Serves a single user now (places, work-hours, locations, language) but is **multiuser-ready** in the data model.
- Looks modern with nice animations.
- Runs in **Docker on a home server** after development, with **SQLite on a mounted volume**.
- Has an **interactive map** showing the center + radius + 8 compass sample points colored by score.
- **No auth** for now (open on LAN); schema kept multiuser-ready.

The whole point is to reuse the existing, well-tested domain + orchestration logic unchanged and only add a presentation/persistence layer on top.

## Key finding: almost everything is reusable as-is

- `run_score` / `run_forecast` / `run_hourly` (`crates/grusindeks-cli/src/run.rs`, all `pub async fn`) already return fully `Serialize` aggregates (`crates/grusindeks-cli/src/aggregate.rs`). Each point carries `Point{lat,lon}`, `bearing_deg`, `bearing_label`, `is_center`, and a full `Grusindeks{total, breakdown, penalties, ...}` → these **are** the JSON API responses and map directly onto map markers (lat/lon + score color).
- `MetClient` is `Clone` (cheap) → store once in Axum state, clone per request.
- `ProgressSink` (run.rs) has all-no-op defaults → web uses an empty `NoopProgress`.
- **Weather caching is already solved**: `crates/grusindeks-met/src/cache.rs` is a disk cache honoring `Expires` / `If-Modified-Since` (MET TOS). Refreshing the page within `Expires` (~30 min) = **zero MET calls** (served from disk); after expiry = a cheap `304` revalidation. Web layer just points the cache dir at the Docker volume.
- **The one blocker**: the local-time→UTC window builders live in the **binary** `main.rs`, so they aren't importable. They must be extracted into a shared library (Phase 0).

## Architecture decisions (locked)

| Aspect | Decision |
|---|---|
| Frontend | Leptos (SSR + hydration) + Tailwind, via `cargo-leptos` |
| Backend | Axum (Leptos integrates with it), reusing `grusindeks-core` + `grusindeks-met` unchanged |
| Storage | SQLite via `sqlx` + migrations; multiuser-ready schema; `score_history` for trends |
| Weather cache | Existing `grusindeks-met` disk cache, dir on the Docker volume |
| Auth | None (LAN-open); all tables scoped by `user_id` with seeded `id=1` |
| Map | MapLibre GL JS via a thin `wasm-bindgen` JS shim |
| Deploy | Multi-stage Docker + docker-compose, SQLite + cache on a mounted volume |

---

## Phase 0 — Shared-lib refactor (no behavior change)

Convert `grusindeks-cli` into **library + thin binary** (cleaner than a new crate — keeps existing module paths and tests intact).

- Add `crates/grusindeks-cli/src/lib.rs` exposing `pub mod {aggregate, config, run, theme, windows}`. Gate CLI-only deps (`owo-colors`, `indicatif`, `terminal_size`, `unicode-width`, `clap`, `clap_complete`) and `pub mod {output, progress}` behind a `cli` feature enabled only by the binary.
- Create `crates/grusindeks-cli/src/windows.rs` and **move** from `main.rs` (make `pub`): `build_day_windows`, `build_work_hour_exclusions`, `daytime_header_hours`, `resolve_window`, `window_starts_within_nowcast_horizon`, `local_to_utc`, `has_forecast_hour_in_window`, `next_whole_hour_at_or_after`, `resolve_location`, `location_frost_source`, `build_client`, the `NOWCAST_HORIZON`/`DEFAULT_FORECAST_DAYS`/`MAX_FORECAST_DAYS`/`MAX_HOURS` consts, and the `build_day_windows_tests` module. Parametrize `build_client(app, version, cfg, api_base, frost_base)` so it isn't tied to the crate name.
- `Cargo.toml` (cli): add `[lib] name="grusindeks_cli"` keeping `[[bin]]`; add `cli` feature.
- `main.rs` becomes thin: `use grusindeks_cli::{run::*, windows::*, config::*};` + clap `Cli` + `cmd_*` + output/progress wiring.
- Add `#[derive(Deserialize)]` to the aggregate types in `aggregate.rs` (`AggregateScore`, `PointScore`, `DayAggregate`, `DayPointScore`, `MultiDayForecast`, `HourlyForecast`, `HourScore`, `HourlyDayAggregate`, `NowcastAlert`, `RainHistory`) + any core types missing it (`Grusindeks`, `ScoreBreakdown`, `Penalty`, `DayScore`, `OptimalWindow`, `WindowStats`, `Confidence`) so Leptos server fns round-trip them.

**Verify:** `cargo test --workspace` green; diff a `grusindeks --json` forecast before/after (identical).

## Phase 1 — Axum + Leptos skeleton

New crate `crates/grusindeks-web/` (add to workspace `members`). Set `rust-version` **per-package** on `grusindeks-web` to whatever the pinned Leptos release needs — do **not** raise the reusable crates' 1.80 MSRV. `crate-type = ["cdylib","rlib"]`; features `ssr` / `hydrate`.

Deps: `leptos`, `leptos_axum`, `leptos_meta`, `leptos_router`, `axum`, `tower-http` (compression + ServeDir), `tokio`, `sqlx` (`runtime-tokio-rustls`, `sqlite`, `migrate`, `chrono`), `grusindeks-cli` (lib, `default-features=false`, no `cli`), `grusindeks-core`, `grusindeks-met`, `wasm-bindgen`/`web-sys`/`console_error_panic_hook` (hydrate), optional `leptos-motion`.

- `state.rs`: `AppState { client: ArcSwap<MetClient>, db: SqlitePool }`. Build `MetClient` via `windows::build_client(APP, VERSION, &cfg, None, None)`; cache dir from `GRUSINDEKS_CACHE_DIR`.
- One `get_score` server fn → `run_score(&client, ScoreInputs{.., progress:&NoopProgress})` → returns `AggregateScore`. Inject `AppState` via `leptos_routes_with_context`.
- Bare `DashboardPage` showing `mean`. Tailwind wired through `[package.metadata.leptos]` (`tailwind-input-file = "style/main.scss"`).

**Verify:** `cargo leptos watch`; page shows a live Oslo score. wiremock server-fn test (below) passes.

## Phase 2 — SQLite + settings CRUD

`migrations/0001_init.sql` (tables below). Set PRAGMAs on connect (not in migration): `journal_mode=WAL`, `synchronous=NORMAL`, `foreign_keys=ON`, `busy_timeout=5s`. Use the **runtime** `sqlx::query`/`query_as` API (not `query!` macros) to avoid the compile-time-DB / offline-cache burden.

```
users(id PK, name, created_at)
prefs(user_id PK→users, user_agent_contact, default_place_id→places, language,
      daytime_start, daytime_end, show_rain_history, show_window_stats,
      frost_client_id, frost_source_id)
places(id PK, user_id→users, name, lat, lon, radius_km, frost_source_id, UNIQUE(user_id,name))
work_hours(user_id PK→users, enabled, days_csv, window_start, window_end)
score_history(id PK, user_id→users, place_id→places, kind, target_time, computed_at,
      mean, min, max, bd_temperature, bd_wind, bd_precipitation,
      bd_precip_probability, bd_ground, confidence,
      UNIQUE(user_id, place_id, kind, target_time))
```
Seed `users(1)`, `prefs(1, 'CHANGE_ME@example.com')`, `work_hours(1)`. All queries hardcode `const DEFAULT_USER_ID = 1` today; swap for session user id when auth lands (no schema migration).

- `db/models.rs::load_config(pool, user_id) -> config::Config` rebuilds the **exact existing `Config` shape** (places BTreeMap, `DaytimeWindow::try_from`, `Language`, `FrostConfig`, `WorkHoursConfig` from `days_csv`) → feeds unchanged into `windows.rs` builders + `build_client`.
- `db/queries.rs`: per-table upsert/delete for the CRUD server fns.
- `SettingsPage`: prefs form + places table + work-hours form. On contact/frost change → `ArcSwap` rebuild of `MetClient`.

**Verify:** edit a place in UI → reload → forecast reflects it; DB round-trip test (`load_config` ⇄ known `Config`); WAL file appears.

## Phase 3 — Dashboard components

Server fns `get_forecast` (→`run_forecast`) and `get_hourly` (→`run_hourly`), reusing `build_day_windows` / `build_work_hour_exclusions` / `daytime_header_hours`. Routes: `/` dashboard, `/place/:name` detail, `/settings`. Load via `Resource` + `Suspense`/`Transition`.

- `color.rs`: port the **exact buckets** from `theme.rs::score_color` — `0–24 bad`, `25–44 marginal`, `45–64 ok`, `65–84 good`, `85–100 great` — exposing `class_for(u8)` (Tailwind) + `hex_for(u8)` (for MapLibre GeoJSON). **Unit test mirroring `theme.rs`'s bucket test** so web colors can't drift from CLI semantics. Severity chips reuse `theme::severity_color` boundaries.
- Components: `gauge` (SVG arc + count-up), `day_card` (mean/min-max/optimal-window badge/confidence dimming), `subscore_bars` (5 axes from `ScoreBreakdown`), `penalty_chips` (from `Grusindeks.penalties`), `nowcast_banner` ("regn om N min" computed client-side from `NowcastAlert` UTC times + `peak_mm_h`).
- Animations: View Transitions API for route changes; Tailwind CSS transitions for bars/cards/chips/count-up. `leptos-motion` springs **only** on gauge + map markers, in a later polish pass.

**Verify:** dashboard renders min/mean/max, sub-scores, penalties, nowcast banner for fixture data.

## Phase 4 — Interactive map

- `assets/map_glue.js`: a thin JS shim owning the `maplibregl.Map`, exposing exactly 3 functions: `gi_map_init(id,lng,lat,zoom)`, `gi_map_set_points(geojson)`, `gi_map_set_center_ring(lng,lat,radius_km)`. All MapLibre API churn stays in JS; Rust surface stays tiny/stable. **Vendor** MapLibre locally (LAN server may lack internet), not CDN.
- `components/map.rs`: `#[wasm_bindgen(module="/assets/map_glue.js")]` extern bindings; a `MapView` rendering `<div id="gi-map">` with a **client-only** `Effect` (guard against SSR) calling `gi_map_init` once, then `gi_map_set_points` on `points` signal change.
- Build GeoJSON in Rust from `points[]`: each `PointScore` → Point feature at `[lon,lat]` with props `{ total, color: hex_for(total), bearing_label, is_center }`; data-driven `circle-color`; center pin distinct. Radius ring polygon from `grusindeks_core::geo::destination(center, bearing, radius_km)` sampled at 64 bearings (reuses existing geo math; ring matches the sampling disk exactly).

**Verify:** center pin + ring + 8 colored compass markers render and recolor on place/day change.

## Phase 5 — History / trends (+ optional in-process cache)

- On each successful `get_score`/`get_forecast`, upsert `score_history` rows (`INSERT ... ON CONFLICT(user_id,place_id,kind,target_time) DO UPDATE`) — idempotent, no spam.
- `list_history` server fn + `sparkline.rs` (SVG polyline from rows; optional per-axis from `bd_*`).
- Optional headless accumulation: a `tokio::spawn` hourly loop (or scheduled job) walking all places → `run_forecast` → log, so trends grow even when nobody opens the page.
- **Optional polish**: small in-process `(place, window) → AggregateScore` cache (short TTL) so rapid refreshes skip even disk-read + re-scoring. Not required — the disk cache already makes refreshes fast and TOS-safe.

**Verify:** sparkline populates after a few refreshes / a scheduled run.

## Phase 6 — Docker + polish

Multi-stage Dockerfile: `rust:<pin>` builder with `cargo install cargo-leptos --locked`, `rustup target add wasm32-unknown-unknown`, Node (for Tailwind CLI) → `cargo leptos build --release -p grusindeks-web` → `debian:bookworm-slim` runtime with `ca-certificates`, the server bin, and `target/site`. Env: `LEPTOS_SITE_ROOT=/app/site`, `LEPTOS_SITE_ADDR=0.0.0.0:3000`, `GRUSINDEKS_DB=/data/grusindeks.db`, `GRUSINDEKS_CACHE_DIR=/data/cache`. On startup: ensure `/data` dirs, run `sqlx::migrate!()`, build `MetClient`. `VOLUME ["/data"]`. docker-compose mounts a named volume to `/data`, `restart: unless-stopped`. rustls means no OpenSSL in runtime image.

Polish: `leptos-motion` springs on gauge + markers; dark/light toggle (gruvbox dark default, `prefers-color-scheme` + `localStorage`); `tower-http` compression; wasm `opt-level="z"` + `wasm-opt` via cargo-leptos.

**Verify:** `docker compose up`; hit `http://server:3000`; data + cache persist across `docker compose restart`.

---

## Verification (end-to-end)

- **Local:** `cargo leptos watch -p grusindeks-web` with `GRUSINDEKS_DB`/`GRUSINDEKS_CACHE_DIR` → temp paths.
- **Score endpoints w/o network:** reuse the proven `run.rs` test pattern — `wiremock::MockServer` serving `fixtures/locationforecast_oslo.json`, `MetClientConfig.api_base` pointed at it, `AppState` with temp SQLite, call server fns directly in `#[tokio::test]`, assert `mean`/`points.len()`.
- **DB:** migrate temp SQLite, round-trip `load_config` ⇄ `Config`.
- **Color parity:** test that `color::class_for`/`hex_for` boundaries equal `theme::score_color`.
- **Map:** manual / browser screenshot; assert marker count via a debug hook in the shim.
- Keep `cargo fmt`/`clippy`/`test --workspace` green; the moved `build_day_windows_tests` + `run.rs` tests guard the Phase 0 extraction.

## Critical files

**Modify:**
- `crates/grusindeks-cli/src/main.rs` — extract helpers to `windows.rs`; thin binary.
- `crates/grusindeks-cli/src/aggregate.rs` — add `Deserialize` derives.
- `crates/grusindeks-cli/Cargo.toml` — add `[lib]` + `cli` feature.
- `Cargo.toml` (workspace) — add `grusindeks-web` to `members`.

**Reuse unchanged:** `run.rs` (`run_*`, `ProgressSink`, `*Inputs`), `config.rs` (`Config` shape), `theme.rs` (color buckets), `grusindeks-met` (incl. disk cache), all of `grusindeks-core`.

**Create:** `crates/grusindeks-cli/src/{lib.rs, windows.rs}`; the whole `crates/grusindeks-web/` tree (server fns, components, pages, `db/`, `color.rs`, `migrations/`, `style/`, `assets/map_glue.js`); `Dockerfile`; `docker-compose.yml`.

## Risks & mitigations

- **cargo-leptos MSRV / build deps** — low (local toolchain 1.92); pin Leptos + cargo-leptos + builder image; per-package `rust-version`; Node in build image for Tailwind.
- **WASM bundle size** — `opt-level="z"` + `wasm-opt` + server compression; keep MapLibre as external JS (only thin glue in WASM).
- **MapLibre interop** — confine all API calls to `map_glue.js` (3-fn Rust surface); client-only init effect; vendor MapLibre.
- **SQLite writes** — WAL + busy_timeout + idempotent ON CONFLICT upserts; low single-user traffic; small pool (not max=1).
- **CLI/web config drift** — both consume the same `Config` + `windows.rs`; color parity test guards the one duplicated table.
