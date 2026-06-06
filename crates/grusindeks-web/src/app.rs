//! Leptos application root: document shell, router/navigation, the dashboard,
//! and the settings page (prefs / places / work-hours CRUD).

use leptos::prelude::*;
use leptos_meta::{provide_meta_context, MetaTags, Stylesheet, Title};
use leptos_router::components::{Route, Router, Routes, A};
use leptos_router::path;

use crate::components::{
    BestWindowHint, DayCard, DaySelect, NowcastBanner, Recommendation, RideTimeline, ScoreGauge,
    SubscoreBars,
};
use crate::dto::{PlaceDto, PrefsDto, WorkHoursDto};
use crate::icons;
use crate::map::MapView;
use crate::server::{
    get_forecast, get_hourly, get_hourly_day, get_prefs, get_score, get_work_hours, list_places,
    remove_place, save_place, save_prefs, save_work_hours,
};

/// The HTML document the server renders around the hydrated app.
pub fn shell(options: LeptosOptions) -> impl IntoView {
    view! {
        <!DOCTYPE html>
        <html lang="no">
            <head>
                <meta charset="utf-8"/>
                <meta name="viewport" content="width=device-width, initial-scale=1"/>
                // Vendored MapLibre (no CDN) + the thin map glue, as classic
                // scripts so their globals exist before the wasm module runs.
                <link rel="icon" type="image/svg+xml" href="/favicon.svg"/>
                <link rel="stylesheet" href="/maplibre-gl.css"/>
                <script src="/maplibre-gl.js"></script>
                <script src="/map_glue.js"></script>
                <AutoReload options=options.clone()/>
                <HydrationScripts options/>
                <MetaTags/>
            </head>
            <body>
                <App/>
            </body>
        </html>
    }
}

#[component]
pub fn App() -> impl IntoView {
    provide_meta_context();

    view! {
        <Stylesheet id="leptos" href="/pkg/grusindeks-web.css"/>
        <Title text="Grusindeks"/>

        <Router>
            <div class="min-h-screen bg-gruv-bg0 text-gruv-fg">
                <NavBar/>
                <main>
                    <Routes fallback=|| view! { <p class="p-8">"Fant ikke siden."</p> }>
                        <Route path=path!("") view=DashboardPage/>
                        <Route path=path!("settings") view=SettingsPage/>
                    </Routes>
                </main>
            </div>
        </Router>
    }
}

#[component]
fn NavBar() -> impl IntoView {
    view! {
        <header class="sticky top-0 z-30 border-b border-gruv-bg2 bg-gruv-bg0/80 backdrop-blur-sm">
            <nav class="mx-auto flex max-w-5xl items-center justify-between px-6 py-5">
                <A href="/">
                    // The whole lockup is the home link; `group` drives the
                    // logo's needle-swing + the wordmark tint on hover.
                    <span class="group -mx-2 flex items-center gap-2.5 rounded-lg px-2 py-1 transition-colors hover:bg-gruv-bg1/50">
                        {icons::logo("h-7 w-7 text-gruv-fg gx-logo", Some("Grusindeks"))}
                        <span class="hidden text-lg font-bold tracking-tight text-gruv-fg transition-colors group-hover:text-gruv-aqua sm:inline">
                            "Grus"<span class="font-semibold text-gruv-gray">"indeks"</span>
                        </span>
                    </span>
                </A>
                <A href="/settings">
                    <span class="flex items-center gap-1.5 text-sm text-gruv-gray transition-colors hover:text-gruv-fg">
                        {icons::settings("h-[18px] w-[18px]", None)}
                        <span class="hidden sm:inline">"Innstillinger"</span>
                    </span>
                </A>
            </nav>
        </header>
    }
}

