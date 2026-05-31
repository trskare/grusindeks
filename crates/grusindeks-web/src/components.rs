//! Presentational components for the dashboard. Each takes owned data (from an
//! awaited server-fn response) and renders with the shared score palette
//! ([`crate::color`]). Animations are plain CSS transitions.

use chrono::Local;
use leptos::prelude::*;

use grusindeks_core::aggregate::{DayAggregate, NowcastAlert};
use grusindeks_core::daily::Confidence;
use grusindeks_core::score::{Penalty, ScoreBreakdown, Severity};

use crate::color;
use crate::dto::HistoryPoint;

/// Radial score gauge: a ring filled to `total`% in the score colour, with the
/// number and a verdict label in the middle.
#[component]
pub fn ScoreGauge(total: u8, label: String) -> impl IntoView {
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
                <div class=format!("text-4xl font-extrabold tabular-nums {}", color::text_class(total))>
                    {total}
                </div>
                <div class="text-xs text-gruv-gray">{label}</div>
            </div>
        </div>
    }
}

/// Five per-axis sub-score bars from a [`ScoreBreakdown`].
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
        <div class="space-y-2">
            {rows.into_iter().map(|(name, val)| {
                let bar = color::bg_class(val);
                view! {
                    <div class="flex items-center gap-3 text-sm">
                        <span class="w-16 text-gruv-gray">{name}</span>
                        <div class="h-2 flex-1 overflow-hidden rounded-full bg-gruv-bg2">
                            <div
                                class=format!("h-full rounded-full {bar}")
                                style=format!("width:{val}%; transition: width 700ms cubic-bezier(0.22,1,0.36,1);")
                            ></div>
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
    view! {
        <svg width="100%" viewBox=format!("0 0 {w} {h}") preserveAspectRatio="none" class="w-full">
            <polyline
                points=poly fill="none" stroke=stroke stroke-width="2"
                stroke-linecap="round" stroke-linejoin="round"
            />
            <circle cx=format!("{lx:.1}") cy=format!("{ly:.1}") r="3" fill=stroke/>
        </svg>
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

/// One day in the multi-day strip: weather icon, coloured mean, spread, the
/// best sub-window, and a confidence note. Long-range low-confidence days are
/// dimmed.
#[component]
pub fn DayCard(day: DayAggregate) -> impl IntoView {
    let mean = day.mean;
    let date = day.date.format("%a %d.%m").to_string();
    let icon = day.center().weather_icon.clone();
    let dim = if day.confidence == Confidence::Lav {
        "opacity-60"
    } else {
        ""
    };
    let best = day.optimal_window.as_ref().map(|ow| {
        let s = ow.window.start.with_timezone(&Local).format("%H:%M");
        let e = ow.window.end.with_timezone(&Local).format("%H:%M");
        format!("beste {s}–{e} (+{})", ow.improvement)
    });

    view! {
        <div class=format!("min-w-[8.5rem] flex-1 rounded-xl bg-gruv-bg1 p-4 shadow transition hover:-translate-y-0.5 {dim}")>
            <div class="flex items-center justify-between">
                <span class="text-sm text-gruv-gray">{date}</span>
                <span class="text-lg">{icon}</span>
            </div>
            <div class=format!("mt-2 text-3xl font-bold tabular-nums {}", color::text_class(mean))>
                {mean}
            </div>
            <div class="text-xs text-gruv-gray">{format!("{}–{}", day.min, day.max)}</div>
            {best.map(|b| view! { <div class="mt-1 text-xs text-gruv-aqua">{b}</div> })}
            <div class="mt-1 text-[10px] text-gruv-gray">{confidence_label(day.confidence)}</div>
        </div>
    }
}
