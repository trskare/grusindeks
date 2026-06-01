//! Presentational components for the dashboard. Each takes owned data (from an
//! awaited server-fn response) and renders with the shared score palette
//! ([`crate::color`]). Animations are plain CSS transitions.

use chrono::{DateTime, Duration, Local, TimeZone, Timelike, Utc};
use leptos::prelude::*;

use grusindeks_core::aggregate::{
    DayAggregate, HourScore, HourlyDayAggregate, HourlyPrecip, NowcastAlert,
};
use grusindeks_core::daily::{BestWindowReason, Confidence};
use grusindeks_core::score::{Component, Penalty, ScoreBreakdown, Severity, WindowStats};
use grusindeks_core::types::RideWindow;

use crate::color;
use crate::dto::HistoryPoint;
use crate::icons;

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
    /// `true` when the window starts before sunset (or sunset is unknown) — we
    /// don't suggest waiting for a window that lands in the dark.
    pub before_sunset: bool,
    /// `true` when the window is on a later local date than `now` (e.g. the
    /// forecast rolled to tomorrow once today's ride window was over) — the card
    /// labels it "i morgen" and never offers a "vent til" today.
    pub tomorrow: bool,
}

impl BestWindowHint {
    /// Build from an [`OptimalWindow`], formatting the window edges in local
    /// time. `now` is the wall clock used to decide whether the window is
    /// still ahead; `sunset` (if known) gates the "wait for it" suggestion.
    pub fn from_window(
        ow: &grusindeks_core::daily::OptimalWindow,
        now: DateTime<Utc>,
        sunset: Option<DateTime<Utc>>,
    ) -> Self {
        Self {
            start: ow
                .window
                .start
                .with_timezone(&Local)
                .format("%H:%M")
                .to_string(),
            end: ow
                .window
                .end
                .with_timezone(&Local)
                .format("%H:%M")
                .to_string(),
            improvement: ow.improvement,
            reason: ow.reason.map(best_window_reason_label),
            total: ow.score.total,
            starts_in_future: ow.window.start > now,
            before_sunset: sunset.is_none_or(|s| ow.window.start < s),
            tomorrow: ow.window.start.with_timezone(&Local).date_naive()
                != now.with_timezone(&Local).date_naive(),
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

/// Domain icon for a penalty's component — keeps the reason chips in the same
/// visual language as the lane gutter and the stat tiles.
fn penalty_icon(c: Component, class: &'static str) -> AnyView {
    match c {
        Component::Temperature => icons::thermometer(class, None).into_any(),
        Component::Wind => icons::wind(class, None).into_any(),
        Component::Precipitation => icons::cloud_rain(class, None).into_any(),
        Component::PrecipProbability => icons::umbrella(class, None).into_any(),
        Component::Ground => icons::mountain(class, None).into_any(),
        Component::HardCap | Component::NoData => icons::cloud_rain(class, None).into_any(),
    }
}

fn severity_chip_class(s: Severity) -> &'static str {
    match s {
        Severity::Critical => "bg-gruv-red/20 text-gruv-red",
        Severity::Major => "bg-gruv-orange/20 text-gruv-orange",
        Severity::Minor => "bg-gruv-yellow/20 text-gruv-yellow",
    }
}

/// A plain-language recommendation for the current score.
#[component]
pub fn Recommendation(
    total: u8,
    label: String,
    /// Score penalties for the scored window — rendered as "trekkes ned av"
    /// chips. Empty → a positive note derived from the breakdown.
    penalties: Vec<Penalty>,
    /// The window's sub-score breakdown — drives the positive note when there
    /// are no penalties.
    breakdown: ScoreBreakdown,
    /// The next-3-hour measured numbers (°C / m/s / mm), shown as tiles right
    /// under the verdict — same window the verdict describes.
    stats: WindowStats,
    place: String,
    /// "Better window later today" — promoted up from the multi-day strip so
    /// a rider sees the timing decision next to the verdict. `None` when no
    /// stand-out window beats the current conditions.
    best_window: Option<BestWindowHint>,
    /// When the score was computed (server clock). Falls back to the client
    /// clock when the server didn't stamp it. Also anchors the "neste 3 timer"
    /// horizon badge (start → start + 3 h).
    updated: Option<DateTime<Utc>>,
) -> impl IntoView {
    let (default_cta, default_summary) = cta_for(total);
    // Window-aware CTA: when the next 3 h aren't already good but a later
    // window today is meaningfully better — and still in daylight — suggest
    // waiting for it rather than riding now.
    let wait_for = best_window.as_ref().filter(|bw| {
        total < 65
            && bw.starts_in_future
            && bw.before_sunset
            && bw.total >= total + 10
            && !bw.tomorrow
    });
    let waiting = wait_for.is_some();
    let (cta, summary) = match wait_for {
        Some(bw) => (
            format!("Vent til {}", bw.start),
            format!(
                "Bedre forhold senere i dag — +{} poeng fra {}.",
                bw.improvement, bw.start
            ),
        ),
        None => (default_cta.to_string(), default_summary.to_string()),
    };
    let updated_dt = updated.unwrap_or_else(Utc::now).with_timezone(&Local);
    let updated_str = updated_dt.format("%H:%M").to_string();
    let horizon = format!(
        "{}–{}",
        updated_dt.format("%H:%M"),
        (updated_dt + Duration::hours(3)).format("%H:%M")
    );
    let place = if place.trim().is_empty() {
        "standardsted".to_string()
    } else {
        place
    };
    let positive = penalties.is_empty().then(|| score_reason(breakdown, &[]));
    view! {
        <div class=format!("emboss rounded-2xl bg-gradient-to-br p-6 {}", soft_bg_class(total))>
            <div class="flex items-start justify-between gap-4">
                <div class="min-w-0">
                    // Horizon badge — makes the (otherwise invisible) 3-hour scope explicit.
                    <div class="flex flex-wrap items-center gap-x-2 gap-y-1">
                        <span class="inline-flex items-center gap-1 rounded-full bg-gruv-bg0/60 px-2 py-0.5 text-[11px] font-semibold uppercase tracking-wide text-gruv-fg/80">
                            {icons::clock("h-3 w-3", None)}
                            {format!("neste 3 timer · {horizon}")}
                        </span>
                        <span class="text-xs font-medium text-gruv-gray">
                            {format!("· {place} · oppdatert {updated_str}")}
                        </span>
                    </div>
                    <h2 class="mt-2 text-3xl font-bold tracking-tight">{cta}</h2>
                    <p class="mt-1 text-sm text-gruv-fg/90">{summary}</p>
                </div>
                <span class=format!("shrink-0 rounded-full px-3 py-1 text-xs font-semibold ring-1 ring-current/40 {}", color::text_class(total))>
                    {label}
                </span>
            </div>

            // The next-3-hour measured numbers, same window the verdict describes.
            <div class="mt-4">
                <WindowStatsRow stats=stats/>
            </div>

            // Reason — "trekkes ned av" chips (or a positive note when clean).
            <div class="mt-4 rounded-xl inset-well bg-gruv-bg0/55 px-4 py-3">
                {match positive {
                    Some(note) => view! { <p class="text-sm text-gruv-fg/90">{note}</p> }.into_any(),
                    None => view! {
                        <p class="mb-2 text-[11px] font-semibold uppercase tracking-wide text-gruv-fg/55">"Trekkes ned av"</p>
                        <div class="flex flex-wrap gap-2">
                            {penalties.into_iter().map(|p| {
                                let cls = severity_chip_class(p.severity);
                                view! {
                                    <span class=format!("inline-flex items-center gap-1.5 rounded-lg px-2.5 py-1 text-sm {cls}")>
                                        {penalty_icon(p.component, "h-3.5 w-3.5")}
                                        {p.message}
                                    </span>
                                }
                            }).collect_view()}
                        </div>
                    }.into_any(),
                }}
            </div>

            // Better window — later today, or tomorrow once today's window is over.
            {best_window.map(|bw| {
                let breathe = if waiting { "gx-breathe" } else { "" };
                let when = if bw.tomorrow { "Bedre vindu i morgen" } else { "Bedre vindu senere" };
                view! {
                    <div class=format!("mt-2 flex flex-wrap items-center gap-x-2 gap-y-1 rounded-xl inset-well bg-gruv-aqua/10 px-4 py-2.5 text-sm ring-1 ring-inset ring-gruv-aqua/25 {breathe}")>
                        {icons::bike("h-4 w-4 text-gruv-aqua", None)}
                        <span class="text-[11px] font-semibold uppercase tracking-wide text-gruv-aqua/90">{when}</span>
                        <span class="font-semibold tabular-nums text-gruv-fg">{format!("{}–{}", bw.start, bw.end)}</span>
                        {bw.reason.map(|r| view! { <span class="text-gruv-fg/70">{r}</span> })}
                        <span class="ml-auto rounded-md bg-gruv-aqua/20 px-1.5 py-0.5 text-xs font-bold tabular-nums text-gruv-aqua">
                            {format!("+{}", bw.improvement)}
                        </span>
                    </div>
                }
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
                    style=format!("transition: stroke-dashoffset 900ms cubic-bezier(0.22,1,0.36,1); filter: drop-shadow(0 0 6px {hex}40);")
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
    // Lead the caption with "feels like" when it diverges from the air temp by
    // a degree or more; otherwise just show the range.
    let temp_sub =
        if stats.felt_temp_c.is_finite() && (stats.felt_temp_c - stats.mean_temp_c).abs() >= 1.0 {
            format!(
                "føles {:.0}° · {:.0}–{:.0}",
                stats.felt_temp_c, stats.min_temp_c, stats.max_temp_c
            )
        } else {
            format!("temp {:.0}–{:.0}", stats.min_temp_c, stats.max_temp_c)
        };
    let wind_main = format!("{:.0} m/s", stats.max_wind_ms);
    let wind_sub = match stats.max_gust_ms {
        Some(g) if g > stats.max_wind_ms => format!("kast {g:.0}"),
        _ => String::new(),
    };
    let rain = if stats.total_precip_mm > 0.0 {
        format!("{:.1} mm", stats.total_precip_mm)
    } else {
        "tørt".to_string()
    };
    // Each metric in its own inset-well tile; the lane icon replaces the text
    // caption (the unit stays in the value so "4" is never ambiguous).
    let cell = "flex flex-col gap-1 rounded-xl inset-well bg-gruv-bg0/40 px-3.5 py-3";
    let metric = "text-2xl font-bold leading-none tabular-nums text-gruv-fg";
    let cap = "text-xs text-gruv-fg/55";
    let icon = "h-4 w-4 text-gruv-fg/45";
    view! {
        <div class="grid grid-cols-3 gap-2.5">
            <div class=cell>
                {icons::thermometer(icon, Some("Temperatur"))}
                <span class=metric>{temp}</span>
                <span class=cap>{temp_sub}</span>
            </div>
            <div class=cell>
                {icons::wind(icon, Some("Vind"))}
                <span class=metric>{wind_main}</span>
                <span class=cap>{wind_sub}</span>
            </div>
            <div class=cell>
                {icons::cloud_rain(icon, Some("Nedbør"))}
                <span class=metric>{rain}</span>
            </div>
        </div>
    }
    .into_any()
}

/// A per-hour precipitation mini-chart for the scored window — answers "when
/// inside the next 3 h does it actually rain?". Renders nothing on a dry
/// window (the stats row already says "tørt").
#[component]
pub fn HourlyPrecipStrip(hours: Vec<HourlyPrecip>) -> impl IntoView {
    let max = hours.iter().map(|h| h.precip_mm).fold(0.0_f64, f64::max);
    if hours.len() < 2 || max < 0.05 {
        return ().into_any();
    }
    view! {
        <div class="space-y-1.5">
            <p class="text-xs uppercase tracking-wide text-gruv-fg/70">"Nedbør per time"</p>
            <div class="flex items-end gap-2">
                {hours.into_iter().map(|h| {
                    let label = h.time.with_timezone(&Local).format("%H").to_string();
                    let wet = h.precip_mm >= 0.05;
                    // Floor the height so a wet hour is always visibly taller than a dry one.
                    let pct = if wet { ((h.precip_mm / max) * 100.0).clamp(12.0, 100.0) } else { 4.0 };
                    let amt = if wet { format!("{:.1}", h.precip_mm) } else { "·".to_string() };
                    let fill = if wet { "bg-gruv-blue" } else { "bg-gruv-bg2" };
                    view! {
                        <div class="flex flex-1 flex-col items-center gap-1">
                            <span class="text-xs tabular-nums text-gruv-fg/70">{amt}</span>
                            <div class="flex h-10 w-full items-end">
                                <div class=format!("w-full rounded-t {fill}") style=format!("height:{pct:.0}%")></div>
                            </div>
                            <span class="text-xs tabular-nums text-gruv-fg/70">{label}</span>
                        </div>
                    }
                }).collect_view()}
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
                let icon = match name {
                    "Temp" => icons::thermometer("h-3.5 w-3.5", None).into_any(),
                    "Vind" => icons::wind("h-3.5 w-3.5", None).into_any(),
                    "Nedbør" => icons::cloud_rain("h-3.5 w-3.5", None).into_any(),
                    "Sjanse" => icons::umbrella("h-3.5 w-3.5", None).into_any(),
                    _ => icons::mountain("h-3.5 w-3.5", None).into_any(),
                };
                // `val` is the 0–100 quality sub-score (higher = better). The fill
                // colour is the red→yellow→green quality ramp for every row.
                // "Sjanse" is special: the sub-score is `100 − rain-probability`,
                // so we surface the *actual* rain chance as the number and the
                // bar width (longer = more likely → redder), while still colouring
                // by quality. The 65 "good" tick only makes sense for quality bars.
                let is_prob = name == "Sjanse";
                let hex = color::hex(val);
                let width = if is_prob { 100u8.saturating_sub(val) } else { val };
                let display = if is_prob {
                    format!("{}%", 100u8.saturating_sub(val))
                } else {
                    val.to_string()
                };
                view! {
                    <div class="flex items-center gap-3 text-sm">
                        <span class="flex w-20 items-center gap-1.5 text-gruv-fg/70">{icon}{name}</span>
                        <div class="relative h-2.5 flex-1 overflow-hidden rounded-full bg-gruv-bg2">
                            <div
                                class="h-full rounded-full"
                                style=format!("width:{width}%; background-color:{hex}; transition: width 700ms cubic-bezier(0.22,1,0.36,1);")
                            ></div>
                            {(!is_prob).then(|| view! {
                                <div class="absolute inset-y-0 w-px bg-gruv-bg0/70" style="left:65%"></div>
                            })}
                        </div>
                        <span class="w-9 text-right tabular-nums">{display}</span>
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
    // Compact: just the line + a single caption row, so the trend stays a small
    // (~20%) companion to the map in the right column.
    view! {
        <div>
            <svg width="100%" viewBox=format!("0 0 {w} {h}") preserveAspectRatio="none" class="h-12 w-full overflow-visible">
                <line x1="0" y1=format!("{:.1}", h / 2.0) x2=w.to_string() y2=format!("{:.1}", h / 2.0) stroke="#504945" stroke-width="1" stroke-dasharray="3 4"/>
                <polyline
                    points=poly fill="none" stroke=stroke stroke-width="2.5"
                    stroke-linecap="round" stroke-linejoin="round"
                />
                <circle cx=format!("{lx:.1}") cy=format!("{ly:.1}") r="3" fill=stroke/>
            </svg>
            <div class="mt-2 flex items-center justify-between text-[10px] uppercase tracking-wide text-gruv-fg/55">
                <span>"14d"</span>
                <span class="normal-case tabular-nums">{format!("min {min} · maks {max}")}</span>
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
pub fn DayCard(
    day: DayAggregate,
    #[prop(optional)] on_select: Option<Callback<(chrono::NaiveDate, i32, i32)>>,
) -> impl IntoView {
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
    let mean_color = if low {
        "text-gruv-fg/70"
    } else {
        color::text_class(mean)
    };

    let st = &day.center().score.stats;
    let weather =
        (!st.is_empty()).then(|| format!("{:.0}° · {:.1} mm", st.max_temp_c, st.total_precip_mm));

    let best = day.optimal_window.as_ref().map(|ow| {
        let s = ow.window.start.with_timezone(&Local).format("%H:%M");
        let e = ow.window.end.with_timezone(&Local).format("%H:%M");
        match ow.reason.map(best_window_reason_label) {
            Some(r) => format!("beste {s}–{e} · {r}"),
            None => format!("beste {s}–{e}"),
        }
    });

    let selected_date = day.date;

    view! {
        <button type="button"
            class=format!("min-w-[8.5rem] flex-1 rounded-xl bg-gruv-bg1 p-4 text-left shadow-lg transition hover:-translate-y-0.5 focus:outline-none focus:ring-2 focus:ring-gruv-aqua/70 {border}")
            on:click=move |ev| {
                if let Some(cb) = on_select {
                    cb.run((selected_date, ev.client_x(), ev.client_y()));
                }
            }
        >
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
        </button>
    }
}

/// Compact lane timeline for the popup opened from the multi-day strip.
#[component]
pub fn CompactDayTimeline(
    day: HourlyDayAggregate,
    best_window: Option<RideWindow>,
) -> impl IntoView {
    let date = day.date.format("%a %d.%m").to_string();
    let hours = day.hours;
    if hours.is_empty() {
        return view! { <p class="text-sm text-gruv-fg/70">"Ingen timesdata for dagen."</p> }
            .into_any();
    }

    let best = best_window.map(|w| {
        (
            w.start.with_timezone(&Local).format("%H:%M").to_string(),
            w.end.with_timezone(&Local).format("%H:%M").to_string(),
            w,
        )
    });
    let in_best = |h: &HourScore| {
        best.as_ref().is_some_and(|(_, _, w)| {
            let e = h.time + Duration::hours(1);
            h.time < w.end && e > w.start
        })
    };
    let grid = format!(
        "grid-template-columns: repeat({}, minmax(3.3rem, 1fr));",
        hours.len()
    );
    let cell = "flex h-9 items-center justify-center border-l border-gruv-bg2/50 text-[11px] tabular-nums first:border-l-0";

    view! {
        <div>
            <div class="mb-3 flex flex-wrap items-center justify-between gap-2">
                <div>
                    <h3 class="text-sm font-semibold uppercase tracking-wide text-gruv-gray">"Time for time"</h3>
                    <p class="text-xs text-gruv-fg/60">{date}</p>
                </div>
                {best.as_ref().map(|(s, e, _)| view! {
                    <span class="inline-flex items-center gap-1 rounded-full bg-gruv-aqua/15 px-2 py-1 text-xs font-semibold text-gruv-aqua ring-1 ring-gruv-aqua/30">
                        {icons::bike("h-3 w-3", None)}
                        {format!("beste {s}–{e}")}
                    </span>
                })}
            </div>

            <div class="overflow-x-auto rounded-xl bg-gruv-bg0/45 inset-well">
                <div class="min-w-max">
                    <div class="ml-10 grid h-6 text-[10px] text-gruv-fg/50" style=grid.clone()>
                        {hours.iter().map(|h| view! {
                            <div class="flex items-center justify-center tabular-nums">
                                {h.time.with_timezone(&Local).format("%H").to_string()}
                            </div>
                        }).collect_view()}
                    </div>

                    <div class="flex border-t border-gruv-bg2/70">
                        <div class="flex w-10 shrink-0 items-center justify-center text-gruv-fg/55">{icons::thermometer("h-4 w-4", Some("Temperatur"))}</div>
                        <div class="grid flex-1" style=grid.clone()>
                            {hours.iter().map(|h| {
                                let best_cls = if in_best(h) { " bg-gruv-aqua/15 ring-1 ring-inset ring-gruv-aqua/50" } else { "" };
                                let bg = color::hex(h.breakdown.temperature);
                                view! {
                                    <div class=format!("{cell}{best_cls}") style=format!("background:linear-gradient(to top,{bg}55,{bg}18)")>
                                        {h.raw.map(|r| format!("{:.0}°", r.temperature_c)).unwrap_or_else(|| "–".to_string())}
                                    </div>
                                }
                            }).collect_view()}
                        </div>
                    </div>

                    <div class="flex border-t border-gruv-bg2/70">
                        <div class="flex w-10 shrink-0 items-center justify-center text-gruv-fg/55">{icons::wind("h-4 w-4", Some("Vind"))}</div>
                        <div class="grid flex-1" style=grid.clone()>
                            {hours.iter().map(|h| {
                                let best_cls = if in_best(h) { " bg-gruv-aqua/15 ring-1 ring-inset ring-gruv-aqua/50" } else { "" };
                                let bg = color::hex(h.breakdown.wind);
                                view! {
                                    <div class=format!("{cell}{best_cls}") style=format!("background:linear-gradient(to top,{bg}55,{bg}18)")>
                                        {h.raw.map(|r| format!("{:.0}", r.wind_speed_ms)).unwrap_or_else(|| "–".to_string())}
                                    </div>
                                }
                            }).collect_view()}
                        </div>
                    </div>

                    <div class="flex border-t border-gruv-bg2/70">
                        <div class="flex w-10 shrink-0 items-center justify-center text-gruv-fg/55">{icons::cloud_rain("h-4 w-4", Some("Nedbør"))}</div>
                        <div class="grid flex-1" style=grid>
                            {hours.iter().map(|h| {
                                let best_cls = if in_best(h) { " bg-gruv-aqua/15 ring-1 ring-inset ring-gruv-aqua/50" } else { "" };
                                let wet = h.raw.is_some_and(|r| r.precipitation_mm >= 0.05);
                                let bg = color::hex(h.breakdown.precipitation);
                                view! {
                                    <div class=format!("{cell}{best_cls}") style=format!("background:linear-gradient(to top,{bg}55,{bg}18)")>
                                        {if wet {
                                            view! { <span class="text-gruv-blue">{icons::droplet("h-4 w-4", Some("Regn"))}</span> }.into_any()
                                        } else {
                                            view! { <span class="text-gruv-fg/35">"·"</span> }.into_any()
                                        }}
                                    </div>
                                }
                            }).collect_view()}
                        </div>
                    </div>
                </div>
            </div>

            <div class="mt-2 flex flex-wrap gap-x-3 gap-y-1 text-[10px] text-gruv-fg/55">
                <span>"Vind: m/s"</span>
                <span class="flex items-center gap-1 text-gruv-blue">{icons::droplet("h-3 w-3", None)}"nedbør"</span>
                <span class="text-gruv-aqua">"markert felt = beste vindu"</span>
            </div>
        </div>
    }.into_any()
}

// ---------------- Ride timeline ----------------
//
// A horizontal hour-by-hour timeline for the rest of today: five stacked lanes
// (Temp / Vind / Nedbør / Sjanse / Bakke) over a shared time axis, with the
// best riding window, rain periods, the "now" marker, and dusk drawn as
// overlays. The lane *geometry* is one inline SVG (stretched with
// `preserveAspectRatio="none"`, like [`Sparkline`]); all text/chrome is
// absolutely-positioned HTML so it stays crisp under that non-uniform scale.

/// Plot geometry in SVG user units (the viewBox is `0 0 W H`). The plot is
/// stretched to the container width, so x is effectively a 0–1000 fraction.
const TL_W: f64 = 1000.0;
/// Lane height in SVG/px units (viewBox is 1:1 with the rendered plot). ~30 %
/// taller than the original 32 for more breathing room per lane.
const TL_LANE_H: f64 = 42.0;
const TL_LANES: usize = 5;
const TL_H: f64 = TL_LANE_H * TL_LANES as f64;
/// Vertical inset for value lines inside a lane, so peaks/troughs don't touch
/// the lane edges.
const TL_PAD: f64 = 4.0;

fn n1(v: f64) -> String {
    format!("{v:.1}")
}
/// Format a 0–1 fraction as a CSS percentage with enough precision that
/// overlays line up with the SVG columns.
fn pct(f: f64) -> String {
    format!("{:.3}%", f * 100.0)
}

/// One scored hour, pre-projected onto the timeline's x-axis.
struct TlCol<'a> {
    /// Left/right/centre as 0–1 fractions of the domain.
    l: f64,
    r: f64,
    mid: f64,
    hour: u32,
    low: bool,
    s: &'a HourScore,
}

/// Map a temperature/wind value into a lane's inner y-band (inverted: higher
/// value → higher on screen). `lane` is the lane index (0-based).
fn lane_y(lane: usize, value: f64, min: f64, max: f64) -> f64 {
    let top = lane as f64 * TL_LANE_H + TL_PAD;
    let h = TL_LANE_H - 2.0 * TL_PAD;
    let range = (max - min).max(0.1);
    top + (1.0 - ((value - min) / range).clamp(0.0, 1.0)) * h
}

#[component]
pub fn RideTimeline(
    day: HourlyDayAggregate,
    /// Today's best sub-window (from the multi-day forecast's day 0). Drawn as
    /// the aqua window box; `None` hides it.
    best_window: Option<RideWindow>,
    /// Centre sunrise for the shown day — drives the dawn glow + night shading.
    sunrise: Option<DateTime<Utc>>,
    /// Centre sunset for the shown day — drives the dusk glow + night shading.
    sunset: Option<DateTime<Utc>>,
    /// Wall clock captured once by the caller.
    now: DateTime<Utc>,
) -> impl IntoView {
    let date = day.date;
    let hours = day.hours;
    if hours.is_empty() {
        return view! {
            <p class="text-sm text-gruv-fg/70">"Ingen timesdata for dagen."</p>
        }
        .into_any();
    }

    // ---- domain: "now → end of the shown day". MET publishes no past hours,
    // so a full 00–24 axis would be mostly empty in the evening; instead we
    // start at `now` for today (the timeline is always full of forecast) and at
    // local midnight for a future day. End is that day's midnight. Local
    // midnight is resolved client-side.
    let local_midnight = |d: chrono::NaiveDate| -> Option<DateTime<Utc>> {
        Local
            .from_local_datetime(&d.and_hms_opt(0, 0, 0)?)
            .single()
            .map(|t| t.with_timezone(&Utc))
    };
    // Start at the first forecast hour we actually have (the live hour for
    // today, midnight for a future day) so the lanes fill from the left edge —
    // no empty sliver before the first cell. End at the shown day's midnight.
    let d0 = hours.first().unwrap().time;
    let last_end = hours.last().unwrap().time + Duration::hours(1);
    let dom_end0 = date
        .succ_opt()
        .and_then(local_midnight)
        .unwrap_or(last_end)
        .max(last_end);
    let dom_end = best_window.map_or(dom_end0, |w| dom_end0.max(w.end));
    let span = (dom_end - d0).num_seconds().max(1) as f64;
    let frac = |t: DateTime<Utc>| (((t - d0).num_seconds()) as f64 / span).clamp(0.0, 1.0);

    let cols: Vec<TlCol> = hours
        .iter()
        .map(|s| {
            let l = frac(s.time);
            let r = frac(s.time + Duration::hours(1));
            TlCol {
                l,
                r,
                mid: (l + r) / 2.0,
                hour: s.time.with_timezone(&Local).hour(),
                low: s.confidence == Confidence::Lav,
                s,
            }
        })
        .collect();
    let n = cols.len();
    // Label density: avoid crowding when there are many columns.
    let label_step = if n > 12 { 3 } else { 2 };

    // ---- value series from raw centre-point conditions ----
    let temps: Vec<(f64, f64)> = cols
        .iter()
        .filter_map(|c| c.s.raw.map(|r| (c.mid, r.temperature_c)))
        .collect();
    let (tmin, tmax) = temps.iter().fold((f64::MAX, f64::MIN), |(lo, hi), (_, t)| {
        (lo.min(*t), hi.max(*t))
    });
    let temp_line = (temps.len() >= 2).then(|| {
        // Extend flat to both plot edges so the line spans the first/last hour
        // cells fully (the per-hour value is constant across its cell) — without
        // this the line starts at the first cell's midpoint, leaving a gap.
        let y = |t: f64| lane_y(0, t, tmin, tmax);
        let mut s = format!("0.0,{:.1}", y(temps[0].1));
        for (x, t) in &temps {
            s.push_str(&format!(" {:.1},{:.1}", x * TL_W, y(*t)));
        }
        s.push_str(&format!(" {:.1},{:.1}", TL_W, y(temps[temps.len() - 1].1)));
        s
    });

    let winds: Vec<(f64, f64, Option<f64>)> = cols
        .iter()
        .filter_map(|c| c.s.raw.map(|r| (c.mid, r.wind_speed_ms, r.wind_gust_ms)))
        .collect();
    // Scale wind against a floor of 10 m/s so a calm day doesn't exaggerate
    // tiny gusts, expanding to fit any real gust above it.
    let wmax = winds
        .iter()
        .map(|(_, w, g)| g.unwrap_or(*w).max(*w))
        .fold(10.0_f64, f64::max);
    let wind_line = (winds.len() >= 2).then(|| {
        let y = |w: f64| lane_y(1, w, 0.0, wmax);
        let mut s = format!("0.0,{:.1}", y(winds[0].1));
        for (x, w, _) in &winds {
            s.push_str(&format!(" {:.1},{:.1}", x * TL_W, y(*w)));
        }
        s.push_str(&format!(" {:.1},{:.1}", TL_W, y(winds[winds.len() - 1].1)));
        s
    });
    let gust_line = {
        let g: Vec<(f64, f64)> = winds
            .iter()
            .filter_map(|(x, _, g)| g.map(|gg| (*x, gg)))
            .collect();
        (g.len() >= 2).then(|| {
            g.iter()
                .map(|(x, gg)| format!("{:.1},{:.1}", x * TL_W, lane_y(1, *gg, 0.0, wmax)))
                .collect::<Vec<_>>()
                .join(" ")
        })
    };

    // Precip lane (index 2): mm bars + a faint probability "ghost" behind them.
    let pmax = cols
        .iter()
        .filter_map(|c| c.s.raw.map(|r| r.precipitation_mm))
        .fold(0.6_f64, f64::max);
    let precip_base = 3.0 * TL_LANE_H; // bottom of lane 2

    // ---- rain bands: merge consecutive wet (≥0.05 mm) hours ----
    let wet: Vec<bool> = cols
        .iter()
        .map(|c| c.s.raw.is_some_and(|r| r.precipitation_mm >= 0.05))
        .collect();
    let mut rain_bands: Vec<(f64, f64)> = Vec::new();
    let mut i = 0;
    while i < n {
        if wet[i] {
            let start = cols[i].l;
            let mut j = i;
            while j + 1 < n && wet[j + 1] {
                j += 1;
            }
            rain_bands.push((start, cols[j].r));
            i = j + 1;
        } else {
            i += 1;
        }
    }

    // Which day does this timeline cover? When today's ride window is already
    // past, `get_hourly` rolls forward to tomorrow, so the heading must say so
    // — and the "now" marker only makes sense when *now* falls inside the
    // shown span (otherwise it would pin uselessly to an edge).
    let today_local = now.with_timezone(&Local).date_naive();
    let day_label = if day.date == today_local {
        "i dag"
    } else if Some(day.date) == today_local.succ_opt() {
        "i morgen"
    } else {
        "" // get_hourly only ever returns today or tomorrow
    };
    let now_in_range = now >= d0 && now < dom_end;
    let now_f = frac(now);
    // Sunrise/sunset fractions across the full-day domain (+ formatted times for
    // the marker tooltips). Both drive the daylight arc and the night shading.
    let sunrise_f = sunrise
        .filter(|s| *s > d0 && *s < dom_end)
        .map(|s| (frac(s), s.with_timezone(&Local).format("%H:%M").to_string()));
    let sunset_f = sunset
        .filter(|s| *s > d0 && *s < dom_end)
        .map(|s| (frac(s), s.with_timezone(&Local).format("%H:%M").to_string()));
    // Daylight illustrated as a *semi-transparent wash over the whole plot*
    // (not a separate lane): night = a dark-blue tint, sunrise/sunset = an
    // orange glow, daytime fully transparent so lanes stay vivid. Built across
    // whatever sun events fall inside the (now→end-of-day) domain, so it works
    // whether `now` is morning, afternoon, or after dark. `None` only when we
    // have no sun info at all (polar edge case).
    let daylight = {
        let day_col = "rgba(0,0,0,0)";
        let night_col = "rgba(29,38,61,0.50)";
        let glow_col = "rgba(254,128,25,0.36)";
        let gp = 3.5_f64; // glow half-width, % of the axis
                          // Transitions inside the domain, in order: sunrise → day, sunset → night.
        let mut events: Vec<(f64, &str)> = Vec::new();
        if let Some((f, _)) = &sunrise_f {
            events.push((*f, day_col));
        }
        if let Some((f, _)) = &sunset_f {
            events.push((*f, night_col));
        }
        events.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let day_at_start = match (sunrise, sunset) {
            (Some(sr), Some(ss)) => d0 >= sr && d0 < ss,
            (Some(sr), None) => d0 >= sr,
            (None, Some(ss)) => d0 < ss,
            (None, None) => true,
        };
        if sunrise.is_none() && sunset.is_none() {
            None
        } else {
            let mut cur = if day_at_start { day_col } else { night_col };
            let mut stops = format!("{cur} 0%");
            for (f, to) in events.iter().copied() {
                let p = (f * 100.0).clamp(0.0, 100.0);
                stops.push_str(&format!(
                    ", {cur} {:.1}%, {glow_col} {:.1}%, {to} {:.1}%",
                    (p - gp).max(0.0),
                    p,
                    (p + gp).min(100.0)
                ));
                cur = to;
            }
            stops.push_str(&format!(", {cur} 100%"));
            Some(format!("background:linear-gradient(to right, {stops})"))
        }
    };
    let bw = best_window.map(|w| {
        let s = w.start.with_timezone(&Local).format("%H").to_string();
        let e = w.end.with_timezone(&Local).format("%H").to_string();
        (frac(w.start), frac(w.end), s, e, w.start > now)
    });

    // SVG x for a 0–1 fraction.
    let sx = |f: f64| f * TL_W;

    // Lane background heat cells for a score axis.
    let heat = |lane: usize, score: fn(&ScoreBreakdown) -> u8, dim: bool| {
        let y = lane as f64 * TL_LANE_H;
        cols.iter()
            .map(|c| {
                let fill = color::hex(score(&c.s.breakdown));
                let op = if dim { "0.5" } else { "0.92" };
                view! {
                    <rect x=n1(sx(c.l)) y=n1(y) width=n1(sx(c.r - c.l)) height=n1(TL_LANE_H)
                        fill=fill opacity=op />
                }
            })
            .collect_view()
    };

    let glyph = "flex flex-1 items-center justify-center";
    view! {
        <div>
            <div class="mb-2 flex items-center gap-2">
                <span class="text-[11px] font-semibold uppercase tracking-[0.12em] text-gruv-fg/45">"Time for time"</span>
                <span class="h-px flex-1 bg-gradient-to-r from-gruv-bg2/70 to-transparent shadow-[0_1px_0_rgba(235,219,178,0.08)]"></span>
                // Day badge — a bold aqua chip when the timeline isn't today, so a
                // tomorrow view can never be mistaken for the current evening.
                {if day_label == "i dag" {
                    view! { <span class="text-xs text-gruv-fg/60">"i dag"</span> }.into_any()
                } else if day_label.is_empty() {
                    ().into_any()
                } else {
                    view! {
                        <span class="inline-flex items-center gap-1 rounded-full bg-gruv-aqua px-2 py-0.5 text-[10px] font-bold uppercase tracking-wide text-gruv-bg0">
                            {icons::arrow_right("h-3 w-3", None)}
                            {day_label}
                        </span>
                    }.into_any()
                }}
            </div>

            <div class="flex gap-x-1.5">
                // ---- lane icon gutter (offset past the hour axis; inner column
                // matches the plot height so icons stay centred on their lanes) ----
                <div class="flex w-5 shrink-0 flex-col pt-[18px] text-gruv-fg/45">
                    <div class="flex h-52 flex-col">
                        <div class=glyph>{icons::thermometer("h-3.5 w-3.5", Some("Temperatur"))}</div>
                        <div class=glyph>{icons::wind("h-3.5 w-3.5", Some("Vind"))}</div>
                        <div class=glyph>{icons::cloud_rain("h-3.5 w-3.5", Some("Nedbør"))}</div>
                        <div class=glyph>{icons::umbrella("h-3.5 w-3.5", Some("Nedbørssjanse"))}</div>
                        <div class=glyph>{icons::mountain("h-3.5 w-3.5", Some("Underlag"))}</div>
                    </div>
                </div>

                <div class="relative min-w-0 flex-1">
                    // ---- hour axis ---- labels sit at each hour's *start* edge
                    // (like ticks), so "19" marks where 19:00 begins and the now
                    // marker reads correctly as sitting between two hours.
                    <div class="relative h-[18px] text-[10px] tabular-nums text-gruv-fg/50">
                        {cols.iter().enumerate().filter(|(i, _)| i % label_step == 0).map(|(_, c)| {
                            view! {
                                <span class="absolute pl-0.5" style=format!("left:{}", pct(c.l))>
                                    {format!("{:02}", c.hour)}
                                </span>
                            }
                        }).collect_view()}
                    </div>

                    // ---- plot ----
                    <div class="relative h-52 overflow-hidden rounded-lg bg-gruv-bg0/40 inset-well">
                        <svg class="gx-sweep absolute inset-0 h-full w-full"
                            viewBox=format!("0 0 {TL_W} {TL_H}") preserveAspectRatio="none">
                            // lane heat backgrounds
                            {heat(0, |b| b.temperature, false)}
                            {heat(1, |b| b.wind, false)}
                            {heat(3, |b| b.precip_probability, false)}
                            {heat(4, |b| b.ground, true)}

                            // precip lane: probability ghost, then mm bars
                            {cols.iter().map(|c| {
                                let prob = c.s.raw.and_then(|r| r.probability_of_precip).unwrap_or(0.0);
                                let gh = (prob / 100.0).clamp(0.0, 1.0) * (TL_LANE_H - TL_PAD);
                                view! {
                                    <rect x=n1(sx(c.l)) y=n1(precip_base - gh) width=n1(sx(c.r - c.l))
                                        height=n1(gh) fill="#83a598" opacity="0.12" />
                                }
                            }).collect_view()}
                            {cols.iter().map(|c| {
                                let mm = c.s.raw.map_or(0.0, |r| r.precipitation_mm);
                                let wet = mm >= 0.05;
                                let bh = if wet { ((mm / pmax) * (TL_LANE_H - TL_PAD)).max(3.0) } else { 1.5 };
                                let w = (sx(c.r - c.l) - 3.0).max(2.0);
                                let op = if wet { "0.95" } else { "0.4" };
                                view! {
                                    <rect x=n1(sx(c.l) + 1.5) y=n1(precip_base - bh) width=n1(w) height=n1(bh)
                                        rx="1" fill="#83a598" opacity=op />
                                }
                            }).collect_view()}

                            // lane separators
                            {(1..TL_LANES).map(|k| {
                                let y = k as f64 * TL_LANE_H;
                                view! { <line x1="0" y1=n1(y) x2=n1(TL_W) y2=n1(y) stroke="#282828" stroke-width="1"/> }
                            }).collect_view()}

                            // faint hour gridlines at each labeled hour's start, so the
                            // axis label and the column line up unambiguously.
                            {cols.iter().enumerate().filter(|(i, _)| i % label_step == 0).map(|(_, c)| {
                                let x = sx(c.l);
                                view! { <line x1=n1(x) y1="0" x2=n1(x) y2=n1(TL_H) stroke="#282828" stroke-width="1" stroke-opacity="0.6"/> }
                            }).collect_view()}

                            // wind gust (dashed) + wind value line
                            {gust_line.map(|p| view! {
                                <polyline points=p fill="none" stroke="#ebdbb2" stroke-width="1.2"
                                    stroke-opacity="0.5" stroke-dasharray="3 3" vector-effect="non-scaling-stroke"/>
                            })}
                            {wind_line.map(|p| view! {
                                <polyline points=p fill="none" stroke="#ebdbb2" stroke-width="1.8"
                                    stroke-linecap="round" stroke-linejoin="round" vector-effect="non-scaling-stroke"/>
                            })}
                            // temperature value line
                            {temp_line.map(|p| view! {
                                <polyline points=p fill="none" stroke="#ebdbb2" stroke-width="1.8"
                                    stroke-linecap="round" stroke-linejoin="round" vector-effect="non-scaling-stroke"/>
                            })}
                        </svg>

                        // ---- HTML overlays (crisp text + effects) ----

                        // daylight wash over the whole plot: night tint + sunrise/
                        // sunset glow, daytime fully transparent (see `daylight`).
                        // Sits above the lanes but below the data markers below.
                        {daylight.map(|bg| view! {
                            <div class="pointer-events-none absolute inset-0" style=bg></div>
                        })}

                        // sunrise / sunset suns at the horizon moments
                        {sunrise_f.as_ref().map(|(f, _)| view! {
                            <div class="pointer-events-none absolute top-1 z-10 -translate-x-1/2 text-gruv-orange"
                                style=format!("left:{}", pct(*f))>
                                {icons::sunset("h-4 w-4", None)}
                            </div>
                        })}
                        {sunset_f.as_ref().map(|(f, _)| view! {
                            <div class="pointer-events-none absolute top-1 z-10 -translate-x-1/2 text-gruv-orange"
                                style=format!("left:{}", pct(*f))>
                                {icons::sunset("h-4 w-4", None)}
                            </div>
                        })}

                        // rain bands (hatched ribbon down through the lanes) + a droplet marker
                        {rain_bands.iter().map(|(a, b)| {
                            let style = format!(
                                "left:{}; width:{}; background-image:repeating-linear-gradient(45deg,rgba(131,165,152,0.30) 0,rgba(131,165,152,0.30) 2px,transparent 2px,transparent 6px)",
                                pct(*a), pct(b - a)
                            );
                            let mid = (a + b) / 2.0;
                            view! {
                                <>
                                    <div class="pointer-events-none absolute inset-y-0" style=style></div>
                                    <div class="pointer-events-none absolute top-0.5 z-10 -translate-x-1/2 text-gruv-blue"
                                        style=format!("left:{}", pct(mid))>
                                        {icons::droplet("h-3 w-3", None)}
                                    </div>
                                </>
                            }
                        }).collect_view()}

                        // low-confidence columns: faint hatch overlay
                        {cols.iter().filter(|c| c.low).map(|c| {
                            let style = format!(
                                "left:{}; width:{}; background-image:repeating-linear-gradient(-45deg,rgba(40,40,40,0.45) 0,rgba(40,40,40,0.45) 2px,transparent 2px,transparent 5px)",
                                pct(c.l), pct(c.r - c.l)
                            );
                            view! { <div class="pointer-events-none absolute inset-y-0" style=style></div> }
                        }).collect_view()}

                        // best-window box
                        {bw.clone().map(|(a, b, _, _, future)| {
                            let glow = if future { "gx-breathe" } else { "" };
                            view! {
                                <div class=format!("pointer-events-none absolute inset-y-0 rounded-md bg-gruv-aqua/10 ring-1 ring-gruv-aqua/70 {glow}")
                                    style=format!("left:{}; width:{}", pct(a), pct(b - a))></div>
                            }
                        })}

                        // now marker — only when "now" actually falls inside the
                        // shown span (a past/future day has no meaningful marker)
                        {now_in_range.then(|| {
                            let now_hhmm = now.with_timezone(&Local).format("%H:%M").to_string();
                            view! {
                                <div class="pointer-events-none absolute inset-y-0 z-20"
                                    style=format!("left:{}", pct(now_f))>
                                    <div class="absolute inset-y-0 -translate-x-1/2 w-[2px] bg-gruv-fg shadow-[0_0_4px_rgba(235,219,178,0.7)]"></div>
                                    <div class="absolute top-0 -translate-x-1/2 h-1.5 w-1.5 rounded-full bg-gruv-fg"></div>
                                    <div class="absolute bottom-0.5 left-1 rounded bg-gruv-fg px-1 text-[9px] font-bold leading-tight text-gruv-bg0">
                                        {format!("nå {now_hhmm}")}
                                    </div>
                                </div>
                            }
                        })}

                        // unit labels (°C near temp lane, m/s near wind lane)
                        {cols.iter().enumerate().filter(|(i, _)| i % label_step == 0).map(|(_, c)| {
                            match c.s.raw {
                                Some(r) => view! {
                                    <>
                                        <span class="pointer-events-none absolute -translate-x-1/2 text-[9px] font-semibold tabular-nums text-gruv-fg/85"
                                            style=format!("left:{}; top:2px", pct(c.mid))>{format!("{:.0}°", r.temperature_c)}</span>
                                        <span class="pointer-events-none absolute -translate-x-1/2 text-[9px] font-semibold tabular-nums text-gruv-fg/80"
                                            style=format!("left:{}; top:44px", pct(c.mid))>{format!("{:.0}", r.wind_speed_ms)}</span>
                                    </>
                                }.into_any(),
                                None => ().into_any(),
                            }
                        }).collect_view()}
                    </div>

                    // ---- best-window tab (below plot, anchored to the box) ----
                    {bw.map(|(a, b, s, e, future)| {
                        let tag = if future { format!("beste vindu {s}–{e}") } else { format!("beste vindu {s}–{e} (nå)") };
                        view! {
                            <div class="relative mt-1 h-4 text-[10px] font-semibold text-gruv-aqua">
                                <span class="absolute inline-flex -translate-x-1/2 items-center gap-1 whitespace-nowrap" style=format!("left:{}", pct((a + b) / 2.0))>
                                    {icons::bike("h-3 w-3", None)}
                                    {tag}
                                </span>
                            </div>
                        }
                    })}
                </div>
            </div>

            // ---- legend (doubles as the lane key) ----
            <div class="mt-2 flex flex-wrap items-center gap-x-3 gap-y-1 text-[10px] text-gruv-fg/55">
                <span class="flex items-center gap-1"><span class="inline-block h-2 w-0.5 bg-gruv-fg/90"></span>"nå"</span>
                <span class="flex items-center gap-1 text-gruv-aqua/90">{icons::bike("h-3 w-3", None)}"beste vindu"</span>
                <span class="flex items-center gap-1 text-gruv-blue">{icons::droplet("h-3 w-3", None)}"regn"</span>
                <span class="flex items-center gap-1 text-gruv-orange/80">{icons::sunset("h-3 w-3", None)}"solnedgang"</span>
                <span class="mx-1 h-3 w-px bg-gruv-bg2"></span>
                <span class="flex items-center gap-1">{icons::thermometer("h-3 w-3", None)}"temp"</span>
                <span class="flex items-center gap-1">{icons::wind("h-3 w-3", None)}"vind"</span>
                <span class="flex items-center gap-1">{icons::cloud_rain("h-3 w-3", None)}"nedbør"</span>
                <span class="flex items-center gap-1">{icons::umbrella("h-3 w-3", None)}"sjanse"</span>
                <span class="flex items-center gap-1">{icons::mountain("h-3 w-3", None)}"bakke"</span>
            </div>
        </div>
    }
    .into_any()
}