#[component]
fn DashboardPage() -> impl IntoView {
    let places = Resource::new(|| (), |_| async move { list_places().await });
    let prefs = Resource::new(|| (), |_| async move { get_prefs().await });
    let selected = RwSignal::new(String::new());
    let selected_day = RwSignal::new(None::<String>);
    let selected_best_window = RwSignal::new(None::<grusindeks_core::types::RideWindow>);
    // The clicked day card's box in viewport coords: (left, top, width, height).
    let selected_day_anchor = RwSignal::new((0_i32, 0_i32, 0_i32, 0_i32));

    let score = Resource::new(
        move || selected.get(),
        |place| async move { get_score(place, 3).await },
    );
    // Rest-of-day aggregate for the Indeks section: a window from now to local
    // midnight (≥1 h). The 3-hour `score` above still drives the "Kjør nå" card.
    let rest_score = Resource::new(
        move || selected.get(),
        |place| async move {
            let now = chrono::Local::now();
            let end = now
                .date_naive()
                .succ_opt()
                .and_then(|d| d.and_hms_opt(0, 0, 0))
                .map(|m| (m - now.naive_local()).num_hours())
                .unwrap_or(3)
                .clamp(1, 24);
            get_score(place, end).await
        },
    );
    let forecast = Resource::new(
        move || selected.get(),
        |place| async move { get_forecast(place, 6).await },
    );
    let hourly = Resource::new(
        move || selected.get(),
        |place| async move { get_hourly(place).await },
    );
    let hourly_day = Resource::new(
        move || (selected.get(), selected_day.get()),
        |(place, date)| async move {
            match date {
                Some(date) => Some(get_hourly_day(place, date).await),
                None => None,
            }
        },
    );
    view! {
        <section class="mx-auto max-w-5xl px-6 py-10 min-[2024px]:max-w-[2200px]">
            <div class="flex flex-col gap-3 sm:flex-row sm:items-end sm:justify-between">
                <div>
                    // Wordmark lives in the NavBar; here we only frame the view.
                    <p class="text-gruv-fg/70">"Forholdene i dag"</p>
                </div>
                <Suspense>
                    {move || Suspend::new(async move {
                        let opts = places.await.unwrap_or_default();
                        let default_label = prefs.await
                            .ok()
                            .map(|p| p.default_place)
                            .filter(|name| !name.trim().is_empty())
                            .unwrap_or_else(|| "Velg sted".to_string());
                        view! {
                            <label class="flex flex-col gap-1 text-xs font-semibold uppercase tracking-wide text-gruv-fg/70 sm:items-end">
                                "Sted"
                                <select
                                    class="min-h-[44px] w-full cursor-pointer rounded-lg border border-gruv-bg2 bg-gruv-bg1 px-3 py-2.5 text-sm normal-case tracking-normal text-gruv-fg outline-none transition-colors hover:border-gruv-bg2/40 focus:border-gruv-aqua focus:ring-2 focus:ring-gruv-aqua/70 sm:w-auto"
                                    on:change=move |ev| selected.set(event_target_value(&ev))
                                >
                                    <option value="">{default_label}</option>
                                    {opts.into_iter().map(|p| {
                                        let value = p.name.clone();
                                        view! { <option value=value>{p.name}</option> }
                                    }).collect_view()}
                                </select>
                            </label>
                        }
                    })}
                </Suspense>
            </div>

            <div class="mt-8 space-y-8">

            // ---- current window card ----
            <Suspense fallback=move || {
                // Skeleton that mirrors the Recommendation card's shape, so the
                // layout doesn't jump when the real verdict arrives.
                view! {
                    <div class="emboss rounded-2xl bg-gruv-bg1 p-6" aria-hidden="true">
                        <div class="flex items-start justify-between gap-4">
                            <div class="min-w-0 flex-1 space-y-3">
                                <div class="h-6 w-44 animate-pulse rounded-full bg-gruv-bg2/40"></div>
                                <div class="h-8 w-48 animate-pulse rounded bg-gruv-bg2/40"></div>
                                <div class="h-4 w-64 animate-pulse rounded bg-gruv-bg2/30"></div>
                            </div>
                            <div class="h-6 w-20 animate-pulse rounded-full bg-gruv-bg2/40"></div>
                        </div>
                        <div class="mt-4 grid grid-cols-3 gap-2.5">
                            <div class="h-16 animate-pulse rounded-xl inset-well bg-gruv-bg0/40"></div>
                            <div class="h-16 animate-pulse rounded-xl inset-well bg-gruv-bg0/40"></div>
                            <div class="h-16 animate-pulse rounded-xl inset-well bg-gruv-bg0/40"></div>
                        </div>
                        <span class="sr-only">"Laster prognose…"</span>
                    </div>
                }
            }>
                {move || Suspend::new(async move {
                    match score.await {
                        Ok(agg) => {
                            let center = agg.points.iter().find(|p| p.is_center).cloned()
                                .or_else(|| agg.points.first().cloned());
                            match center {
                                Some(c) => {
                                    let nowcast = agg.nowcast_alert.clone();
                                    // All penalties drive the recommendation card's "trekkes ned av"
                                    // chips, so the details card no longer repeats them.
                                    let penalties = c.score.penalties.clone();
                                    let breakdown = c.score.breakdown;
                                    let stats = c.score.stats;
                                    let (sunrise, sunset) = agg.sun.map_or((None, None), |s| (s.sunrise, s.sunset));
                                    // Today's stand-out window, promoted from the multi-day strip.
                                    // Only surface it while it's still relevant (not fully past).
                                    let now = chrono::Utc::now();
                                    let optimal = forecast.await.ok().and_then(|mf| {
                                        mf.days
                                            .first()
                                            .and_then(|d| d.optimal_window.as_ref())
                                            .filter(|ow| ow.window.end > now)
                                            .cloned()
                                    });
                                    let best_window = optimal
                                        .as_ref()
                                        .map(|ow| BestWindowHint::from_window(ow, now, sunset));
                                    // Raw window for the timeline overlay — only when it's
                                    // *today* (the timeline shows the rest of today; a window
                                    // that rolled to tomorrow doesn't belong on it).
                                    let best_window_rw = optimal.as_ref().map(|ow| ow.window).filter(|w| {
                                        w.start.with_timezone(&chrono::Local).date_naive()
                                            == now.with_timezone(&chrono::Local).date_naive()
                                    });
                                    let produced_at = agg.produced_at;
                                    // Current ground state for the "underlag" stats tile: the
                                    // drying model's surface water *right now* (after rain,
                                    // drainage and drying), not the rain that fell over the week.
                                    let ground = agg.ground_water_mm;
                                    // Daylight-left for the "dagslys" stats tile: (value, bar
                                    // fraction 0–100, caption). After sunset it's a muted
                                    // "Mørkt" with no bar. `None` (no sun data) hides the tile.
                                    let daylight = sunset.map(|ss| {
                                        let set_hhmm = ss.with_timezone(&chrono::Local).format("%H:%M").to_string();
                                        if now >= ss {
                                            ("Mørkt".to_string(), None, format!("sol ned {set_hhmm}"))
                                        } else {
                                            let rem = ss - now;
                                            let h = rem.num_hours();
                                            let m = (rem.num_minutes() - h * 60).max(0);
                                            let main = if h > 0 { format!("{h}t {m}m") } else { format!("{m}m") };
                                            let frac = sunrise.map(|sr| {
                                                let total = (ss - sr).num_minutes().max(1) as f64;
                                                let left = (ss - now).num_minutes().clamp(0, total as i64) as f64;
                                                (left / total).clamp(0.0, 1.0) * 100.0
                                            });
                                            (main, frac, format!("sol ned {set_hhmm}"))
                                        }
                                    });
                                    // Rest-of-day aggregate for the Indeks section (gauge + bars
                                    // + spread). Falls back to the 3-hour `agg` if it's not ready.
                                    let rest = rest_score.await.ok();
                                    let rest_center = rest
                                        .as_ref()
                                        .and_then(|r| r.points.iter().find(|p| p.is_center).or_else(|| r.points.first()));
                                    let idx_mean = rest.as_ref().map_or(agg.mean, |r| r.mean);
                                    let idx_min = rest.as_ref().map_or(agg.min, |r| r.min);
                                    let idx_max = rest.as_ref().map_or(agg.max, |r| r.max);
                                    let idx_points = rest.as_ref().map_or(agg.points.len(), |r| r.points.len());
                                    let idx_breakdown = rest_center.map_or(c.score.breakdown, |p| p.score.breakdown);
                                    let idx_highlights = rest_center
                                        .map_or_else(|| c.score.highlights.clone(), |p| p.score.highlights.clone());
                                    let place = match selected.get_untracked() {
                                        p if !p.trim().is_empty() => p,
                                        _ => prefs
                                            .get_untracked()
                                            .and_then(Result::ok)
                                            .map(|p| p.default_place)
                                            .filter(|name| !name.trim().is_empty())
                                            .unwrap_or_else(|| "standardsted".to_string()),
                                    };
                                    view! {
                                        // Two columns once there's room for two full-width
                                        // columns (~2040px+); below that it's the same single
                                        // stack as before — nothing shrinks.
                                        <div class="grid grid-cols-1 gap-6 min-[2024px]:grid-cols-2 min-[2024px]:items-stretch">
                                            // LEFT — verdict + details
                                            <div class="space-y-6">
                                            <Recommendation total=agg.mean label=c.score.label.clone() penalties=penalties breakdown=breakdown stats=stats place=place best_window=best_window updated=produced_at ground=ground daylight=daylight/>
                                            <div class="emboss space-y-5 rounded-2xl bg-gruv-bg1 p-6">
                                                {nowcast.map(|a| view! { <NowcastBanner alert=a/> })}

                                                // ── Time for time ── (the timeline carries its own band header)
                                                <Suspense fallback=move || view! {
                                                    <div class="h-[290px] animate-pulse rounded-lg bg-gruv-bg0/40"></div>
                                                }>
                                                    {move || Suspend::new(async move {
                                                        match hourly.await {
                                                            Ok(mut hf) if !hf.days.is_empty() => {
                                                                // Sun for the *shown* day (today or tomorrow),
                                                                // stamped by get_hourly — not the score window's.
                                                                let (sunrise, sunset) = hf.sun
                                                                    .map_or((None, None), |s| (s.sunrise, s.sunset));
                                                                let day = hf.days.remove(0);
                                                                view! {
                                                                    <RideTimeline day=day best_window=best_window_rw sunrise=sunrise sunset=sunset now=now/>
                                                                }.into_any()
                                                            }
                                                            _ => ().into_any(),
                                                        }
                                                    })}
                                                </Suspense>

                                                // ── Indeks · resten av dagen ──
                                                <div class="space-y-3">
                                                    {section_band("Indeks · resten av dagen")}
                                                    <div class="flex items-center gap-6">
                                                        <ScoreGauge total=idx_mean/>
                                                        <div class="flex-1">
                                                            <SubscoreBars breakdown=idx_breakdown/>
                                                            <p class="mt-3 text-sm text-gruv-fg/70">
                                                                {format!("spenn {}–{} over {} punkter", idx_min, idx_max, idx_points)}
                                                            </p>
                                                        </div>
                                                    </div>
                                                    {(!idx_highlights.is_empty()).then(|| view! {
                                                        <div class="space-y-1 rounded-xl inset-well bg-gruv-bg0/45 p-3 text-sm text-gruv-blue">
                                                            {idx_highlights.into_iter().map(|h| view! { <p>{h}</p> }).collect_view()}
                                                        </div>
                                                    })}
                                                </div>
                                            </div>
                                            </div>
                                            // RIGHT — the map fills the whole column
                                            <div class="min-[2024px]:flex min-[2024px]:flex-col">
                                                <div class="min-[2024px]:flex-1 min-[2024px]:min-h-0">
                                                    <MapView points=agg.points.clone()/>
                                                </div>
                                            </div>
                                        </div>
                                    }.into_any()
                                }
                                None => view! {
                                    <div class="mt-8 rounded-xl inset-well bg-gruv-bg1 p-6">
                                        <p class="text-gruv-fg/70">"Ingen data for valgt sted."</p>
                                        <p class="mt-2 text-sm text-gruv-fg/60">"Velg et annet sted fra listen, eller kontakt meg om stedet mangler."</p>
                                    </div>
                                }.into_any(),
                            }
                        }
                        Err(e) => view! {
                            <p class="gx-pulse-error mt-8 rounded-xl bg-gruv-red/20 p-4 text-gruv-red ring-1 ring-gruv-red/30">
                                {format!("Kunne ikke hente prognose: {e}")}
                            </p>
                        }.into_any(),
                    }
                })}
            </Suspense>

            // ---- multi-day strip ----
            <Suspense>
                {move || Suspend::new(async move {
                    match forecast.await {
                        Ok(mf) => {
                            view! {
                                <div>
                                    <h2 class="mb-3 text-sm font-semibold uppercase tracking-wide text-gruv-gray">
                                        "Dagene fremover"
                                    </h2>
                                    <div class="flex gap-3 overflow-x-auto pb-2">
                                        {mf.days.into_iter().map(|d| {
                                            let best_window = d.optimal_window.as_ref().map(|ow| ow.window);
                                            let on_select = Callback::new(move |(date, l, t, w, h): DaySelect| {
                                                selected_day.set(Some(date.format("%Y-%m-%d").to_string()));
                                                selected_best_window.set(best_window);
                                                selected_day_anchor.set((l, t, w, h));
                                            });
                                            view! { <DayCard day=d on_select=on_select/> }
                                        }).collect_view()}
                                    </div>
                                    {move || selected_day.get().map(|_| {
                                        let (left, top_c, width, _h) = selected_day_anchor.get();
                                        // Bloom UP out of the clicked card: park the popup just
                                        // above the card's top edge and aim the morph origin at the
                                        // card's centre-x. The strip sits near the page bottom, so
                                        // open upward by default; only when there's no room above
                                        // does `top` clamp down and the origin flips to point up.
                                        let popup_h = 330;
                                        let cx = left + width / 2;
                                        let raw_top = top_c - popup_h - 12;
                                        let opened_up = raw_top >= 8;
                                        let top = raw_top.max(8);
                                        // Popup left, clamped into the viewport (width kept in CSS
                                        // so a resize can never desync it).
                                        let left_expr = format!(
                                            "clamp(0.5rem, calc({cx}px - min(29rem, calc((100vw - 1rem) / 2))), calc(100vw - min(58rem, calc(100vw - 1rem)) - 0.5rem))"
                                        );
                                        // Morph origin, in the popup's own box: x = card centre
                                        // minus the popup's left; y points at the card edge.
                                        let origin_y = if opened_up {
                                            "calc(100% + 12px)".to_string()
                                        } else {
                                            format!("{}px", (top_c - top).clamp(8, popup_h - 8))
                                        };
                                        let style = format!(
                                            "top:{top}px; left:{left_expr}; --gx-origin-x:calc({cx}px - {left_expr}); --gx-origin-y:{origin_y}"
                                        );
                                        view! {
                                            <div class="fixed inset-0 z-50 bg-gruv-bg0/25 backdrop-blur-[2px]" on:click=move |_| {
                                                selected_day.set(None);
                                                selected_best_window.set(None);
                                            }>
                                                <div
                                                    class="gx-popdown fixed w-[min(58rem,calc(100vw-1rem))] rounded-2xl bg-gruv-bg1 p-4 shadow-2xl ring-1 ring-gruv-bg2/70"
                                                    style=style
                                                    on:click=move |ev| ev.stop_propagation()
                                                >
                                                    <div class="mb-2 flex justify-end">
                                                        <button type="button"
                                                            class="cursor-pointer rounded-lg bg-gruv-bg0/70 px-2.5 py-1 text-xs font-semibold text-gruv-fg/70 ring-1 ring-gruv-bg2 transition-colors hover:bg-gruv-bg2/60 hover:text-gruv-fg focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-gruv-aqua"
                                                            on:click=move |_| {
                                                                selected_day.set(None);
                                                                selected_best_window.set(None);
                                                            }
                                                        >"Lukk"</button>
                                                    </div>
                                                    <Suspense fallback=move || view! {
                                                        <div class="h-[290px] animate-pulse rounded-xl bg-gruv-bg0/45 p-4 text-sm text-gruv-fg/70 inset-well">
                                                            "Laster timesvarsel…"
                                                        </div>
                                                    }>
                                                        {move || Suspend::new(async move {
                                                            match hourly_day.await {
                                                                Some(Ok(mut hf)) => {
                                                                    let sun = hf.sun;
                                                                    match hf.days.pop() {
                                                                        Some(day) => {
                                                                            let (sunrise, sunset) = sun.map_or((None, None), |s| (s.sunrise, s.sunset));
                                                                            view! {
                                                                                <RideTimeline
                                                                                    day=day
                                                                                    best_window=selected_best_window.get_untracked()
                                                                                    sunrise=sunrise
                                                                                    sunset=sunset
                                                                                    now=chrono::Utc::now()
                                                                                />
                                                                            }.into_any()
                                                                        }
                                                                        None => ().into_any(),
                                                                    }
                                                                }
                                                                Some(Err(e)) => view! {
                                                                    <div class="gx-pulse-error rounded-xl bg-gruv-red/15 p-4 text-sm text-gruv-red ring-1 ring-gruv-red/30">
                                                                        {format!("Kunne ikke hente timesvarsel: {e}")}
                                                                    </div>
                                                                }.into_any(),
                                                                None => ().into_any(),
                                                            }
                                                        })}
                                                    </Suspense>
                                                </div>
                                            </div>
                                        }
                                    })}
                                </div>
                            }.into_any()
                        },
                        Err(_) => ().into_any(),
                    }
                })}
            </Suspense>

            </div>
        </section>
    }
}

