//! Presentational components for the dashboard. Each takes owned data (from an
//! awaited server-fn response) and renders with the shared score palette
//! ([`crate::color`]). Animations are plain CSS transitions.

use chrono::Local;
use leptos::prelude::*;

use grusindeks_core::aggregate::{DayAggregate, NowcastAlert};
use grusindeks_core::daily::{BestWindowReason, Confidence};
use grusindeks_core::score::{Component, Penalty, ScoreBreakdown, Severity, WindowStats};

use crate::color;
use crate::dto::HistoryPoint;

/// Norwegian one-word "why this window is best" phrase. The web UI is
/// Norwegian-only (like every other string here), so we hardcode the
/// Norwegian arm rather than plumbing a `Language` through every component.
fn best_window_reason_label(r: BestWindowReason) -> &'static str {
    match r {
        BestWindowReason::Mildest => "mildest",
        BestWindowReason::MinstKald => "minst kald",
        BestWindowReason::Vind => "minst vind",
        BestWindowReason::Nedbor => "tørrest",
    }
}

/// Pre-formatted "better window later today" hint for the recommendation card.
/// Built in `app.rs` from `MultiDayForecast::days[0].optimal_window`.
#[derive(Clone)]
pub struct BestWindowHint {
    pub start: String,
    pub end: String,
    pub improvement: u8,
    pub reason: Option<&'static str>,
    /// Absolute score of the window — lets the card decide whether it's worth
    /// suggesting the rider wait for it.
    pub total: u8,
    /// `true` when the window hasn't started yet (so "Vent til {start}" makes
    /// sense). Computed once against the wall clock in `app.rs`.
    pub starts_in_future: bool,
}

impl BestWindowHint {
    /// Build from an [`OptimalWindow`], formatting the window edges in local
    /// time. `now` is the wall clock used to decide whether the window is
    /// still ahead.
    pub fn from_window(
        ow: &grusindeks_core::daily::OptimalWindow,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Self {
        Self {
            start: ow.window.start.with_timezone(&Local).format("%H:%M").to_string(),
            end: ow.window.end.with_timezone(&Local).format("%H:%M").to_string(),
            improvement: ow.improvement,
            reason: ow.reason.map(best_window_reason_label),
            total: ow.score.total,
            starts_in_future: ow.window.start > now,
        }
    }
}

fn cta_for(total: u8) -> (&'static str, &'static str) {
    match total {
        85..=100 => ("Kjør nå", "Strålende forhold de neste 3 timene."),
        65..=84 => ("Kjør nå", "Gode forhold de neste 3 timene."),
        45..=64 => (
            "Vurder kort tur",
            "Brukbart, men med noen tydelige forbehold.",
        ),
        25..=44 => ("Vent hvis du kan", "Forholdene er svake akkurat nå."),
        _ => ("Ikke anbefalt", "Dårlige forhold for grus akkurat nå."),
    }
}

fn soft_bg_class(total: u8) -> &'static str {
    match color::bucket(total) {
        color::Bucket::Bad => "from-gruv-red/20 via-gruv-bg1 to-gruv-bg1 ring-gruv-red/30",
        color::Bucket::Marginal => {
            "from-gruv-orange/20 via-gruv-bg1 to-gruv-bg1 ring-gruv-orange/30"
        }
        color::Bucket::Ok => "from-gruv-yellow/20 via-gruv-bg1 to-gruv-bg1 ring-gruv-yellow/30",
        color::Bucket::Good => "from-gruv-lime/20 via-gruv-bg1 to-gruv-bg1 ring-gruv-lime/30",
        color::Bucket::Great => "from-gruv-green/20 via-gruv-bg1 to-gruv-bg1 ring-gruv-green/30",
    }
}

fn component_label(c: Component) -> &'static str {
    match c {
        Component::Temperature => "temperatur",
        Component::Wind => "vind",
        Component::Precipitation => "nedbør",
        Component::PrecipProbability => "nedbørssjanse",
        Component::Ground => "underlag",
        Component::HardCap => "været",
        Component::NoData => "datagrunnlaget",
    }
}

fn weakest_axis(b: ScoreBreakdown) -> (&'static str, u8) {
    [
        ("temperatur", b.temperature),
        ("vind", b.wind),
        ("nedbør", b.precipitation),
        ("nedbørssjanse", b.precip_probability),
        ("underlag", b.ground),
    ]
    .into_iter()
    .min_by_key(|(_, v)| *v)
    .unwrap_or(("forhold", 100))
}

pub fn score_reason(breakdown: ScoreBreakdown, penalties: &[Penalty]) -> String {
    if let Some(p) = penalties.first() {
        format!(
            "Trekkes ned av {}: {}",
            component_label(p.component),
            p.message
        )
    } else {
        let (axis, val) = weakest_axis(breakdown);
        if val >= 85 {
            "Ingen tydelige trekk — jevnt gode delscore.".to_string()
        } else {
            format!("Svakeste delscore er {axis} ({val}).")
        }
    }
}

