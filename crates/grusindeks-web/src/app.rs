//! Leptos application root: document shell, router/navigation, the dashboard,
//! and the settings page (prefs / places / work-hours CRUD).

use leptos::prelude::*;
use leptos_meta::{provide_meta_context, MetaTags, Stylesheet, Title};
use leptos_router::components::{Route, Router, Routes, A};
use leptos_router::path;

use crate::components::{
    score_reason, BestWindowHint, DayCard, HourlyPrecipStrip, NowcastBanner, PenaltyChips,
    Recommendation, ScoreGauge, Sparkline, SubscoreBars, WindowStatsRow,
};
use crate::dto::{PlaceDto, PrefsDto, WorkHoursDto};
use crate::map::MapView;
use crate::server::{
    get_forecast, get_history, get_prefs, get_score, get_work_hours, list_places, remove_place,
    save_place, save_prefs, save_work_hours,
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
        <header class="border-b border-gruv-bg2 bg-gruv-bg0/80 backdrop-blur">
            <nav class="mx-auto flex max-w-5xl items-center justify-between px-6 py-4">
                <A href="/">
                    <span class="text-lg font-bold tracking-tight">"Grusindeks"</span>
                </A>
                <A href="/settings">
                    <span class="text-sm text-gruv-gray transition-colors hover:text-gruv-fg">
                        "Innstillinger"
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

    let score = Resource::new(
        move || selected.get(),
        |place| async move { get_score(place, 3).await },
    );
    let forecast = Resource::new(
        move || selected.get(),
        |place| async move { get_forecast(place, 6).await },
    );
    // Refetch history once the current score has been logged server-side
    // (reading `score` here makes this depend on its completion).
    let history = Resource::new(
        move || {
            let _ = score.get();
            selected.get()
        },
        |place| async move { get_history(place, "score".to_string(), 48).await },
    );

    view! {
        <section class="mx-auto max-w-5xl px-6 py-10">
            <div class="flex flex-col gap-3 sm:flex-row sm:items-end sm:justify-between">
                <div>
                    <h1 class="text-3xl font-bold tracking-tight">"Grusindeks"</h1>
                    <p class="mt-1 text-gruv-fg/70">"Neste 3 timer"</p>
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
                                    class="min-h-[44px] w-full rounded-lg border border-gruv-bg2 bg-gruv-bg1 px-3 py-2.5 text-sm normal-case tracking-normal text-gruv-fg sm:w-auto"
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
                view! { <p class="animate-pulse text-gruv-fg/70">"Laster prognose…"</p> }
            }>
                {move || Suspend::new(async move {
                    match score.await {
                        Ok(agg) => {
                            let center = agg.points.iter().find(|p| p.is_center).cloned()
                                .or_else(|| agg.points.first().cloned());
                            match center {
                                Some(c) => {
                                    let nowcast = agg.nowcast_alert.clone();
                                    let penalties = c.score.penalties.clone();
                                    let reason = score_reason(c.score.breakdown, &penalties);
                                    // `score_reason` leads with the top penalty, so don't repeat it
                                    // as a chip — show only the *additional* penalties.
                                    let chips = penalties.iter().skip(1).cloned().collect::<Vec<_>>();
                                    let highlights = c.score.highlights.clone();
                                    let stats = c.score.stats;
                                    let sunset = agg.sun.and_then(|s| s.sunset);
                                    // Today's stand-out window, promoted from the multi-day strip.
                                    // Only surface it while it's still relevant (not fully past).
                                    let now = chrono::Utc::now();
                                    let best_window = forecast.await.ok().and_then(|mf| {
                                        mf.days
                                            .first()
                                            .and_then(|d| d.optimal_window.as_ref())
                                            .filter(|ow| ow.window.end > now)
                                            .map(|ow| BestWindowHint::from_window(ow, now, sunset))
                                    });
                                    // Daylight hint from the centre's sunset.
                                    let daylight = sunset.map(|s| {
                                        let t = s.with_timezone(&chrono::Local).format("%H:%M");
                                        if now > s {
                                            format!("Mørkt nå — solnedgang var {t}")
                                        } else {
                                            format!("Dagslys til {t}")
                                        }
                                    });
                                    let hourly_precip = agg.hourly_precip.clone();
                                    let produced_at = agg.produced_at;
                                    // Plain-language surface history, only when the pref is on.
                                    let show_rain = prefs.await.map(|p| p.show_rain_history).unwrap_or(false);
                                    let rain_line = show_rain
                                        .then(|| agg.rain_history.as_ref().map(|rh| {
                                            format!(
                                                "Underlag · siste {}t: {:.0} mm over {} regndag{}",
                                                rh.lookback_hours,
                                                rh.total_mm,
                                                rh.rain_days,
                                                if rh.rain_days == 1 { "" } else { "er" },
                                            )
                                        }))
                                        .flatten();
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
                                        <div class="space-y-6">
                                            <Recommendation total=agg.mean label=c.score.label.clone() reason=reason place=place best_window=best_window updated=produced_at/>
                                            <div class="space-y-6 rounded-2xl bg-gruv-bg1 p-6 shadow-lg ring-1 ring-gruv-bg2/60">
                                                {nowcast.map(|a| view! { <NowcastBanner alert=a/> })}
                                                <WindowStatsRow stats=stats/>
                                                <HourlyPrecipStrip hours=hourly_precip/>
                                                <div class="flex items-center gap-6">
                                                    <ScoreGauge total=agg.mean/>
                                                    <div class="flex-1">
                                                        <SubscoreBars breakdown=c.score.breakdown/>
                                                        <p class="mt-3 text-sm text-gruv-fg/70">
                                                            {format!("spenn {}–{} over {} punkter", agg.min, agg.max, agg.points.len())}
                                                        </p>
                                                    </div>
                                                </div>
                                                {(!highlights.is_empty()).then(|| view! {
                                                    <div class="space-y-1 rounded-xl bg-gruv-bg0/45 p-3 text-sm text-gruv-blue">
                                                        {highlights.into_iter().map(|h| view! { <p>{h}</p> }).collect_view()}
                                                    </div>
                                                })}
                                                {rain_line.map(|t| view! { <p class="text-sm text-gruv-fg/70">{t}</p> })}
                                                {daylight.map(|t| view! { <p class="text-sm text-gruv-fg/70">{t}</p> })}
                                                {(!chips.is_empty()).then(|| view! { <PenaltyChips penalties=chips/> })}
                                            </div>
                                            <MapView points=agg.points.clone()/>
                                        </div>
                                    }.into_any()
                                }
                                None => view! { <p class="mt-8 text-gruv-gray">"Ingen data."</p> }.into_any(),
                            }
                        }
                        Err(e) => view! {
                            <p class="mt-8 rounded-xl bg-gruv-red/20 p-4 text-gruv-red">
                                {format!("Kunne ikke hente prognose: {e}")}
                            </p>
                        }.into_any(),
                    }
                })}
            </Suspense>

            // ---- trend sparkline (hidden entirely until there's enough history) ----
            <Suspense>
                {move || Suspend::new(async move {
                    match history.await {
                        Ok(pts) if pts.len() >= 2 => view! {
                            <div class="rounded-2xl bg-gruv-bg1 p-6 shadow-lg ring-1 ring-gruv-bg2/60">
                                <div class="mb-3 flex items-center justify-between">
                                    <h2 class="text-sm font-semibold uppercase tracking-wide text-gruv-gray">
                                        "Trend"
                                    </h2>
                                    <span class="text-xs text-gruv-fg/70">"snitt over tid"</span>
                                </div>
                                <Sparkline points=pts/>
                            </div>
                        }.into_any(),
                        _ => ().into_any(),
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
                                        {mf.days.into_iter().map(|d| view! { <DayCard day=d/> }).collect_view()}
                                    </div>
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
    "mt-1 w-full rounded-lg border border-gruv-bg2 bg-gruv-bg0 px-3 py-2 text-sm outline-none focus:border-gruv-aqua"
}
fn btn_cls() -> &'static str {
    "rounded-lg bg-gruv-aqua px-4 py-2 text-sm font-semibold text-gruv-bg0 transition hover:brightness-110 disabled:opacity-50"
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
                                        <button class="text-gruv-red hover:underline"
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