/// A section header for the details card: an uppercase eyebrow label followed
/// by a fading "groove" hairline, so the card reads as distinct stacked bands.
fn section_band(label: &'static str) -> impl IntoView {
    view! {
        <div class="flex items-center gap-2">
            <span class="text-[11px] font-semibold uppercase tracking-[0.12em] text-gruv-fg/60">{label}</span>
            <span class="h-px flex-1 bg-gradient-to-r from-gruv-aqua/40 via-gruv-aqua/15 to-transparent shadow-[0_0_8px_rgba(69,133,136,0.15)]"></span>
        </div>
    }
}

// ---------------- Settings ----------------

#[component]
fn SettingsPage() -> impl IntoView {
    view! {
        <section class="mx-auto max-w-3xl space-y-10 px-6 py-10">
            <h1 class="text-3xl font-bold tracking-tight">"Innstillinger"</h1>
            <PrefsSection/>
            <PlacesSection/>
            <WorkHoursSection/>
        </section>
    }
}

fn card() -> &'static str {
    "rounded-2xl bg-gruv-bg1 p-6 shadow-lg space-y-4"
}
fn label_cls() -> &'static str {
    "block text-sm text-gruv-gray"
}
fn input_cls() -> &'static str {
    "mt-1 w-full rounded-lg border border-gruv-bg2 bg-gruv-bg0 px-3 py-2 text-sm outline-none focus:border-gruv-aqua focus:ring-2 focus:ring-gruv-aqua/70"
}
fn btn_cls() -> &'static str {
    "rounded-lg bg-gruv-aqua px-4 py-2 text-sm font-semibold text-gruv-bg0 transition-all duration-150 hover:brightness-110 disabled:opacity-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-offset-2 focus-visible:ring-offset-gruv-bg0 focus-visible:ring-gruv-aqua"
}
/// Destructive action in a list row: text-weight, not a heavy solid fill, but a
/// real hit target with a hover wash and a keyboard focus ring.
fn btn_danger_cls() -> &'static str {
    "rounded-md px-2 py-1 text-sm font-medium text-gruv-red transition-colors hover:bg-gruv-red/15 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-gruv-red"
}