/// A plain-language recommendation for the current score.
#[component]
pub fn Recommendation(
    total: u8,
    label: String,
    reason: String,
    place: String,
    /// "Better window later today" — promoted up from the multi-day strip so
    /// a rider sees the timing decision next to the verdict. `None` when no
    /// stand-out window beats the current conditions.
    best_window: Option<BestWindowHint>,
) -> impl IntoView {
    let (default_cta, default_summary) = cta_for(total);
    // Window-aware CTA: when the next 3 h aren't already good but a later
    // window today is meaningfully better, suggest waiting for it rather than
    // riding now.
    let wait_for = best_window
        .as_ref()
        .filter(|bw| total < 65 && bw.starts_in_future && bw.total >= total + 10);
    let (cta, summary) = match wait_for {
        Some(bw) => (format!("Vent til {}", bw.start), default_summary),
        None => (default_cta.to_string(), default_summary),
    };
    let updated = Local::now().format("%H:%M").to_string();
    let place = if place.trim().is_empty() {
        "standardsted".to_string()
    } else {
        place
    };
    view! {
        <div class=format!("rounded-2xl bg-gradient-to-br p-6 ring-1 shadow-xl {}", soft_bg_class(total))>
            <div class="flex items-start justify-between gap-4">
                <div>
                    <p class="text-xs font-semibold uppercase tracking-wide text-gruv-gray">
                        {format!("{place} · oppdatert {updated}")}
                    </p>
                    <h2 class="mt-2 text-3xl font-bold tracking-tight">{cta}</h2>
                    <p class="mt-1 text-sm text-gruv-fg/90">{summary}</p>
                </div>
                <span class=format!("rounded-full px-3 py-1 text-xs font-semibold ring-1 ring-current {}", color::text_class(total))>
                    {label}
                </span>
            </div>
            <p class="mt-4 rounded-xl bg-gruv-bg0/45 px-4 py-3 text-sm text-gruv-fg/90">{reason}</p>
            {best_window.map(|bw| view! {
                <p class="mt-3 flex flex-wrap items-center gap-x-2 text-sm font-semibold text-gruv-aqua">
                    <span>{format!("Bedre vindu {}–{}", bw.start, bw.end)}</span>
                    {bw.reason.map(|r| view! { <span class="font-normal text-gruv-fg/70">{format!("· {r}")}</span> })}
                    <span class="font-normal text-gruv-fg/60">{format!("+{} poeng", bw.improvement)}</span>
                </p>
            })}
        </div>
    }
}

/// Radial score gauge: a ring filled to `total`% in the score colour, with the
/// number and a verdict label in the middle.
#[component]
pub fn ScoreGauge(total: u8) -> impl IntoView {
    let r = 52.0_f64;
    let circ = 2.0 * std::f64::consts::PI * r;
    let offset = circ * (1.0 - (total as f64 / 100.0));
    let hex = color::hex(total);
    view! {
        <div class="relative inline-grid place-items-center">
            <svg width="150" height="150" viewBox="0 0 150 150" class="-rotate-90">
                <circle cx="75" cy="75" r=r.to_string() fill="none" stroke="#504945" stroke-width="12"/>
                <circle
                    cx="75" cy="75" r=r.to_string() fill="none"
                    stroke=hex stroke-width="12" stroke-linecap="round"
                    stroke-dasharray=circ.to_string()
                    stroke-dashoffset=offset.to_string()
                    style="transition: stroke-dashoffset 900ms cubic-bezier(0.22,1,0.36,1);"
                />
            </svg>
            <div class="absolute text-center">
                <div class=format!("text-4xl font-bold tabular-nums {}", color::text_class(total))>
                    {total}
                </div>
                <div class="text-xs text-gruv-fg/60">"indeks"</div>
            </div>
        </div>
    }
}

/// Compact real-world numbers (°C / m/s / mm) for the current window. Renders
/// nothing on the empty-window (NaN) path so we never print "NaN°C".
#[component]
pub fn WindowStatsRow(stats: WindowStats) -> impl IntoView {
    if stats.is_empty() {
        return ().into_any();
    }
    let temp = format!("{:.0}°C", stats.mean_temp_c);
    let temp_range = format!("{:.0}–{:.0}", stats.min_temp_c, stats.max_temp_c);
    let wind = match stats.max_gust_ms {
        Some(g) if g > stats.max_wind_ms => format!("{:.0} m/s · kast {:.0}", stats.max_wind_ms, g),
        _ => format!("{:.0} m/s", stats.max_wind_ms),
    };
    let rain = if stats.total_precip_mm > 0.0 {
        format!("{:.1} mm", stats.total_precip_mm)
    } else {
        "tørt".to_string()
    };
    let cell = "flex flex-col gap-0.5";
    let metric = "text-base font-semibold tabular-nums text-gruv-fg";
    let cap = "text-xs uppercase tracking-wide text-gruv-fg/70";
    view! {
        <div class="grid grid-cols-3 gap-3">
            <div class=cell>
                <span class=metric>{temp}</span>
                <span class=cap>{format!("temp {temp_range}")}</span>
            </div>
            <div class=cell>
                <span class=metric>{wind}</span>
                <span class=cap>"vind"</span>
            </div>
            <div class=cell>
                <span class=metric>{rain}</span>
                <span class=cap>"nedbør 3t"</span>
            </div>
        </div>
    }
    .into_any()
}

/// Five per-axis sub-score bars from a [`ScoreBreakdown`]. Bars are monochrome
/// (aqua) by default so colour stays meaningful: only sub-scores that are
/// actually weak turn orange/red. A faint tick marks the "good" cutoff (65) so
/// the bars read positionally, not by colour alone.
#[component]
pub fn SubscoreBars(breakdown: ScoreBreakdown) -> impl IntoView {
    let rows = [
        ("Temp", breakdown.temperature),
        ("Vind", breakdown.wind),
        ("Nedbør", breakdown.precipitation),
        ("Sjanse", breakdown.precip_probability),
        ("Bakke", breakdown.ground),
    ];
    view! {
        <div class="space-y-2.5">
            {rows.into_iter().map(|(name, val)| {
                let bar = color::bar_fill_class(val);
                view! {
                    <div class="flex items-center gap-3 text-sm">
                        <span class="w-16 text-gruv-fg/70">{name}</span>
                        <div class="relative h-2.5 flex-1 overflow-hidden rounded-full bg-gruv-bg2">
                            <div
                                class=format!("h-full rounded-full {bar}")
                                style=format!("width:{val}%; transition: width 700ms cubic-bezier(0.22,1,0.36,1);")
                            ></div>
                            <div class="absolute inset-y-0 w-px bg-gruv-bg0/70" style="left:65%"></div>
                        </div>
                        <span class="w-8 text-right tabular-nums">{val}</span>
                    </div>
                }
            }).collect_view()}
        </div>
    }
}

/// Penalty chips, coloured by severity (worst = red).
#[component]
pub fn PenaltyChips(penalties: Vec<Penalty>) -> impl IntoView {
    view! {
        <div class="flex flex-wrap gap-2">
            {penalties.into_iter().map(|p| {
                let cls = match p.severity {
                    Severity::Critical => "bg-gruv-red/20 text-gruv-red",
                    Severity::Major => "bg-gruv-orange/20 text-gruv-orange",
                    Severity::Minor => "bg-gruv-yellow/20 text-gruv-yellow",
                };
                view! {
                    <span class=format!("rounded-full px-3 py-1 text-xs {cls}")>{p.message}</span>
                }
            }).collect_view()}
        </div>
    }
}

/// Imminent-rain banner from a radar nowcast. "Regn om N min" is computed
/// client-side from the absolute UTC times in the alert.
#[component]
pub fn NowcastBanner(alert: NowcastAlert) -> impl IntoView {
    use chrono::Utc;
    let mins = (alert.first_rain_at - Utc::now()).num_minutes().max(0);
    let peak = alert.peak_mm_h;
    let cls = if peak >= 2.0 {
        "bg-gruv-red/20 text-gruv-red"
    } else if peak >= 0.5 {
        "bg-gruv-orange/20 text-gruv-orange"
    } else {
        "bg-gruv-yellow/20 text-gruv-yellow"
    };
    view! {
        <div class=format!("flex items-center gap-2 rounded-xl px-4 py-3 text-sm {cls}")>
            <span>"🌧"</span>
            <span>{format!("Regn på radar om ~{mins} min (topp {peak:.1} mm/t)")}</span>
        </div>
    }
}