#[component]
fn PrefsSection() -> impl IntoView {
    let prefs = Resource::new(|| (), |_| async move { get_prefs().await });
    let places = Resource::new(|| (), |_| async move { list_places().await });

    view! {
        <Suspense fallback=move || view! { <p class="text-gruv-gray">"Laster…"</p> }>
            {move || Suspend::new(async move {
                let p = prefs.await;
                let opts = places.await.unwrap_or_default();
                match p {
                    Ok(p) => view! { <PrefsForm initial=p place_names=opts/> }.into_any(),
                    Err(e) => view! { <p class="text-gruv-red">{format!("Feil: {e}")}</p> }.into_any(),
                }
            })}
        </Suspense>
    }
}

#[component]
fn PrefsForm(initial: PrefsDto, place_names: Vec<PlaceDto>) -> impl IntoView {
    let contact = RwSignal::new(initial.user_agent_contact.clone());
    let language = RwSignal::new(initial.language.clone());
    let default_place = RwSignal::new(initial.default_place.clone());
    let daytime_start = RwSignal::new(initial.daytime_start.clone());
    let daytime_end = RwSignal::new(initial.daytime_end.clone());
    let frost_client = RwSignal::new(initial.frost_client_id.clone());
    let frost_source = RwSignal::new(initial.frost_source_id.clone());
    let show_rain = RwSignal::new(initial.show_rain_history);
    let show_stats = RwSignal::new(initial.show_window_stats);

    let save = Action::new(|dto: &PrefsDto| {
        let dto = dto.clone();
        async move { save_prefs(dto).await }
    });

    let on_save = move |_| {
        save.dispatch(PrefsDto {
            user_agent_contact: contact.get(),
            default_place: default_place.get(),
            language: language.get(),
            daytime_start: daytime_start.get(),
            daytime_end: daytime_end.get(),
            show_rain_history: show_rain.get(),
            show_window_stats: show_stats.get(),
            frost_client_id: frost_client.get(),
            frost_source_id: frost_source.get(),
        });
    };

    view! {
        <div class=card()>
            <h2 class="text-xl font-semibold">"Generelt"</h2>
            <div>
                <label class=label_cls()>"MET-kontakt (User-Agent)"</label>
                <input class=input_cls() prop:value=move || contact.get()
                    on:input=move |ev| contact.set(event_target_value(&ev))/>
            </div>
            <div class="grid grid-cols-2 gap-4">
                <div>
                    <label class=label_cls()>"Språk"</label>
                    <select class=input_cls() on:change=move |ev| language.set(event_target_value(&ev))>
                        <option value="norwegian" selected=move || language.get() == "norwegian">"Norsk"</option>
                        <option value="swedish" selected=move || language.get() == "swedish">"Svenska"</option>
                    </select>
                </div>
                <div>
                    <label class=label_cls()>"Standardsted"</label>
                    <select class=input_cls() on:change=move |ev| default_place.set(event_target_value(&ev))>
                        <option value="">"(ingen)"</option>
                        {place_names.into_iter().map(|p| {
                            let name = p.name;
                            let value = name.clone();
                            let sel = name.clone();
                            view! { <option value=value selected=move || default_place.get() == sel>{name}</option> }
                        }).collect_view()}
                    </select>
                </div>
            </div>
            <div class="grid grid-cols-2 gap-4">
                <div>
                    <label class=label_cls()>"Dag-vindu start"</label>
                    <input class=input_cls() prop:value=move || daytime_start.get()
                        on:input=move |ev| daytime_start.set(event_target_value(&ev))/>
                </div>
                <div>
                    <label class=label_cls()>"Dag-vindu slutt"</label>
                    <input class=input_cls() prop:value=move || daytime_end.get()
                        on:input=move |ev| daytime_end.set(event_target_value(&ev))/>
                </div>
            </div>
            <div class="grid grid-cols-2 gap-4">
                <div>
                    <label class=label_cls()>"Frost client_id"</label>
                    <input class=input_cls() prop:value=move || frost_client.get()
                        on:input=move |ev| frost_client.set(event_target_value(&ev))/>
                </div>
                <div>
                    <label class=label_cls()>"Frost source_id"</label>
                    <input class=input_cls() prop:value=move || frost_source.get()
                        on:input=move |ev| frost_source.set(event_target_value(&ev))/>
                </div>
            </div>
            <div class="flex gap-6">
                <label class="flex items-center gap-2 text-sm">
                    <input type="checkbox" prop:checked=move || show_rain.get()
                        on:change=move |ev| show_rain.set(event_target_checked(&ev))/>
                    "Vis regnhistorikk"
                </label>
                <label class="flex items-center gap-2 text-sm">
                    <input type="checkbox" prop:checked=move || show_stats.get()
                        on:change=move |ev| show_stats.set(event_target_checked(&ev))/>
                    "Vis vindustall"
                </label>
            </div>
            <div class="flex items-center gap-3">
                <button class=btn_cls() on:click=on_save disabled=move || save.pending().get()>"Lagre"</button>
                {move || save.value().get().map(|r| match r {
                    Ok(()) => view! { <span class="text-sm text-gruv-green">"Lagret"</span> }.into_any(),
                    Err(e) => view! { <span class="text-sm text-gruv-red">{format!("Feil: {e}")}</span> }.into_any(),
                })}
            </div>
        </div>
    }
}