/// Trend sparkline of mean scores over time (0–100 fixed y-scale). Renders an
/// SVG polyline coloured by the most recent value; the last point gets a dot.
#[component]
pub fn Sparkline(points: Vec<HistoryPoint>) -> impl IntoView {
    if points.len() < 2 {
        return view! {
            <p class="text-xs text-gruv-gray">"For lite historikk ennå — kommer etter hvert."</p>
        }
        .into_any();
    }
    let w = 280.0_f64;
    let h = 44.0_f64;
    let n = points.len();
    let last = points.last().map(|p| p.mean).unwrap_or(0);
    let stroke = color::hex(last);
    let poly: String = points
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let x = (i as f64) / ((n - 1) as f64) * w;
            let y = h - (p.mean as f64 / 100.0) * h;
            format!("{x:.1},{y:.1}")
        })
        .collect::<Vec<_>>()
        .join(" ");
    let (lx, ly) = {
        let x = w;
        let y = h - (last as f64 / 100.0) * h;
        (x, y)
    };
    let min = points.iter().map(|p| p.mean).min().unwrap_or(last);
    let max = points.iter().map(|p| p.mean).max().unwrap_or(last);
    view! {
        <div>
            <div class="mb-3 flex items-end justify-between gap-4">
                <div>
                    <div class=format!("text-2xl font-bold tabular-nums {}", color::text_class(last))>{last}</div>
                    <div class="text-xs text-gruv-fg/70">"siste måling"</div>
                </div>
                <div class="text-right text-xs text-gruv-fg/70">
                    <div>{format!("min {min} · maks {max}")}</div>
                    <div>"siste 48 timer"</div>
                </div>
            </div>
            <svg width="100%" viewBox=format!("0 0 {w} {h}") preserveAspectRatio="none" class="w-full overflow-visible">
                <line x1="0" y1=format!("{:.1}", h / 2.0) x2=w.to_string() y2=format!("{:.1}", h / 2.0) stroke="#504945" stroke-width="1" stroke-dasharray="3 4"/>
                <polyline
                    points=poly fill="none" stroke=stroke stroke-width="2.5"
                    stroke-linecap="round" stroke-linejoin="round"
                />
                <circle cx=format!("{lx:.1}") cy=format!("{ly:.1}") r="3" fill=stroke/>
            </svg>
            <div class="mt-2 flex justify-between text-xs uppercase tracking-wide text-gruv-fg/70">
                <span>"48t"</span>
                <span>"nå"</span>
            </div>
        </div>
    }
    .into_any()
}

fn confidence_label(c: Confidence) -> &'static str {
    match c {
        Confidence::Hoy => "høy tillit",
        Confidence::Middels => "middels tillit",
        Confidence::Lav => "lav tillit",
    }
}

/// A non-colour-only confidence cue: literal dot colour by confidence band.
fn confidence_dot(c: Confidence) -> &'static str {
    match c {
        Confidence::Hoy => "bg-gruv-green",
        Confidence::Middels => "bg-gruv-yellow",
        Confidence::Lav => "bg-gruv-gray",
    }
}

/// One day in the multi-day strip: weather icon, coloured mean, spread, the
/// best sub-window, and a confidence note. Long-range low-confidence days are
/// dimmed.
#[component]
pub fn DayCard(day: DayAggregate) -> impl IntoView {
    let mean = day.mean;
    let date = day.date.format("%a %d.%m").to_string();
    let icon = day.center().weather_icon.clone();
    let low = day.confidence == Confidence::Lav;
    // Low-confidence days are signalled by a dashed border and a neutral mean
    // colour — never by dimming, which would drop the numbers below readable
    // contrast.
    let border = if low {
        "border border-dashed border-gruv-bg2"
    } else {
        "ring-1 ring-gruv-bg2/60"
    };
    let mean_color = if low { "text-gruv-fg/70" } else { color::text_class(mean) };

    let st = &day.center().score.stats;
    let weather = (!st.is_empty())
        .then(|| format!("{:.0}° · {:.1} mm", st.max_temp_c, st.total_precip_mm));

    let best = day.optimal_window.as_ref().map(|ow| {
        let s = ow.window.start.with_timezone(&Local).format("%H:%M");
        let e = ow.window.end.with_timezone(&Local).format("%H:%M");
        match ow.reason.map(best_window_reason_label) {
            Some(r) => format!("beste {s}–{e} · {r}"),
            None => format!("beste {s}–{e}"),
        }
    });

    view! {
        <div class=format!("min-w-[8.5rem] flex-1 rounded-xl bg-gruv-bg1 p-4 shadow-lg transition hover:-translate-y-0.5 {border}")>
            <div class="flex items-center justify-between">
                <span class="text-sm text-gruv-fg/70">{date}</span>
                <span class="text-lg">{icon}</span>
            </div>
            <div class=format!("mt-2 text-2xl font-bold tabular-nums {mean_color}")>
                {mean}
            </div>
            <div class="text-xs text-gruv-fg/70">{format!("{}–{}", day.min, day.max)}</div>
            {weather.map(|w| view! { <div class="mt-1 text-xs text-gruv-fg/70">{w}</div> })}
            {best.map(|b| view! { <div class="mt-1 text-xs text-gruv-aqua">{b}</div> })}
            <div class="mt-2 flex items-center gap-1.5 text-xs text-gruv-fg/70">
                <span class=format!("h-1.5 w-1.5 rounded-full {}", confidence_dot(day.confidence))></span>
                {confidence_label(day.confidence)}
            </div>
        </div>
    }
}