#[component]
fn PlacesSection() -> impl IntoView {
    let places = Resource::new(|| (), |_| async move { list_places().await });

    let save = Action::new(|dto: &PlaceDto| {
        let dto = dto.clone();
        async move { save_place(dto).await }
    });
    let del = Action::new(|id: &i64| {
        let id = *id;
        async move { remove_place(id).await }
    });

    // Refetch the list whenever a save or delete completes.
    Effect::new(move |_| {
        save.version().get();
        del.version().get();
        places.refetch();
    });

    let new_name = RwSignal::new(String::new());
    let new_lat = RwSignal::new(String::new());
    let new_lon = RwSignal::new(String::new());
    let new_radius = RwSignal::new("20".to_string());

    let add = move |_| {
        save.dispatch(PlaceDto {
            id: None,
            name: new_name.get(),
            lat: new_lat.get().parse().unwrap_or(0.0),
            lon: new_lon.get().parse().unwrap_or(0.0),
            radius_km: new_radius.get().parse().unwrap_or(20.0),
            frost_source_id: String::new(),
        });
        new_name.set(String::new());
        new_lat.set(String::new());
        new_lon.set(String::new());
    };

    view! {
        <div class=card()>
            <h2 class="text-xl font-semibold">"Steder"</h2>
            <Suspense fallback=move || view! { <p class="text-gruv-gray">"Laster…"</p> }>
                {move || Suspend::new(async move {
                    let list = places.await.unwrap_or_default();
                    view! {
                        <ul class="divide-y divide-gruv-bg2">
                            {list.into_iter().map(|p| {
                                let id = p.id;
                                view! {
                                    <li class="flex items-center justify-between py-2 text-sm">
                                        <span>
                                            <span class="font-medium">{p.name}</span>
                                            <span class="ml-2 text-gruv-gray">
                                                {format!("{:.4}, {:.4} · {} km", p.lat, p.lon, p.radius_km)}
                                            </span>
                                        </span>
                                        <button class=btn_danger_cls()
                                            on:click=move |_| { if let Some(id) = id { del.dispatch(id); } }>
                                            "Slett"
                                        </button>
                                    </li>
                                }
                            }).collect_view()}
                        </ul>
                    }
                })}
            </Suspense>

            <div class="grid grid-cols-4 gap-2">
                <input class=input_cls() placeholder="navn" prop:value=move || new_name.get()
                    on:input=move |ev| new_name.set(event_target_value(&ev))/>
                <input class=input_cls() placeholder="lat" prop:value=move || new_lat.get()
                    on:input=move |ev| new_lat.set(event_target_value(&ev))/>
                <input class=input_cls() placeholder="lon" prop:value=move || new_lon.get()
                    on:input=move |ev| new_lon.set(event_target_value(&ev))/>
                <input class=input_cls() placeholder="radius km" prop:value=move || new_radius.get()
                    on:input=move |ev| new_radius.set(event_target_value(&ev))/>
            </div>
            <button class=btn_cls() on:click=add>"Legg til sted"</button>
        </div>
    }
}

const WEEKDAYS: [(&str, &str); 7] = [
    ("mon", "Man"),
    ("tue", "Tir"),
    ("wed", "Ons"),
    ("thu", "Tor"),
    ("fri", "Fre"),
    ("sat", "Lør"),
    ("sun", "Søn"),
];

#[component]
fn WorkHoursSection() -> impl IntoView {
    let wh = Resource::new(|| (), |_| async move { get_work_hours().await });
    view! {
        <Suspense fallback=move || view! { <p class="text-gruv-gray">"Laster…"</p> }>
            {move || Suspend::new(async move {
                match wh.await {
                    Ok(w) => view! { <WorkHoursForm initial=w/> }.into_any(),
                    Err(e) => view! { <p class="text-gruv-red">{format!("Feil: {e}")}</p> }.into_any(),
                }
            })}
        </Suspense>
    }
}

#[component]
fn WorkHoursForm(initial: WorkHoursDto) -> impl IntoView {
    let enabled = RwSignal::new(initial.enabled);
    let start = RwSignal::new(initial.window_start.clone());
    let end = RwSignal::new(initial.window_end.clone());
    let days = RwSignal::new(initial.days.clone());

    let save = Action::new(|dto: &WorkHoursDto| {
        let dto = dto.clone();
        async move { save_work_hours(dto).await }
    });

    let on_save = move |_| {
        save.dispatch(WorkHoursDto {
            enabled: enabled.get(),
            days: days.get(),
            window_start: start.get(),
            window_end: end.get(),
        });
    };

    view! {
        <div class=card()>
            <h2 class="text-xl font-semibold">"Arbeidstid (unngås i beste vindu)"</h2>
            <label class="flex items-center gap-2 text-sm">
                <input type="checkbox" prop:checked=move || enabled.get()
                    on:change=move |ev| enabled.set(event_target_checked(&ev))/>
                "Aktiver"
            </label>
            <div class="flex flex-wrap gap-2">
                {WEEKDAYS.iter().map(|(tok, lab)| {
                    let tok = tok.to_string();
                    let tok_in = tok.clone();
                    let tok_chk = tok.clone();
                    let checked = move || days.get().iter().any(|d| d == &tok_chk);
                    view! {
                        <label class="flex items-center gap-1 rounded-lg border border-gruv-bg2 px-2 py-1 text-sm">
                            <input type="checkbox" prop:checked=checked
                                on:change=move |ev| {
                                    let on = event_target_checked(&ev);
                                    days.update(|d| {
                                        if on { if !d.iter().any(|x| x == &tok_in) { d.push(tok_in.clone()); } }
                                        else { d.retain(|x| x != &tok_in); }
                                    });
                                }/>
                            {*lab}
                        </label>
                    }
                }).collect_view()}
            </div>
            <div class="grid grid-cols-2 gap-4">
                <div>
                    <label class=label_cls()>"Start"</label>
                    <input class=input_cls() prop:value=move || start.get()
                        on:input=move |ev| start.set(event_target_value(&ev))/>
                </div>
                <div>
                    <label class=label_cls()>"Slutt"</label>
                    <input class=input_cls() prop:value=move || end.get()
                        on:input=move |ev| end.set(event_target_value(&ev))/>
                </div>
            </div>
            <div class="flex items-center gap-3">
                <button class=btn_cls() on:click=on_save disabled=move || save.pending().get()>"Lagre"</button>
                {move || save.value().get().map(|r| match r {
                    Ok(()) => view! { <span class="text-sm text-gruv-green">"Lagret"</span> }.into_any(),
                    Err(e) => view! { <span class="text-sm text-gruv-red">{format!("Feil: {e}")}</span> }.into_any(),
                })}
            </div>
        </div>
    }
}
