//! Human-readable formatting of an `AggregateScore` / `MultiDayForecast`.
//!
//! Pure functions that return `String`s — easy to insta-snapshot, easy to
//! reason about. All colour goes through `theme::*` helpers, which silently
//! degrade to plain text when stdout isn't a TTY (covers tests, pipes, and
//! `NO_COLOR`).

use std::fmt::Write as _;

use chrono::{DateTime, Datelike, Duration as ChronoDuration, Local, NaiveDate, Timelike, Utc};
use grusindeks_core::daily::Confidence;
use grusindeks_core::lang::Language;
use grusindeks_core::score::{Penalty, Severity, WindowStats};
use grusindeks_core::types::RideWindow;
use unicode_width::UnicodeWidthStr;

use crate::aggregate::{
    AggregateScore, DayAggregate, HourlyForecast, MultiDayForecast, NowcastAlert, RainHistory,
};
use crate::theme;

/// Per-render toggles for the optional footer chips. Defaults to "show
/// both" so call sites that don't yet pass flags get the historic
/// behaviour. The CLI layer narrows these from config + flag overrides.
#[derive(Debug, Clone, Copy)]
pub struct ChipFlags {
    pub rain_history: bool,
    pub window_stats: bool,
}

impl Default for ChipFlags {
    fn default() -> Self {
        Self {
            rain_history: true,
            window_stats: true,
        }
    }
}

/// Bar width for the per-day row. We render 9 full cells max + a 10th
/// half-block sub-cell so values like 95 and 100 are visually distinct
/// (the old 10-cell bar with `(value*10+50)/100` rounding collapsed
/// 95–100 to identical glyphs).
const BAR_FULL_CELLS: usize = 9;

/// Indent before the tree-prefix on breakdown / penalty / "Beste luke"
/// rows. Chosen so the *bar* on a breakdown row lines up directly under
/// the bar on the parent day row — readers can sanity-check "the day's
/// mean is the average of the four sub-scores" by tracing a vertical
/// line.
///
/// Math: the parent row puts its bar at column 19 (2 indent + 11
/// day-label + 2 + 3 icon-area + 1 = 19). The breakdown row layout is
/// `<indent>├─ <label-padded-9> <bar>`, so indent + 2 + 1 + 9 + 1 = 19,
/// giving indent = 6.
const BREAKDOWN_INDENT: &str = "      ";

/// Bar with the filled portion painted in the score colour and the empty
/// portion dimmed. Returns the styled string ready to drop into a row.
///
/// The bar is 10 cells wide visually: up to 9 full blocks plus one
/// trailing sub-cell drawn from the half-block ramp `▏▎▍▌▋▊▉█`. That
/// gives us 9 × 8 + 1 = 73 levels in the same width as the old 10-cell
/// full-block bar.
fn colored_bar(value: u8) -> String {
    // Total sub-cells available: 9 × 8 = 72.
    let total_sub = BAR_FULL_CELLS * 8;
    let sub = (usize::from(value) * total_sub + 50) / 100;
    let sub = sub.min(total_sub);
    let full = sub / 8;
    let rem = sub % 8;
    let trailing = match rem {
        0 => None,
        1 => Some("▏"),
        2 => Some("▎"),
        3 => Some("▍"),
        4 => Some("▌"),
        5 => Some("▋"),
        6 => Some("▊"),
        7 => Some("▉"),
        _ => unreachable!(),
    };

    // At the top of the scale, paint a 10th full block so score 100
    // never reads as "9 cells + visual gap".
    if full == BAR_FULL_CELLS {
        return theme::paint_bar_filled(&"█".repeat(BAR_FULL_CELLS + 1), value);
    }

    let filled = "█".repeat(full);
    let empty_cells = BAR_FULL_CELLS + 1 - full - if trailing.is_some() { 1 } else { 0 };
    // `▒` (U+2592, medium shade, ~50% density) instead of `░` (light
    // shade, ~25%): matches the visual weight of the half-block
    // partial cell's gray background. With `░` the empty section read
    // as a noticeably *lighter* grey than the partial cell's filler,
    // creating an unintended visual seam mid-bar.
    let empty: String = "▒".repeat(empty_cells);

    // The trailing half-block cell needs a *background* colour, not just
    // a foreground — the right portion of `▏▎▍▌▋▊▉` is the cell
    // background, which would otherwise be the terminal background
    // (showing as a black gap between the filled run and the empty run).
    // Painting bg=gray makes the bar read as continuous.
    let trailing_painted = trailing
        .map(|t| theme::paint_bar_partial(t, value))
        .unwrap_or_default();

    format!(
        "{}{}{}",
        theme::paint_bar_filled(&filled, value),
        trailing_painted,
        theme::paint_bar_empty(&empty),
    )
}

/// 8-level sparkline glyph for `▁▂▃▄▅▆▇█`. Maps a 0–100 score to the
/// closest block. Used in the week-trend line above the day rows.
fn sparkline_glyph(value: u8) -> char {
    let levels = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let idx = (usize::from(value) * (levels.len() - 1) + 50) / 100;
    levels[idx.min(levels.len() - 1)]
}

/// Slope arrow over a sequence of daily scores. Uses the sign of the
/// difference between the first and last day; we don't need a real
/// linear regression for a 6-day view.
fn trend_arrow(values: &[u8]) -> char {
    if values.len() < 2 {
        return '→';
    }
    let first = i32::from(values[0]);
    let last = i32::from(*values.last().unwrap());
    match last - first {
        d if d >= 5 => '↗',
        d if d <= -5 => '↘',
        _ => '→',
    }
}

fn local_hm(t: DateTime<Utc>) -> String {
    t.with_timezone(&Local).format("%H:%M").to_string()
}

fn local_date(t: DateTime<Utc>) -> String {
    t.with_timezone(&Local).format("%Y-%m-%d").to_string()
}

/// Single-line banner that surfaces the radar nowcast above the regular
/// report. Only writes anything when `alert` is `Some` — call sites pass
/// the option-shaped field straight through.
///
/// Tone (`Severity::Critical` red vs. `Severity::Minor` yellow) follows
/// the same red/yellow split the penalty list uses, so the user
/// recognises the visual weight at a glance:
/// - **Critical** when the radar sees a steady downpour (`peak_mm_h ≥ 0.5`
///   *and* the rain lasts ≥ 30 min) or rain is imminent (≤ 30 min away).
/// - **Minor** otherwise — light, brief, or far enough out (~60–120 min)
///   that the extrapolation has slack.
fn write_nowcast_banner(out: &mut String, alert: Option<&NowcastAlert>, lang: Language) {
    let Some(a) = alert else {
        return;
    };
    let now = Utc::now();
    let until_first = a.first_rain_at - now;
    let duration = a.last_rain_at - a.first_rain_at;
    let imminent = until_first <= ChronoDuration::minutes(30);
    let heavy = a.peak_mm_h >= 0.5 && duration >= ChronoDuration::minutes(30);
    let severity = if imminent || heavy {
        Severity::Critical
    } else {
        Severity::Minor
    };

    let mut parts = String::new();
    let _ = write!(
        parts,
        "{}  {}: ",
        theme::paint_severity("⚠", severity),
        theme::paint_severity(radar_label(lang), severity),
    );
    let _ = write!(
        parts,
        "{} {} ",
        rain_word(lang),
        format_time_until(until_first, lang),
    );
    let peak_label = match lang {
        Language::Norwegian => "topp",
        Language::Swedish => "topp",
    };
    let _ = write!(
        parts,
        "({peak_label} {:.1} mm/h kl {})",
        a.peak_mm_h,
        local_hm(a.peak_at),
    );
    let _ = writeln!(out, "{parts}");
    let _ = writeln!(out);
}

fn radar_label(lang: Language) -> &'static str {
    match lang {
        Language::Norwegian => "Radar",
        Language::Swedish => "Radar",
    }
}

fn rain_word(lang: Language) -> &'static str {
    match lang {
        Language::Norwegian => "regn",
        Language::Swedish => "regn",
    }
}

/// Format `delta` as "om N min" / "om Xt Ym" / "nå". Negative deltas
/// shouldn't reach this path (`build_nowcast_alert` filters them), but
/// we render them as "nå" defensively rather than as "om -3 min".
fn format_time_until(delta: ChronoDuration, lang: Language) -> String {
    let total_min = delta.num_minutes();
    if total_min <= 0 {
        return match lang {
            Language::Norwegian => "nå".into(),
            Language::Swedish => "nu".into(),
        };
    }
    let prefix = match lang {
        Language::Norwegian => "om",
        Language::Swedish => "om",
    };
    if total_min < 60 {
        return format!("{prefix} {total_min} min");
    }
    let hours = total_min / 60;
    let mins = total_min % 60;
    if mins == 0 {
        format!("{prefix} {hours}t")
    } else {
        format!("{prefix} {hours}t {mins} min")
    }
}

/// Render the aggregate score for a single ride window. Penalties (when
/// present) are listed under the bar block so the user immediately sees
/// what dragged the score down. `verbose` lifts the cap on penalty count
/// and surfaces every per-axis sub-score number.
pub fn render_human(
    label: &str,
    radius_km: f64,
    window: RideWindow,
    agg: &AggregateScore,
    verbose: bool,
    flags: ChipFlags,
    lang: Language,
) -> String {
    let mut out = String::new();
    let header = format!(
        "Grusindeks for {label} ({radius_km:.0}km radius) — {} {}–{}",
        local_date(window.start),
        local_hm(window.start),
        local_hm(window.end),
    );
    let _ = writeln!(out, "{}", theme::paint_accent(&header));
    let _ = writeln!(out, "{}", theme::paint_dim(&"═".repeat(63)));
    write_nowcast_banner(&mut out, agg.nowcast_alert.as_ref(), lang);

    // Headline total + label, both painted with the score colour.
    let total_word = match lang {
        Language::Norwegian => "Total",
        Language::Swedish => "Totalt",
    };
    let _ = writeln!(
        out,
        "{total_word}: {}/100  ⭐ {}",
        theme::paint_score(agg.mean),
        theme::paint_label(mean_label(agg.mean, lang), agg.mean),
    );
    let _ = writeln!(out);

    // Per-axis bars (means across points). Same five-axis layout as before
    // — the new piece is colour, plus penalty lines below.
    let agg_temp = avg_axis(agg, |b| b.temperature);
    let agg_wind = avg_axis(agg, |b| b.wind);
    let agg_precip = avg_axis(agg, |b| b.precipitation);
    let agg_prob = avg_axis(agg, |b| b.precip_probability);
    let agg_ground = avg_axis(agg, |b| b.ground);

    write_axis_row(&mut out, axis_label_long("temperature", lang), agg_temp, "");
    write_axis_row(&mut out, axis_label_long("wind", lang), agg_wind, "");
    let precip_combined = combined_precip(agg_precip, agg_prob, lang);
    write_axis_row(
        &mut out,
        axis_label_long("precipitation", lang),
        precip_combined,
        "",
    );
    write_axis_row(&mut out, axis_label_long("ground", lang), agg_ground, "");

    // Penalty list — pulled from the center point. Default caps at the
    // top three so the output stays scannable; `--verbose` lists all.
    let penalties = center_penalties(agg);
    if !penalties.is_empty() {
        let _ = writeln!(out);
        let cap = if verbose { penalties.len() } else { 3 };
        for p in penalties.iter().take(cap) {
            let _ = writeln!(out, "  {}", format_penalty_line(p, lang));
        }
    }

    // Skip the worst/best punkt-rows entirely when every sample tied —
    // showing "Verste 89 (SØ)" and "Beste 89 (NV)" with the same number
    // suggests the bearings *matter* when in fact they're an artefact of
    // tie-breaking on the first/last enumeration. Only render when there's
    // a real spread.
    if agg.points.len() > 1 && agg.min < agg.max {
        let _ = writeln!(out);
        let worst = agg.worst();
        let best = agg.best();
        let (worst_word, best_word) = match lang {
            Language::Norwegian => ("Verste punkt:", "Beste punkt: "),
            Language::Swedish => ("Sämsta punkt:", "Bästa punkt: "),
        };
        let _ = writeln!(
            out,
            "{worst_word}  {} ({})",
            theme::paint_score(worst.score.total),
            worst.bearing_label,
        );
        let _ = writeln!(
            out,
            "{best_word}  {} ({})",
            theme::paint_score(best.score.total),
            best.bearing_label,
        );
    }
    write_aggregate_chips(&mut out, agg, flags, lang);
    out
}

/// Footer chips for the single-window report. Same look-and-feel as the
/// multi-day footer, sourced from the centre point's `WindowStats` and
/// the report's `RainHistory`.
fn write_aggregate_chips(out: &mut String, agg: &AggregateScore, flags: ChipFlags, lang: Language) {
    let mut wrote_any = false;
    if flags.rain_history {
        if let Some(msg) = agg
            .rain_history
            .as_ref()
            .and_then(|h| rain_history_chip_content(h, lang))
        {
            if !wrote_any {
                let _ = writeln!(out);
                wrote_any = true;
            }
            let _ = writeln!(
                out,
                "  {}   {msg}",
                theme::paint_dim(rain_history_chip_label(lang)),
            );
        }
    }
    if flags.window_stats {
        let center_stats = agg
            .points
            .iter()
            .find(|p| p.is_center)
            .or_else(|| agg.points.first())
            .map(|p| &p.score.stats);
        if let Some(msg) = center_stats.and_then(|s| window_stats_chip_content(s, lang)) {
            if !wrote_any {
                let _ = writeln!(out);
            }
            let _ = writeln!(
                out,
                "  {}   {msg}",
                theme::paint_dim(window_stats_chip_label(lang)),
            );
        }
    }
}

/// Render the multi-day forecast.
///
/// Layout (default mode):
/// 1. plain title line — "Grusindeks · {label} · {radius} km · {N} dager"
/// 2. callout box — "★ Beste: {date} — {score}  {bucket}"
/// 3. week-trend line — sparkline + first→last + slope arrow + spread
/// 4. one row per day — day label, weather icon, half-block bar, score,
///    plus a `~` marker for low-confidence days
/// 5. footer chips — Bakke (when known), Skala, low-confidence note
///
/// `--verbose` adds the per-axis breakdown tree under every day and
/// extends the per-day penalty list to all penalties (not just the
/// worst). The "Beste luke" callout under each day also moves into
/// verbose-only territory to keep the default scan tight.
pub fn render_multi_day(
    label: &str,
    radius_km: f64,
    forecast: &MultiDayForecast,
    verbose: bool,
    flags: ChipFlags,
    lang: Language,
) -> String {
    let mut out = String::new();
    let n = forecast.days.len();
    let day_word = match (lang, n) {
        (Language::Norwegian, 1) => "dag",
        (Language::Norwegian, _) => "dager",
        (Language::Swedish, 1) => "dag",
        (Language::Swedish, _) => "dagar",
    };
    let title = format!("Grusindeks · {label} · {radius_km:.0} km · {n} {day_word}");
    let _ = writeln!(out, "{}", theme::paint_accent(&title));
    let _ = writeln!(out);
    write_nowcast_banner(&mut out, forecast.nowcast_alert.as_ref(), lang);

    let today_local: NaiveDate = Local::now().date_naive();

    // Callout: the best day in the forecast. The box itself takes its
    // colour from the score bucket, so the eye reads "this week is in
    // X-territory" before reading any text.
    if let Some(best) = pick_best_day(&forecast.days) {
        write_best_callout(&mut out, best, today_local, lang);
        let _ = writeln!(out);
    }

    // Week-trend line: sparkline over the daily scores, plus first→last
    // numerics and a slope arrow. This is the chart the per-day bars
    // can't be — it shows the *shape* of the week.
    write_week_trend(&mut out, &forecast.days, lang);
    let _ = writeln!(out);

    let mut any_low_confidence = false;
    for day in &forecast.days {
        if day.confidence == Confidence::Lav {
            any_low_confidence = true;
        }
        write_day_row(&mut out, day, today_local, verbose, flags, lang);
    }

    let _ = writeln!(out);
    write_footer(&mut out, forecast, verbose, any_low_confidence, flags, lang);
    out
}

/// Round-cornered callout drawn at the top of the multi-day report,
/// pointing at the single best day. The border colour follows the
/// score bucket so the user gets the verdict from glance alone.
fn write_best_callout(
    out: &mut String,
    best: &DayAggregate,
    today_local: NaiveDate,
    lang: Language,
) {
    let best_phrase = match lang {
        Language::Norwegian => "Beste",
        Language::Swedish => "Bästa",
    };
    let conf_suffix = match (lang, best.confidence) {
        (_, Confidence::Hoy) => "",
        (Language::Norwegian, Confidence::Middels) => "  (middels konfidens)",
        (Language::Norwegian, Confidence::Lav) => "  (lav konfidens)",
        (Language::Swedish, Confidence::Middels) => "  (medel tillförlitlighet)",
        (Language::Swedish, Confidence::Lav) => "  (låg tillförlitlighet)",
    };
    let body = format!(
        "  ★ {best_phrase}: {}  —  {}  {}{}  ",
        day_label(best.date, today_local, lang),
        best.mean,
        mean_label(best.mean, lang),
        conf_suffix,
    );
    let body_width = UnicodeWidthStr::width(body.as_str());
    let bar = "─".repeat(body_width);
    let top = format!("╭{bar}╮");
    let mid = format!("│{body}│");
    let bot = format!("╰{bar}╯");
    let _ = writeln!(out, "{}", theme::paint_score_soft(&top, best.mean));
    // Keep the body content uncoloured so the score number stays
    // readable on dark backgrounds; only the border carries the bucket
    // signal.
    let star_painted = mid.replacen('★', &theme::paint_score_soft("★", best.mean), 1);
    let _ = writeln!(out, "{star_painted}");
    let _ = writeln!(out, "{}", theme::paint_score_soft(&bot, best.mean));
}

/// Week-trend line: per-day sparkline coloured by each day's bucket,
/// followed by the first→last score numerics and a coarse slope arrow.
/// Shows the *shape* of the week — exactly the information that gets
/// flattened when the per-day bars are all in the same bucket.
fn write_week_trend(out: &mut String, days: &[DayAggregate], lang: Language) {
    if days.is_empty() {
        return;
    }
    let scores: Vec<u8> = days.iter().map(|d| d.mean).collect();
    let mut spark = String::new();
    for d in days {
        let glyph = sparkline_glyph(d.mean).to_string();
        spark.push_str(&theme::paint_bar_filled(&glyph, d.mean));
    }
    let first = scores.first().copied().unwrap_or(0);
    let last = scores.last().copied().unwrap_or(0);
    let arrow = trend_arrow(&scores);
    let min = scores.iter().min().copied().unwrap_or(0);
    let max = scores.iter().max().copied().unwrap_or(0);
    let spread = max.saturating_sub(min);
    let week_word = match lang {
        Language::Norwegian => "Uke",
        Language::Swedish => "Vecka",
    };
    let spread_word = match lang {
        Language::Norwegian => "spredning",
        Language::Swedish => "spridning",
    };
    let _ = writeln!(
        out,
        "  {week_word}   {spark}   {first} → {last}   {arrow}   {spread_word} {spread} p"
    );
}

/// Trailing chips: Bakke-state (when present), Skala bucket legend, and
/// a one-line note when any day is low-confidence. Everything that used
/// to repeat per row lives here once.
fn write_footer(
    out: &mut String,
    forecast: &MultiDayForecast,
    _verbose: bool,
    any_low_confidence: bool,
    flags: ChipFlags,
    lang: Language,
) {
    if let Some(msg) = ground_chip(forecast) {
        let label = match lang {
            Language::Norwegian => "Bakke",
            Language::Swedish => "Mark",
        };
        let _ = writeln!(out, "  {}   {msg}", theme::paint_dim(label));
    }

    if flags.rain_history {
        if let Some(msg) = forecast
            .rain_history
            .as_ref()
            .and_then(|h| rain_history_chip_content(h, lang))
        {
            let _ = writeln!(
                out,
                "  {}   {msg}",
                theme::paint_dim(rain_history_chip_label(lang))
            );
        }
    }
    // `Tall` (window_stats) used to live here as a single forecast-wide
    // footer chip. It now renders per day under each day's breakdown row
    // — see `write_day_breakdown` — so the user sees today's tall
    // alongside today's bars, and tomorrow's alongside tomorrow's. The
    // ChipFlags toggle still controls visibility, just at the day level.

    let scale_label = match lang {
        Language::Norwegian => "Skala",
        Language::Swedish => "Skala",
    };
    let _ = writeln!(
        out,
        "  {}   {}",
        theme::paint_dim(scale_label),
        bucket_legend(lang)
    );

    if any_low_confidence {
        let note = match lang {
            Language::Norwegian => "~       lav konfidens (>60 t — 6-t oppløsning fra MET)",
            Language::Swedish => "~       låg tillförlitlighet (>60 h — 6-h upplösning från MET)",
        };
        let _ = writeln!(out, "  {}", theme::paint_dim(note));
    }
}

fn rain_history_chip_label(lang: Language) -> &'static str {
    match lang {
        Language::Norwegian => "Regn 7d",
        Language::Swedish => "Regn 7d",
    }
}

fn window_stats_chip_label(lang: Language) -> &'static str {
    match lang {
        Language::Norwegian => "Tall",
        Language::Swedish => "Tal",
    }
}

/// Compose the "Regn 7d" chip body. Returns `None` when the period had
/// no regndøgn — the Bakke chip's `(N døgn uten regn)` parenthetical
/// already conveys "tørt" with the same number, so a dry-week Regn 7d
/// row is pure redundancy. Rendering it as "tørt siste 7 døgn" right
/// under "tørt og løst dekke (7 døgn uten regn)" gave the user nothing
/// they didn't already see.
fn rain_history_chip_content(history: &RainHistory, lang: Language) -> Option<String> {
    if history.rain_days == 0 {
        return None;
    }
    let lookback_days = (history.lookback_hours / 24).max(1);
    let date = history.wettest_day;
    let date_str = format!("{}. {}", date.day(), month(date.month(), lang));
    let total = history.total_mm;
    let wettest = history.wettest_day_mm;
    let days = history.rain_days;
    Some(match lang {
        Language::Norwegian => format!(
            "{total:.1} mm siste {lookback_days} døgn · våtest {date_str} ({wettest:.1} mm) · {days} regndøgn",
        ),
        Language::Swedish => {
            let dag_word = if days == 1 { "regndag" } else { "regndagar" };
            format!(
                "{total:.1} mm senaste {lookback_days} dygn · blötast {date_str} ({wettest:.1} mm) · {days} {dag_word}",
            )
        }
    })
}

/// Rain status appended to the "Beste luke" / "Beste vindu" line.
///
/// The day row can already show a rain icon for the whole day; this tiny
/// trailer answers the more actionable question: does the suggested
/// sub-window itself look dry, or is there forecast rain inside it?
fn best_window_rain_trailer(
    stats: &WindowStats,
    lang: Language,
    is_opening: bool,
) -> Option<String> {
    if stats.is_empty() {
        return None;
    }

    let place = match (lang, is_opening) {
        (Language::Norwegian, true) => "i luka",
        (Language::Norwegian, false) => "i vinduet",
        (Language::Swedish, true) => "i luckan",
        (Language::Swedish, false) => "i fönstret",
    };
    let total = stats.total_precip_mm.max(0.0);
    let peak = stats.max_hourly_precip_mm.max(0.0);
    let text = match (lang, total, peak) {
        (Language::Norwegian, t, p) if t < 0.05 && p < 0.05 => format!("☂ opphold {place}"),
        (Language::Swedish, t, p) if t < 0.05 && p < 0.05 => format!("☂ uppehåll {place}"),
        (Language::Norwegian, t, p) if t < 0.5 && p < 0.5 => format!("🌦 yr {t:.1} mm {place}"),
        (Language::Swedish, t, p) if t < 0.5 && p < 0.5 => {
            format!("🌦 duggregn {t:.1} mm {place}")
        }
        (Language::Norwegian, _, p) if p >= 3.0 => {
            format!("⛈ kraftig regn {place}, topp {p:.1} mm/t")
        }
        (Language::Swedish, _, p) if p >= 3.0 => {
            format!("⛈ kraftigt regn {place}, topp {p:.1} mm/h")
        }
        (Language::Norwegian, t, _) => format!("🌧 regn {t:.1} mm {place}"),
        (Language::Swedish, t, _) => format!("🌧 regn {t:.1} mm {place}"),
    };
    Some(text)
}

/// Compact, colour-coded one-liner for the "Tall" per-day stats row:
/// `12–14 °C · 0.4 mm · 5 m/s (kast 8)`. Each number is painted by
/// mapping the raw value through the relevant axis sub-score so harsh
/// values render red and benign ones render green — the same palette
/// the breakdown bars use, applied to bare numbers.
///
/// The temperature segment uses `apparent_temp` on the day's mean so the
/// colour reflects the *felt* axis (wind chill / heat index), matching
/// the Temperatur sub-score the breakdown bar already shows.
fn format_tall_line(stats: &WindowStats, lang: Language) -> String {
    use grusindeks_core::felt_temp::apparent_temp;
    use grusindeks_core::score::{precip_subscore, temp_subscore, wind_subscore};

    // Temperature: colour by the *felt* sub-score over the window's mean
    // conditions, since that's what the breakdown bar reflects. We don't
    // have a per-min/per-max felt-T, so the range itself is plain text and
    // only the colour signals comfort.
    let lo = stats.min_temp_c.round() as i32;
    let hi = stats.max_temp_c.round() as i32;
    let temp_text = if lo == hi {
        format!("{lo} °C")
    } else {
        format!("{lo}–{hi} °C")
    };
    let felt = apparent_temp(
        stats.mean_temp_c,
        stats.max_wind_ms,
        stats.mean_humidity_pct,
    );
    let temp_score = temp_subscore(felt);
    let temp_seg = theme::paint_score_str(&temp_text, temp_score);

    // Precipitation: colour by the precip sub-score evaluated on the
    // peak-hour rate (matches the hard-cap behaviour better than total mm
    // — a 5 mm/h hour matters even if the rest of the day was dry).
    let precip_total = stats.total_precip_mm.max(0.0);
    let precip_text = match lang {
        Language::Norwegian => format!("{precip_total:.1} mm"),
        Language::Swedish => format!("{precip_total:.1} mm"),
    };
    let precip_score = precip_subscore(stats.max_hourly_precip_mm.max(0.0));
    let precip_seg = theme::paint_score_str(&precip_text, precip_score);

    // Wind: colour by the wind sub-score on max + gust (worst case in the
    // window). The (kast N) parenthetical inherits the same colour so the
    // pair reads as one unit.
    let wind_r = stats.max_wind_ms.max(0.0).round() as i32;
    let wind_score = wind_subscore(stats.max_wind_ms.max(0.0), stats.max_gust_ms);
    let wind_text = match (stats.max_gust_ms, lang) {
        (Some(g), Language::Norwegian) if g.is_finite() => {
            format!("{wind_r} m/s (kast {})", g.round() as i32)
        }
        (Some(g), Language::Swedish) if g.is_finite() => {
            format!("{wind_r} m/s (by {})", g.round() as i32)
        }
        (_, _) => format!("{wind_r} m/s"),
    };
    let wind_seg = theme::paint_score_str(&wind_text, wind_score);

    let sep = theme::paint_dim("·");
    format!("{temp_seg} {sep} {precip_seg} {sep} {wind_seg}")
}

/// Compose the "Tall" chip body for one window's WindowStats. Returns
/// `None` for the empty-window placeholder (NaN min/max), where there's
/// no meaningful number to print and the existing "Ingen data" penalty
/// already covers the user.
fn window_stats_chip_content(stats: &WindowStats, lang: Language) -> Option<String> {
    if stats.is_empty() {
        return None;
    }
    let lo = stats.min_temp_c.round() as i32;
    let hi = stats.max_temp_c.round() as i32;
    let temp_seg = if lo == hi {
        format!("{lo} °C")
    } else {
        format!("{lo}–{hi} °C")
    };
    let precip_seg = match lang {
        Language::Norwegian => format!("nedbør {:.1} mm", stats.total_precip_mm.max(0.0)),
        Language::Swedish => format!("nederbörd {:.1} mm", stats.total_precip_mm.max(0.0)),
    };
    let wind_r = stats.max_wind_ms.max(0.0).round() as i32;
    let wind_seg = match (stats.max_gust_ms, lang) {
        (Some(g), Language::Norwegian) if g.is_finite() => {
            format!("vind {wind_r} m/s (kast {})", g.round() as i32)
        }
        (Some(g), Language::Swedish) if g.is_finite() => {
            format!("vind {wind_r} m/s (by {})", g.round() as i32)
        }
        (_, _) => format!("vind {wind_r} m/s"),
    };
    Some(format!("{temp_seg} · {precip_seg} · {wind_seg}"))
}

/// Ground-state chip text. The score layer emits a `tørt og løst dekke
/// (N døgn uten regn)` penalty when the surface is dry; we surface that
/// in the footer so the message lands once instead of under every day.
/// Returns the *content* (no label prefix) so the caller can paint the
/// label separately.
fn ground_chip(forecast: &MultiDayForecast) -> Option<String> {
    // Look for a Ground penalty on the first day's center point — the
    // surface state is shared across the whole forecast, so any day will
    // do, and the first one is always present.
    let first = forecast.days.first()?;
    let center = first.center();
    let penalty = center
        .score
        .penalties
        .iter()
        .find(|p| p.component == grusindeks_core::score::Component::Ground)?;
    Some(penalty.message.clone())
}

/// One-line bucket legend: `0 dårlig · 25 marginalt · 45 ok · 65 bra · 85 strålende`.
/// Each label is painted in its bucket's colour so the legend doubles
/// as a colour key.
fn bucket_legend(lang: Language) -> String {
    let (d, m, ok, b, s) = match lang {
        Language::Norwegian => ("dårlig", "marginalt", "ok", "bra", "strålende"),
        Language::Swedish => ("dåligt", "marginellt", "ok", "bra", "strålande"),
    };
    format!(
        "0 {} · 25 {} · 45 {} · 65 {} · 85 {}",
        theme::paint_label(d, 0),
        theme::paint_label(m, 25),
        theme::paint_label(ok, 45),
        theme::paint_label(b, 65),
        theme::paint_label(s, 85),
    )
}

/// Long-form axis label used in the per-axis bars row of `render_human`.
fn axis_label_long(axis: &str, lang: Language) -> &'static str {
    match (lang, axis) {
        (Language::Norwegian, "temperature") => "Temperatur",
        (Language::Norwegian, "wind") => "Vind",
        (Language::Norwegian, "precipitation") => "Nedbør",
        (Language::Norwegian, "ground") => "Bakke",
        (Language::Swedish, "temperature") => "Temperatur",
        (Language::Swedish, "wind") => "Vind",
        (Language::Swedish, "precipitation") => "Nederbörd",
        (Language::Swedish, "ground") => "Mark",
        _ => "?",
    }
}

/// Render one day's row.
///
/// Default mode is intentionally lean: day label, weather icon,
/// half-block bar, score, plus a `~` marker on low-confidence days
/// (which would otherwise repeat "ⓘ lav" on every long-range row).
/// Bucket labels and Ground-penalty chatter live once in the footer.
///
/// `--verbose` adds the per-axis sub-score tree and the full penalty
/// list under each day.
fn write_day_row(
    out: &mut String,
    day: &DayAggregate,
    today_local: NaiveDate,
    verbose: bool,
    flags: ChipFlags,
    lang: Language,
) {
    let center = day.center();
    let day_label = day_label(day.date, today_local, lang);
    let icon = center.weather_icon.as_str();

    let dim = theme::dim_for_confidence(day.confidence);

    // Left-aligned to 3 cells. Right-alignment makes "100" extend one
    // column further left than 2-digit scores (the first digit `1`
    // lands where the leading space of " 79" sits), which breaks
    // first-digit visual scanning down the score column. Left-align
    // anchors all first digits at the same column; the trailing space
    // for 2-digit values lands harmlessly between score and conf-marker.
    let score_str = format!("{:<3}", day.mean);
    let score_p = if dim {
        theme::paint_dim(&score_str)
    } else {
        theme::paint_score_str(&score_str, day.mean)
    };

    let day_label_p = if dim {
        theme::paint_dim(&day_label)
    } else {
        theme::paint_fg(&day_label)
    };

    let conf_marker = if day.confidence == Confidence::Lav {
        theme::paint_dim("~")
    } else {
        " ".to_string()
    };

    let _ = writeln!(
        out,
        "  {}  {}{} {}  {}  {}",
        pad_right(&day_label_p, &day_label, 11),
        icon,
        icon_pad(icon),
        colored_bar(day.mean),
        score_p,
        conf_marker,
    );

    let is_today = day.date == today_local;
    let penalty_take = center.score.penalties.len();

    // Sub-axis breakdown (Temp/Vind/Nedbør/Bakke):
    //   * default mode: today only — that's the day the user acts on
    //   * --verbose: every day. The tree's last row closes with `└─`
    //     unless penalty rows follow (verbose only).
    let want_tall =
        flags.window_stats && (is_today || verbose) && !day.center().score.stats.is_empty();
    if is_today || verbose {
        // The breakdown tree closes with `└─` unless something else
        // follows it — penalty rows in verbose, or the Tall stats line
        // when window_stats is enabled.
        let breakdown_continues = (verbose && penalty_take > 0) || want_tall;
        write_day_breakdown(out, day, dim, breakdown_continues, lang);
    }
    if want_tall {
        let stats = &day.center().score.stats;
        let tail_branch = if verbose && penalty_take > 0 {
            "├─"
        } else {
            "└─"
        };
        let _ = writeln!(
            out,
            "{BREAKDOWN_INDENT}{tail_branch} {} {}",
            theme::paint_dim(&format!("{:<9}", window_stats_chip_label(lang))),
            format_tall_line(stats, lang),
        );
    }

    // "Beste luke" — a sub-day window that scores significantly higher
    // than the day's mean. The score layer only emits an `optimal_window`
    // when the improvement clears its threshold, so rendering it
    // whenever it's `Some` is correct. Surface it in default mode too:
    // knowing "today's mean is 70 but 06–09 is 95" is one of the
    // highest-value signals the renderer can show.
    //
    // Read from `day.optimal_window` (aggregate-level) rather than
    // `center.optimal_window` (per-point) so the displayed `improvement`
    // and `reason` are aligned with the multi-point mean and breakdown
    // the rest of the row shows. The center's per-point view is computed
    // against its solo day total, which can disagree with the displayed
    // mean by several points.
    if let Some(ow) = &day.optimal_window {
        // "Luke" (NO) / "lucka" (SE) means an *opening* — it implies the
        // sub-window is meaningfully better than the day around it. When
        // the improvement is zero (which only happens when the user opted
        // in to `--best-window`, since the default config still filters
        // sub-windows that don't clear the threshold), the word is
        // misleading and the "+0 poeng" suffix is noise. Use "vindu" /
        // "fönster" instead and drop the suffix.
        let (phrase, points_word) = match (lang, ow.improvement) {
            (Language::Norwegian, 0) => ("Beste vindu", "poeng"),
            (Language::Norwegian, _) => ("Beste luke", "poeng"),
            (Language::Swedish, 0) => ("Bästa fönster", "poäng"),
            (Language::Swedish, _) => ("Bästa lucka", "poäng"),
        };
        let suffix = if ow.improvement > 0 {
            format!(", +{} {}", ow.improvement, points_word)
        } else {
            String::new()
        };
        // One-word "why this window" trailer. Only rendered when the score
        // layer found a clear axis winner — uniform days return None and
        // the line stays clean.
        let reason_trailer = ow
            .reason
            .map(|r| format!(" — {}", theme::paint_dim(r.label(lang))))
            .unwrap_or_default();
        let rain_trailer = best_window_rain_trailer(&ow.score.stats, lang, ow.improvement > 0)
            .map(|s| format!(" · {}", theme::paint_dim(&s)))
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "{BREAKDOWN_INDENT}★ {phrase}: {}–{} → {} ({}{}){}{}",
            local_hm(ow.window.start),
            local_hm(ow.window.end),
            theme::paint_score(ow.score.total),
            theme::paint_label(mean_label(ow.score.total, lang), ow.score.total),
            suffix,
            reason_trailer,
            rain_trailer,
        );
    }

    if !verbose {
        return;
    }

    // Verbose-only: full penalty list under each day, then any positive
    // context lines ("Sol bidrar med +2.8 °C ...") under the same tree.
    let highlight_take = center.score.highlights.len();
    let total_rows = penalty_take + highlight_take;
    for (i, penalty) in center.score.penalties.iter().take(penalty_take).enumerate() {
        let is_last = i + 1 == total_rows;
        let branch = if is_last { "└─" } else { "├─" };
        let _ = writeln!(
            out,
            "{BREAKDOWN_INDENT}{branch} {}",
            format_penalty_line(penalty, lang)
        );
    }
    for (i, highlight) in center.score.highlights.iter().enumerate() {
        let is_last = penalty_take + i + 1 == total_rows;
        let branch = if is_last { "└─" } else { "├─" };
        let _ = writeln!(out, "{BREAKDOWN_INDENT}{branch} ☀ {highlight}");
    }
}

/// Pick the day to feature in the "🎯 Beste dag" headline. Highest mean
/// wins; ties go to the higher-confidence day (a Høy 90 beats a Lav 90),
/// and any remaining ties go to the earliest date so the user lands on a
/// day they can actually act on.
fn pick_best_day(days: &[DayAggregate]) -> Option<&DayAggregate> {
    days.iter().max_by(|a, b| {
        a.mean
            .cmp(&b.mean)
            .then(a.confidence.rank().cmp(&b.confidence.rank()))
            .then(b.date.cmp(&a.date))
    })
}

/// Per-axis sub-score breakdown rendered as a small tree under the day
/// row. By default we only emit it for *today* (the day the user acts on);
/// `--verbose` extends it to every day. When `penalties_follow` is true,
/// the last row uses `├─` so the tree continues into the penalty list;
/// otherwise it closes with `└─`.
fn write_day_breakdown(
    out: &mut String,
    day: &DayAggregate,
    dim: bool,
    penalties_follow: bool,
    lang: Language,
) {
    let temp = avg_day_axis(day, |b| b.temperature);
    let wind = avg_day_axis(day, |b| b.wind);
    let precip = avg_day_axis(day, |b| b.precipitation);
    let prob = avg_day_axis(day, |b| b.precip_probability);
    let ground = avg_day_axis(day, |b| b.ground);
    let precip_combined = combined_precip(precip, prob, lang);

    let (temp_label, wind_label, precip_label, ground_label) = match lang {
        Language::Norwegian => ("Temp", "Vind", "Nedbør", "Bakke"),
        Language::Swedish => ("Temp", "Vind", "Nederbörd", "Mark"),
    };
    let rows: [(&str, u8, &str); 4] = [
        (temp_label, temp, ""),
        (wind_label, wind, ""),
        (precip_label, precip_combined, ""),
        (ground_label, ground, ""),
    ];
    let last_idx = rows.len() - 1;
    for (i, (label, value, suffix)) in rows.iter().enumerate() {
        let is_last_breakdown = i == last_idx;
        let branch = if is_last_breakdown && !penalties_follow {
            "└─"
        } else {
            "├─"
        };
        let _ = writeln!(
            out,
            "{BREAKDOWN_INDENT}{branch} {}{}",
            format_axis_row(label, *value, dim),
            theme::paint_dim(suffix),
        );
    }
}

/// One row of the breakdown tree: dim label, score-coloured bar, and the
/// numeric value. Label width is fixed so all rows align under each other.
fn format_axis_row(label: &str, value: u8, dim: bool) -> String {
    // Sized so Swedish "Nederbörd" (9 cells) fits without pushing the
    // bar — keeps sub-bars aligned under the parent across both
    // languages. Norwegian labels (Temp/Vind/Nedbør/Bakke, max 6) get
    // trailing padding to the same width.
    const LABEL_WIDTH: usize = 9;
    let label_padded = format!("{label:<LABEL_WIDTH$}");
    let label_p = theme::paint_dim(&label_padded);
    // Left-aligned to 3 cells, matching the day-row score so first
    // digits land in the same column whether the value is 2 or 3 digits.
    let value_str = format!("{value:<3}");
    let value_p = if dim {
        theme::paint_dim(&value_str)
    } else {
        theme::paint_score_str(&value_str, value)
    };
    // Two spaces between bar and value, matching the day row's format
    // string `"  {}  {}{} {}  {}  {}"` so the per-axis score lines up
    // vertically under the day's mean score.
    format!("{label_p} {}  {value_p}", colored_bar(value))
}

fn avg_day_axis<F>(day: &DayAggregate, f: F) -> u8
where
    F: Fn(&grusindeks_core::score::ScoreBreakdown) -> u8,
{
    let n = day.points.len() as u32;
    if n == 0 {
        return 0;
    }
    let sum: u32 = day
        .points
        .iter()
        .map(|p| u32::from(f(&p.day_score.score.breakdown)))
        .sum();
    (sum / n) as u8
}

/// Format a single penalty line: "Vind: snitt 8.2 m/s" with the
/// component label coloured by component and the message dimmed. The
/// component label adjusts to the requested language; the message body
/// is already localized at score-construction time.
fn format_penalty_line(p: &Penalty, lang: Language) -> String {
    format!(
        "{}: {}",
        theme::paint_component_label(p.component, lang),
        theme::paint_severity(&p.message, p.severity),
    )
}

/// Width-padding for the weather icon column so single-cell glyphs
/// (☀ ☁ ·) and double-cell glyphs (🌧 🌨 🌬 ⛅) leave the same gap
/// before the bar. We target a fixed 3-cell column for the icon: the
/// glyph itself takes 1 or 2 cells, the rest is padding.
fn icon_pad(icon: &str) -> &'static str {
    const TARGET: usize = 3;
    let w = UnicodeWidthStr::width(icon);
    match TARGET.saturating_sub(w) {
        0 => "",
        1 => " ",
        _ => "  ",
    }
}

/// Day label in the requested language. Uses "i dag/i morgen" (NO) or
/// "idag/imorgon" (SE) for the nearest two days, and a short weekday +
/// date otherwise.
fn day_label(date: NaiveDate, today: NaiveDate, lang: Language) -> String {
    use chrono::Datelike;
    let delta = (date - today).num_days();
    let (today_word, tomorrow_word) = match lang {
        Language::Norwegian => ("i dag", "i morgen"),
        Language::Swedish => ("idag", "imorgon"),
    };
    match delta {
        0 => today_word.to_string(),
        1 => tomorrow_word.to_string(),
        _ => format!(
            "{} {}. {}",
            weekday(date, lang),
            date.day(),
            month(date.month(), lang),
        ),
    }
}

/// Two-letter weekday abbreviation in the requested language. Lowercase
/// — neither Norwegian nor Swedish capitalises weekdays mid-sentence.
fn weekday(date: NaiveDate, lang: Language) -> &'static str {
    use chrono::Datelike;
    match (lang, date.weekday()) {
        (Language::Norwegian, chrono::Weekday::Mon) => "ma",
        (Language::Norwegian, chrono::Weekday::Tue) => "ti",
        (Language::Norwegian, chrono::Weekday::Wed) => "on",
        (Language::Norwegian, chrono::Weekday::Thu) => "to",
        (Language::Norwegian, chrono::Weekday::Fri) => "fr",
        (Language::Norwegian, chrono::Weekday::Sat) => "lø",
        (Language::Norwegian, chrono::Weekday::Sun) => "sø",
        (Language::Swedish, chrono::Weekday::Mon) => "må",
        (Language::Swedish, chrono::Weekday::Tue) => "ti",
        (Language::Swedish, chrono::Weekday::Wed) => "on",
        (Language::Swedish, chrono::Weekday::Thu) => "to",
        (Language::Swedish, chrono::Weekday::Fri) => "fr",
        (Language::Swedish, chrono::Weekday::Sat) => "lö",
        (Language::Swedish, chrono::Weekday::Sun) => "sö",
    }
}

/// Three-letter month abbreviation in the requested language. Lowercase
/// — neither language capitalises month names — so we render our own
/// rather than rely on `chrono`'s `%b`.
fn month(month: u32, lang: Language) -> &'static str {
    match (lang, month) {
        (Language::Norwegian, 1) => "jan",
        (Language::Norwegian, 2) => "feb",
        (Language::Norwegian, 3) => "mar",
        (Language::Norwegian, 4) => "apr",
        (Language::Norwegian, 5) => "mai",
        (Language::Norwegian, 6) => "jun",
        (Language::Norwegian, 7) => "jul",
        (Language::Norwegian, 8) => "aug",
        (Language::Norwegian, 9) => "sep",
        (Language::Norwegian, 10) => "okt",
        (Language::Norwegian, 11) => "nov",
        (Language::Norwegian, 12) => "des",
        (Language::Swedish, 1) => "jan",
        (Language::Swedish, 2) => "feb",
        (Language::Swedish, 3) => "mar",
        (Language::Swedish, 4) => "apr",
        (Language::Swedish, 5) => "maj",
        (Language::Swedish, 6) => "jun",
        (Language::Swedish, 7) => "jul",
        (Language::Swedish, 8) => "aug",
        (Language::Swedish, 9) => "sep",
        (Language::Swedish, 10) => "okt",
        (Language::Swedish, 11) => "nov",
        (Language::Swedish, 12) => "dec",
        _ => "?",
    }
}

/// Combine the precipitation amount and probability sub-scores into a
/// single number using their scoring weights. The combined value is the
/// only thing the renderer shows now — the previous
/// `(mengde X, sjanse Y)` annotation was distracting more than it
/// helped, so the detail is dropped entirely.
fn combined_precip(amount: u8, probability: u8, _lang: Language) -> u8 {
    use grusindeks_core::score::thresholds::{W_PRECIP, W_PROB};
    let w_sum = u32::from(W_PRECIP) + u32::from(W_PROB);
    ((u32::from(amount) * u32::from(W_PRECIP) + u32::from(probability) * u32::from(W_PROB)) / w_sum)
        as u8
}

fn write_axis_row(out: &mut String, label: &str, value: u8, suffix: &str) {
    let _ = writeln!(
        out,
        "{:<14} {} {}{}",
        label,
        colored_bar(value),
        theme::paint_score(value),
        suffix,
    );
}

fn avg_axis<F>(agg: &AggregateScore, f: F) -> u8
where
    F: Fn(&grusindeks_core::score::ScoreBreakdown) -> u8,
{
    let n = agg.points.len() as u32;
    if n == 0 {
        return 0;
    }
    let sum: u32 = agg
        .points
        .iter()
        .map(|p| u32::from(f(&p.score.breakdown)))
        .sum();
    (sum / n) as u8
}

/// `Vec<Penalty>` sourced from the center point of the aggregate, so the
/// "why" text matches the user's chosen ride centre, not an averaged
/// fiction.
fn center_penalties(agg: &AggregateScore) -> &[Penalty] {
    let center = agg
        .points
        .iter()
        .find(|p| p.is_center)
        .unwrap_or(&agg.points[0]);
    &center.score.penalties
}

fn mean_label(total: u8, lang: Language) -> &'static str {
    grusindeks_core::score::label_for(total, lang)
}

/// Pad a *coloured* string to `target_visible_width`. We can't trust the
/// formatter's `{:<N}` since ANSI escape codes inflate the length without
/// adding visible cells. The plain `visible` slice is what the eye sees;
/// we measure its display width with `unicode-width` so wide glyphs
/// (CJK, emoji, half/full-blocks) consume the right number of columns.
fn pad_right(coloured: &str, visible: &str, target_visible_width: usize) -> String {
    let visible_len = UnicodeWidthStr::width(visible);
    if visible_len >= target_visible_width {
        coloured.to_string()
    } else {
        let pad = " ".repeat(target_visible_width - visible_len);
        format!("{coloured}{pad}")
    }
}

/// Indent before the tree-prefix on hourly breakdown rows. Sized so the
/// breakdown cells land flush under the parent row's cells, letting the
/// reader trace a vertical line from a sub-score to the day's mean.
///
/// Math: parent row puts its first cell at column 15 (2 indent + 11
/// day-label + 2). Breakdown row layout is
/// `<indent>├─ <label-padded-9> <cells>`, so indent + 2 + 1 + 9 + 1 = 15,
/// giving indent = 2.
const HOURLY_BREAKDOWN_INDENT: &str = "  ";

/// Map a 0–100 score to a 2-cell shading glyph. Pairs with `score_color`
/// so the colour and the shading-density agree: the higher the score, the
/// denser the block. Five buckets share four glyphs — `dårlig` and
/// `marginalt` both use `░░` and rely on the colour gradient (red vs
/// orange) to differentiate. With `NO_COLOR` the two lowest buckets read
/// as one "kjør ikke"-zone, which is the right call ergonomically.
fn hourly_block_glyph(mean: u8) -> &'static str {
    match mean {
        0..=24 => "░░",
        25..=44 => "░░",
        45..=64 => "▒▒",
        65..=84 => "▓▓",
        _ => "██",
    }
}

/// Glyph-prefixed legend for the hourly view: each bucket label is
/// preceded by its shading glyph in the bucket colour, so the legend
/// doubles as a colour *and* glyph key.
fn hourly_bucket_legend(lang: Language) -> String {
    let (d, m, ok, b, s) = match lang {
        Language::Norwegian => ("dårlig", "marginalt", "ok", "bra", "strålende"),
        Language::Swedish => ("dåligt", "marginellt", "ok", "bra", "strålande"),
    };
    format!(
        "{} {} · {} {} · {} {} · {} {} · {} {}",
        theme::paint_score_str("░", 0),
        theme::paint_label(d, 0),
        theme::paint_score_str("░", 25),
        theme::paint_label(m, 25),
        theme::paint_score_str("▒", 45),
        theme::paint_label(ok, 45),
        theme::paint_score_str("▓", 65),
        theme::paint_label(b, 65),
        theme::paint_score_str("█", 85),
        theme::paint_label(s, 85),
    )
}

/// Render the hourly forecast: one row per day, one column per local hour
/// in the configured daytime window. Cells outside a day's clipped ride
/// window (typically the past hours of "today") render as a dim `··`
/// placeholder so the grid stays visually aligned. With `verbose`, each
/// day expands into 4 sub-rows (Temp / Vind / Nedbør / Bakke) so the
/// reader can see *which* axis dragged a low-scoring hour down.
pub fn render_hourly(
    label: &str,
    radius_km: f64,
    forecast: &HourlyForecast,
    verbose: bool,
    flags: ChipFlags,
    lang: Language,
) -> String {
    let mut out = String::new();
    let n = forecast.days.len();
    let day_word = match (lang, n) {
        (Language::Norwegian, 1) => "dag",
        (Language::Norwegian, _) => "dager",
        (Language::Swedish, 1) => "dag",
        (Language::Swedish, _) => "dagar",
    };
    let title_suffix = match lang {
        Language::Norwegian => "time-for-time",
        Language::Swedish => "timme-för-timme",
    };
    let title =
        format!("Grusindeks · {label} · {radius_km:.0} km · {n} {day_word} · {title_suffix}");
    let _ = writeln!(out, "{}", theme::paint_accent(&title));
    let _ = writeln!(out);
    write_nowcast_banner(&mut out, forecast.nowcast_alert.as_ref(), lang);

    if forecast.header_hours.is_empty() || forecast.days.is_empty() {
        let empty_msg = match lang {
            Language::Norwegian => "Ingen timer i konfigurert dag-vindu.",
            Language::Swedish => "Inga timmar i konfigurerat dag-fönster.",
        };
        let _ = writeln!(out, "  {}", theme::paint_dim(empty_msg));
        return out;
    }

    // Header row — same indent and day-label width as the multi-day
    // renderer, so the eye doesn't have to re-anchor when switching views.
    // Hour columns are 2 chars wide ("10".."21"), matching the heatmap
    // glyph width below.
    let header_cells: String = forecast
        .header_hours
        .iter()
        .map(|h| format!("{h:>2}"))
        .collect::<Vec<_>>()
        .join(" ");
    let _ = writeln!(
        out,
        "  {}  {}",
        theme::paint_dim(&" ".repeat(11)),
        theme::paint_dim(&header_cells),
    );

    let today_local: NaiveDate = Local::now().date_naive();
    for day in &forecast.days {
        write_hourly_day_row(
            &mut out,
            day,
            &forecast.header_hours,
            today_local,
            verbose,
            flags,
            lang,
        );
    }

    let _ = writeln!(out);
    if flags.rain_history {
        if let Some(msg) = forecast
            .rain_history
            .as_ref()
            .and_then(|h| rain_history_chip_content(h, lang))
        {
            let _ = writeln!(
                out,
                "  {}   {msg}",
                theme::paint_dim(rain_history_chip_label(lang)),
            );
        }
    }
    let scale_label = match lang {
        Language::Norwegian => "Skala",
        Language::Swedish => "Skala",
    };
    let _ = writeln!(
        out,
        "  {}   {}",
        theme::paint_dim(scale_label),
        hourly_bucket_legend(lang)
    );
    let placeholder_note = match lang {
        Language::Norwegian => "··      utenfor sykkelvinduet",
        Language::Swedish => "··      utanför åkfönstret",
    };
    let _ = writeln!(out, "  {}", theme::paint_dim(placeholder_note));
    out
}

/// One day's row in the hourly grid. Maps each header column (a local
/// clock hour) to the day's matching `HourScore`, painting the score with
/// its bucket colour. Hours outside the day's clipped ride window render
/// as a dim `··` placeholder. With `verbose`, four sub-rows follow with
/// the per-axis breakdown for the same hour columns.
fn write_hourly_day_row(
    out: &mut String,
    day: &crate::aggregate::HourlyDayAggregate,
    header_hours: &[u8],
    today_local: NaiveDate,
    verbose: bool,
    flags: ChipFlags,
    lang: Language,
) {
    let label = day_label(day.date, today_local, lang);
    let label_p = theme::paint_fg(&label);
    let mut cells: Vec<String> = Vec::with_capacity(header_hours.len());
    for &col_h in header_hours {
        cells.push(hourly_cell(day, col_h, |h| h.mean));
    }
    let _ = writeln!(
        out,
        "  {}  {}",
        pad_right(&label_p, &label, 11),
        cells.join(" "),
    );

    if verbose {
        // Reserve `└─` for the actual last row in the per-day tree. When
        // a Tall stats line follows the breakdown, the last sub-axis row
        // becomes `├─` so the tree visually continues into the Tall row.
        let want_tall = flags.window_stats && day.stats.is_some();
        write_hourly_day_breakdown(out, day, header_hours, want_tall, lang);
        if let Some(stats) = day.stats.as_ref() {
            if flags.window_stats {
                let label_padded = format!("{:<9}", window_stats_chip_label(lang));
                let _ = writeln!(
                    out,
                    "{HOURLY_BREAKDOWN_INDENT}{} {} {}",
                    theme::paint_dim("└─"),
                    theme::paint_dim(&label_padded),
                    format_tall_line(stats, lang),
                );
            }
        }
    }
}

/// Render one heatmap cell for a column in a day's row. `score_of` picks
/// either the mean or one of the breakdown axes off the matched
/// `HourScore`. Hours outside the day's clipped ride window render as a
/// dim `··` placeholder; low-confidence hours keep their cell glyph but
/// drop the colour so the eye gravitates to trustworthy data.
fn hourly_cell<F>(day: &crate::aggregate::HourlyDayAggregate, col_hour: u8, score_of: F) -> String
where
    F: Fn(&crate::aggregate::HourScore) -> u8,
{
    // Match the column to a scored hour by *local* clock hour. UTC hours
    // shift across a DST boundary; matching on local keeps the header
    // columns honest.
    let scored = day.hours.iter().find(|h| {
        let local_hour = h.time.with_timezone(&Local).hour() as u8;
        local_hour == col_hour && h.time.with_timezone(&Local).date_naive() == day.date
    });
    match scored {
        Some(h) => {
            let value = score_of(h);
            let glyph = hourly_block_glyph(value);
            if h.confidence == Confidence::Lav {
                theme::paint_dim(glyph)
            } else {
                theme::paint_score_str(glyph, value)
            }
        }
        None => theme::paint_dim(".."),
    }
}

/// Per-axis breakdown rows under a day's main row. Four sub-rows
/// (Temp / Vind / Nedbør / Bakke), each rendered as its own heatmap
/// strip aligned under the day's cells. Surfaces *why* a low-scoring
/// hour scores low (regn? vind? bakke?) without forcing the user back
/// into the daily view.
fn write_hourly_day_breakdown(
    out: &mut String,
    day: &crate::aggregate::HourlyDayAggregate,
    header_hours: &[u8],
    tail_continues: bool,
    lang: Language,
) {
    let (temp_label, wind_label, precip_label, ground_label) = match lang {
        Language::Norwegian => ("Temp", "Vind", "Nedbør", "Bakke"),
        Language::Swedish => ("Temp", "Vind", "Nederbörd", "Mark"),
    };
    // Type-erased per-axis pickers, so we can drive the four rows from
    // one loop. Nedbør folds amount + probability into the same combined
    // value the daily view shows, so the two surfaces agree on what a
    // "rain row" means.
    type Picker<'a> = (&'a str, Box<dyn Fn(&crate::aggregate::HourScore) -> u8>);
    let pickers: [Picker<'_>; 4] = [
        (
            temp_label,
            Box::new(|h: &crate::aggregate::HourScore| h.breakdown.temperature),
        ),
        (
            wind_label,
            Box::new(|h: &crate::aggregate::HourScore| h.breakdown.wind),
        ),
        (
            precip_label,
            Box::new(move |h: &crate::aggregate::HourScore| {
                combined_precip(
                    h.breakdown.precipitation,
                    h.breakdown.precip_probability,
                    lang,
                )
            }),
        ),
        (
            ground_label,
            Box::new(|h: &crate::aggregate::HourScore| h.breakdown.ground),
        ),
    ];
    let last_idx = pickers.len() - 1;
    for (i, (axis_label, picker)) in pickers.iter().enumerate() {
        let branch = if i == last_idx && !tail_continues {
            "└─"
        } else {
            "├─"
        };
        let mut cells: Vec<String> = Vec::with_capacity(header_hours.len());
        for &col_h in header_hours {
            cells.push(hourly_cell(day, col_h, picker.as_ref()));
        }
        // Indent + branch + space + 9-char label puts the first cell at
        // column 15, flush under the parent row's first cell. The label
        // itself is dim'd so the bright row is the data, not the prose.
        let label_padded = format!("{axis_label:<9}");
        let _ = writeln!(
            out,
            "{HOURLY_BREAKDOWN_INDENT}{} {} {}",
            theme::paint_dim(branch),
            theme::paint_dim(&label_padded),
            cells.join(" "),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use grusindeks_core::drying::SurfaceState;
    use grusindeks_core::geo::Point;
    use grusindeks_core::score::score;
    use grusindeks_core::types::HourlyConditions;

    fn t(h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 4, 26, h, 0, 0).unwrap()
    }

    fn perfect(time_h: u32) -> HourlyConditions {
        HourlyConditions {
            thunder: false,
            probability_of_precip: Some(5.0),
            ..HourlyConditions::minimal(t(time_h), 17.0, 2.0, 0.0)
        }
    }

    // ---- Nowcast banner ----

    fn alert_with(peak_mm_h: f64, first_in_min: i64, last_in_min: i64) -> NowcastAlert {
        let now = Utc::now();
        NowcastAlert {
            first_rain_at: now + ChronoDuration::minutes(first_in_min),
            last_rain_at: now + ChronoDuration::minutes(last_in_min),
            peak_at: now + ChronoDuration::minutes((first_in_min + last_in_min) / 2),
            peak_mm_h,
        }
    }

    #[test]
    fn nowcast_banner_omitted_when_alert_is_none() {
        let mut out = String::new();
        write_nowcast_banner(&mut out, None, Language::Norwegian);
        assert!(out.is_empty());
    }

    #[test]
    fn nowcast_banner_includes_peak_and_label_for_imminent_rain() {
        let mut out = String::new();
        let a = alert_with(0.4, 15, 25);
        write_nowcast_banner(&mut out, Some(&a), Language::Norwegian);
        assert!(out.contains("Radar"), "got {out}");
        assert!(out.contains("regn"), "got {out}");
        assert!(out.contains("0.4 mm/h"), "got {out}");
        // 15 min away → "om 15 min" or near.
        assert!(out.contains(" min"), "got {out}");
    }

    #[test]
    fn nowcast_banner_uses_hours_minutes_for_far_alerts() {
        let mut out = String::new();
        let a = alert_with(0.2, 95, 100);
        write_nowcast_banner(&mut out, Some(&a), Language::Norwegian);
        assert!(out.contains("1t"), "expected hour fragment in {out}");
    }

    #[test]
    fn nowcast_banner_swedish_uses_swedish_words() {
        let mut out = String::new();
        let a = alert_with(0.4, 25, 35);
        write_nowcast_banner(&mut out, Some(&a), Language::Swedish);
        assert!(out.contains("Radar"), "got {out}");
        assert!(
            out.contains("om "),
            "Swedish should still use 'om' prefix: {out}"
        );
    }

    #[test]
    fn human_output_includes_total_and_breakdown_labels() {
        let win = RideWindow::from_hours(t(14), 3);
        let s = score(
            &(14..17).map(perfect).collect::<Vec<_>>(),
            win,
            Some(SurfaceState::default()),
            Language::Norwegian,
        );
        let center = Point::new(59.9139, 10.7522);
        let agg = AggregateScore::from_points(center, vec![(center, s)], Language::Norwegian);
        let out = render_human(
            "Oslo",
            20.0,
            win,
            &agg,
            false,
            ChipFlags::default(),
            Language::Norwegian,
        );
        assert!(out.contains("Grusindeks for Oslo"));
        assert!(out.contains("Total:"));
        assert!(out.contains("Temperatur"));
        assert!(out.contains("Vind"));
        assert!(out.contains("Nedbør"));
        assert!(out.contains("Bakke"));
        assert!(
            !out.contains("Regnsannsynl."),
            "precipitation rows should now be merged: {out}"
        );
    }

    #[test]
    fn human_output_lists_penalty_when_score_dropped() {
        // Windy day → wind subscore drops → wind penalty surfaces.
        let win = RideWindow::from_hours(t(14), 3);
        let hours: Vec<HourlyConditions> = (14..17)
            .map(|h| HourlyConditions::minimal(t(h), 17.0, 9.0, 0.0))
            .collect();
        let s = score(
            &hours,
            win,
            Some(SurfaceState::default()),
            Language::Norwegian,
        );
        let center = Point::new(59.9139, 10.7522);
        let agg = AggregateScore::from_points(center, vec![(center, s)], Language::Norwegian);
        let out = render_human(
            "Oslo",
            20.0,
            win,
            &agg,
            false,
            ChipFlags::default(),
            Language::Norwegian,
        );
        // Component label + a number from the wind speed should be present.
        assert!(
            out.contains("Vind") && out.contains("9"),
            "expected wind penalty in {out}"
        );
    }

    #[test]
    fn aggregate_uses_swedish_compass_labels_when_language_is_swedish() {
        // Multi-point aggregate in Swedish must produce NÖ/Ö/SÖ (not the
        // Norwegian Ø) and "centrum" for the centre row, not "senter".
        let win = RideWindow::from_hours(t(14), 3);
        let s = score(
            &(14..17).map(perfect).collect::<Vec<_>>(),
            win,
            Some(SurfaceState::default()),
            Language::Swedish,
        );
        let center = Point::new(59.9139, 10.7522);
        // East offset → bearing 90° → "Ö" in Swedish.
        let east = Point::new(59.9139, 11.0);
        let agg = AggregateScore::from_points(
            center,
            vec![(center, s.clone()), (east, s)],
            Language::Swedish,
        );
        let center_pt = agg.points.iter().find(|p| p.is_center).unwrap();
        assert_eq!(center_pt.bearing_label, "centrum");
        let other = agg.points.iter().find(|p| !p.is_center).unwrap();
        assert!(
            !other.bearing_label.contains('Ø'),
            "expected Swedish Ö, got {:?}",
            other.bearing_label
        );
    }

    #[test]
    fn combined_precip_weights_amount_higher_than_probability() {
        // 25:10 weighting from W_PRECIP / W_PROB. With amount=100,
        // probability=40 we get (100*25 + 40*10) / 35 = 2900/35 = 82.
        assert_eq!(combined_precip(100, 40, Language::Norwegian), 82);
        assert_eq!(combined_precip(100, 100, Language::Norwegian), 100);
    }

    #[test]
    fn day_label_for_today_and_tomorrow_uses_norwegian_phrasing() {
        let today = NaiveDate::from_ymd_opt(2026, 4, 26).unwrap();
        assert_eq!(day_label(today, today, Language::Norwegian), "i dag");
        assert_eq!(
            day_label(
                today + chrono::Duration::days(1),
                today,
                Language::Norwegian
            ),
            "i morgen"
        );
    }

    #[test]
    fn day_label_for_distant_day_uses_weekday_and_date() {
        let today = NaiveDate::from_ymd_opt(2026, 4, 26).unwrap(); // Sunday
        let in_three = today + chrono::Duration::days(3); // Wednesday
        let label = day_label(in_three, today, Language::Norwegian);
        assert!(label.starts_with("on "), "got {label}");
        assert!(label.contains("29"), "got {label}");
        assert!(label.ends_with("apr"), "got {label}");
    }

    #[test]
    fn multi_day_render_includes_title_and_skala_footer() {
        use crate::aggregate::DayAggregate;
        use grusindeks_core::daily::compute_day;

        let win = RideWindow::from_hours(t(6), 12);
        let now = t(5);
        let center = Point::new(59.9139, 10.7522);
        let hours: Vec<_> = (6..18).map(perfect).collect();
        let day = DayAggregate::from_points(
            NaiveDate::from_ymd_opt(2026, 4, 26).unwrap(),
            win,
            center,
            vec![(
                center,
                compute_day(
                    &hours,
                    win,
                    Some(SurfaceState::default()),
                    now,
                    Language::Norwegian,
                    grusindeks_core::daily::BestWindowConfig::default(),
                ),
            )],
            Language::Norwegian,
        );
        let forecast = MultiDayForecast {
            days: vec![day],
            rain_history: None,
            nowcast_alert: None,
        };
        let out = render_multi_day(
            "Oslo",
            20.0,
            &forecast,
            false,
            ChipFlags::default(),
            Language::Norwegian,
        );
        assert!(out.contains("Grusindeks · Oslo"), "got {out}");
        assert!(out.contains("dag"), "got {out}");
        // Bucket legend lives once in the footer, replacing the per-row
        // bucket-name repetition.
        assert!(out.contains("Skala"), "missing scale legend: {out}");
        assert!(out.contains("strålende"), "missing bucket label: {out}");
    }

    #[test]
    fn multi_day_render_shows_headline_with_best_day() {
        use crate::aggregate::DayAggregate;
        use grusindeks_core::daily::compute_day;

        let win = RideWindow::from_hours(t(6), 12);
        let now = t(5);
        let center = Point::new(59.9139, 10.7522);
        let hours: Vec<_> = (6..18).map(perfect).collect();
        let day = DayAggregate::from_points(
            NaiveDate::from_ymd_opt(2026, 4, 26).unwrap(),
            win,
            center,
            vec![(
                center,
                compute_day(
                    &hours,
                    win,
                    Some(SurfaceState::default()),
                    now,
                    Language::Norwegian,
                    grusindeks_core::daily::BestWindowConfig::default(),
                ),
            )],
            Language::Norwegian,
        );
        let forecast = MultiDayForecast {
            days: vec![day],
            rain_history: None,
            nowcast_alert: None,
        };
        let out = render_multi_day(
            "Oslo",
            20.0,
            &forecast,
            false,
            ChipFlags::default(),
            Language::Norwegian,
        );
        // The "best day" callout uses the short prefix "Beste:" inside a
        // round-cornered box, plus the "★" star in front. Either pin
        // suffices to confirm the headline rendered.
        assert!(
            out.contains("★ Beste:") && out.contains("╭"),
            "expected best-day callout in {out}"
        );
    }

    #[test]
    fn multi_day_render_appends_one_word_reason_after_optimal_window() {
        // Mixed day with a clear dry "luke": the trailing reason should
        // surface as "tørrest" so the user gets a one-glance "why".
        use crate::aggregate::DayAggregate;
        use grusindeks_core::daily::compute_day;

        fn awful(time_h: u32) -> HourlyConditions {
            HourlyConditions {
                thunder: false,
                probability_of_precip: Some(95.0),
                ..HourlyConditions::minimal(t(time_h), 5.0, 11.0, 3.0)
            }
        }

        let win = RideWindow::from_hours(t(6), 12);
        let now = t(5);
        let center = Point::new(59.9139, 10.7522);
        let mut hours: Vec<_> = (6..9).map(perfect).collect();
        hours.extend((9..15).map(awful));
        hours.extend((15..18).map(perfect));
        let day = DayAggregate::from_points(
            NaiveDate::from_ymd_opt(2026, 4, 26).unwrap(),
            win,
            center,
            vec![(
                center,
                compute_day(
                    &hours,
                    win,
                    Some(SurfaceState::default()),
                    now,
                    Language::Norwegian,
                    grusindeks_core::daily::BestWindowConfig::default(),
                ),
            )],
            Language::Norwegian,
        );
        let forecast = MultiDayForecast {
            days: vec![day],
            rain_history: None,
            nowcast_alert: None,
        };
        let out = render_multi_day(
            "Oslo",
            20.0,
            &forecast,
            true,
            ChipFlags::default(),
            Language::Norwegian,
        );
        assert!(
            out.contains("tørrest"),
            "expected 'tørrest' reason after Beste luke line: {out}"
        );
        assert!(
            out.contains("opphold i luka"),
            "expected dry-window rain status after Beste luke line: {out}"
        );
    }

    #[test]
    fn multi_day_render_appends_rain_status_after_rainy_best_window() {
        use crate::aggregate::DayAggregate;
        use grusindeks_core::daily::{compute_day, BestWindowConfig};

        fn rainy(time_h: u32) -> HourlyConditions {
            HourlyConditions {
                thunder: false,
                probability_of_precip: Some(95.0),
                ..HourlyConditions::minimal(t(time_h), 12.0, 3.0, 1.0)
            }
        }

        let win = RideWindow::from_hours(t(6), 12);
        let now = t(5);
        let center = Point::new(59.9139, 10.7522);
        let hours: Vec<_> = (6..18).map(rainy).collect();
        let cfg = BestWindowConfig {
            length_hours: 3,
            min_improvement: 0,
            excluded_windows: Vec::new(),
        };
        let day = DayAggregate::from_points(
            NaiveDate::from_ymd_opt(2026, 4, 26).unwrap(),
            win,
            center,
            vec![(
                center,
                compute_day(
                    &hours,
                    win,
                    Some(SurfaceState::default()),
                    now,
                    Language::Norwegian,
                    cfg,
                ),
            )],
            Language::Norwegian,
        );
        let forecast = MultiDayForecast {
            days: vec![day],
            rain_history: None,
            nowcast_alert: None,
        };
        let out = render_multi_day(
            "Oslo",
            20.0,
            &forecast,
            false,
            ChipFlags::default(),
            Language::Norwegian,
        );
        assert!(
            out.contains("regn 3.0 mm i vinduet"),
            "expected rainy-window status after Beste vindu line: {out}"
        );
    }

    #[test]
    fn multi_day_render_uses_vindu_label_and_omits_zero_improvement_suffix() {
        // Uniformly perfect day + `--best-window`-style config (min_improvement: 0)
        // → optimal_window is Some with improvement == 0. The renderer must:
        //   1. say "Beste vindu" instead of "Beste luke" (luke implies a
        //      contrast against the rest of the day);
        //   2. drop the "+0 poeng" suffix entirely (noise).
        use crate::aggregate::DayAggregate;
        use grusindeks_core::daily::{compute_day, BestWindowConfig};

        let win = RideWindow::from_hours(t(6), 12);
        let now = t(5);
        let center = Point::new(59.9139, 10.7522);
        let hours: Vec<_> = (6..18).map(perfect).collect();
        let cfg = BestWindowConfig {
            length_hours: 2,
            min_improvement: 0,
            excluded_windows: Vec::new(),
        };
        let day = DayAggregate::from_points(
            NaiveDate::from_ymd_opt(2026, 4, 26).unwrap(),
            win,
            center,
            vec![(
                center,
                compute_day(
                    &hours,
                    win,
                    Some(SurfaceState::default()),
                    now,
                    Language::Norwegian,
                    cfg,
                ),
            )],
            Language::Norwegian,
        );
        let forecast = MultiDayForecast {
            days: vec![day],
            rain_history: None,
            nowcast_alert: None,
        };
        let out = render_multi_day(
            "Oslo",
            20.0,
            &forecast,
            false,
            ChipFlags::default(),
            Language::Norwegian,
        );
        assert!(
            out.contains("Beste vindu"),
            "expected 'Beste vindu' label when improvement is 0, got: {out}"
        );
        assert!(
            !out.contains("Beste luke"),
            "should not say 'Beste luke' when improvement is 0: {out}"
        );
        assert!(
            !out.contains("+0 poeng"),
            "should drop the '+0 poeng' suffix: {out}"
        );
    }

    #[test]
    fn multi_day_render_calls_out_optimal_luke_when_present() {
        use crate::aggregate::DayAggregate;
        use grusindeks_core::daily::compute_day;
        use grusindeks_core::types::HourlyConditions;

        fn awful(time_h: u32) -> HourlyConditions {
            HourlyConditions {
                thunder: false,
                probability_of_precip: Some(95.0),
                ..HourlyConditions::minimal(t(time_h), 5.0, 11.0, 3.0)
            }
        }

        let win = RideWindow::from_hours(t(6), 12);
        let now = t(5);
        let center = Point::new(59.9139, 10.7522);
        let mut hours: Vec<_> = (6..9).map(perfect).collect();
        hours.extend((9..15).map(awful));
        hours.extend((15..18).map(perfect));
        let day = DayAggregate::from_points(
            NaiveDate::from_ymd_opt(2026, 4, 26).unwrap(),
            win,
            center,
            vec![(
                center,
                compute_day(
                    &hours,
                    win,
                    Some(SurfaceState::default()),
                    now,
                    Language::Norwegian,
                    grusindeks_core::daily::BestWindowConfig::default(),
                ),
            )],
            Language::Norwegian,
        );
        let forecast = MultiDayForecast {
            days: vec![day],
            rain_history: None,
            nowcast_alert: None,
        };
        // Verbose surfaces "Beste luke" under days with a usable
        // optimal window. Default mode keeps the per-day rows lean and
        // the luke callout lives only in --verbose now.
        let out = render_multi_day(
            "Oslo",
            20.0,
            &forecast,
            true,
            ChipFlags::default(),
            Language::Norwegian,
        );
        assert!(
            out.contains("Beste luke") || out.contains("luke"),
            "expected luke callout in verbose output: {out}"
        );
    }

    #[test]
    fn multi_day_render_surfaces_ground_state_in_footer() {
        use crate::aggregate::DayAggregate;
        use grusindeks_core::daily::compute_day;

        // Saturated ground → Ground penalty exists. Default mode now
        // surfaces it once in the footer "Bakke" chip rather than
        // repeating it under every day row.
        let win = RideWindow::from_hours(t(6), 12);
        let now = t(5);
        let center = Point::new(59.9139, 10.7522);
        let hours: Vec<_> = (6..18).map(perfect).collect();
        let day = DayAggregate::from_points(
            NaiveDate::from_ymd_opt(2026, 4, 26).unwrap(),
            win,
            center,
            vec![(
                center,
                compute_day(
                    &hours,
                    win,
                    Some(SurfaceState::new(5.0)),
                    now,
                    Language::Norwegian,
                    grusindeks_core::daily::BestWindowConfig::default(),
                ),
            )],
            Language::Norwegian,
        );
        let forecast = MultiDayForecast {
            days: vec![day],
            rain_history: None,
            nowcast_alert: None,
        };
        let out = render_multi_day(
            "Oslo",
            20.0,
            &forecast,
            false,
            ChipFlags::default(),
            Language::Norwegian,
        );
        assert!(
            out.to_lowercase().contains("bakke"),
            "expected Bakke chip in footer: {out}"
        );
        // The breakdown tree for "i dag" still renders in default mode
        // (its rows look like `├─ Temp …` — no colon). What default mode
        // *omits* is the per-day penalty list (rows with `: ` between
        // the component label and its message). Verbose adds those back.
        let count_penalty_rows = |s: &str| {
            s.lines()
                .filter(|line| (line.contains("├─") || line.contains("└─")) && line.contains(": "))
                .count()
        };
        assert_eq!(
            count_penalty_rows(&out),
            0,
            "default mode should not show per-day penalty rows: {out}"
        );
        let verbose = render_multi_day(
            "Oslo",
            20.0,
            &forecast,
            true,
            ChipFlags::default(),
            Language::Norwegian,
        );
        assert!(
            count_penalty_rows(&verbose) > 0,
            "verbose should surface per-day penalty rows: {verbose}"
        );
    }

    #[test]
    #[ignore = "demo: run with --ignored --nocapture to inspect the rendered single-day output"]
    fn demo_render_human_with_storm() {
        // Storm wind triggers the hard-cap path.
        let win = RideWindow::from_hours(t(14), 3);
        let hours: Vec<HourlyConditions> = (14..17)
            .map(|h| HourlyConditions {
                thunder: false,
                wind_gust_ms: Some(22.0),
                cloud_area_fraction: Some(95.0),
                probability_of_precip: Some(40.0),
                ..HourlyConditions::minimal(t(h), 9.0, 16.0, 0.5)
            })
            .collect();
        let s = score(
            &hours,
            win,
            Some(SurfaceState::new(3.0)),
            Language::Norwegian,
        );
        let center = Point::new(59.9139, 10.7522);
        let agg = AggregateScore::from_points(center, vec![(center, s)], Language::Norwegian);
        eprintln!(
            "\n--- DEFAULT ---\n{}",
            render_human(
                "Oslo",
                20.0,
                win,
                &agg,
                false,
                ChipFlags::default(),
                Language::Norwegian
            )
        );
        eprintln!(
            "--- VERBOSE ---\n{}",
            render_human(
                "Oslo",
                20.0,
                win,
                &agg,
                true,
                ChipFlags::default(),
                Language::Norwegian
            )
        );
    }

    #[test]
    #[ignore = "demo: run with --ignored --nocapture to inspect the rendered multi-day output"]
    fn demo_render_multi_day_with_mixed_weather() {
        use crate::aggregate::DayAggregate;
        use grusindeks_core::daily::compute_day;

        fn at(time_h: u32, day_offset: i64) -> DateTime<Utc> {
            let date = NaiveDate::from_ymd_opt(2026, 4, 26)
                .unwrap()
                .checked_add_days(chrono::Days::new(day_offset as u64))
                .unwrap();
            Utc.from_utc_datetime(&date.and_hms_opt(time_h, 0, 0).unwrap())
        }
        fn windy(time_h: u32, day_offset: i64) -> HourlyConditions {
            HourlyConditions {
                thunder: false,
                wind_gust_ms: Some(13.0),
                cloud_area_fraction: Some(80.0),
                ..HourlyConditions::minimal(at(time_h, day_offset), 6.0, 9.0, 0.0)
            }
        }
        fn rainy(time_h: u32, day_offset: i64) -> HourlyConditions {
            HourlyConditions {
                thunder: false,
                probability_of_precip: Some(85.0),
                cloud_area_fraction: Some(95.0),
                ..HourlyConditions::minimal(at(time_h, day_offset), 8.0, 4.0, 1.5)
            }
        }
        fn nice(time_h: u32, day_offset: i64) -> HourlyConditions {
            HourlyConditions {
                thunder: false,
                probability_of_precip: Some(10.0),
                cloud_area_fraction: Some(30.0),
                ..HourlyConditions::minimal(at(time_h, day_offset), 17.0, 3.0, 0.0)
            }
        }

        let center = Point::new(59.9139, 10.7522);
        let now = Utc.with_ymd_and_hms(2026, 4, 26, 5, 0, 0).unwrap();
        let mut days = Vec::new();
        let recipes: [fn(u32, i64) -> HourlyConditions; 6] =
            [nice, rainy, nice, windy, nice, rainy];
        for (offset, hr) in recipes.iter().enumerate() {
            let date = NaiveDate::from_ymd_opt(2026, 4, 26)
                .unwrap()
                .checked_add_days(chrono::Days::new(offset as u64))
                .unwrap();
            let win = RideWindow::from_hours(
                Utc.from_utc_datetime(&date.and_hms_opt(6, 0, 0).unwrap()),
                12,
            );
            let hours: Vec<_> = (6..18).map(|h| hr(h, offset as i64)).collect();
            let day = DayAggregate::from_points(
                date,
                win,
                center,
                vec![(
                    center,
                    compute_day(
                        &hours,
                        win,
                        Some(SurfaceState::new(1.0)),
                        now,
                        Language::Norwegian,
                        grusindeks_core::daily::BestWindowConfig::default(),
                    ),
                )],
                Language::Norwegian,
            );
            days.push(day);
        }
        let forecast = MultiDayForecast {
            days,
            rain_history: None,
            nowcast_alert: None,
        };
        eprintln!(
            "\n--- DEFAULT ---\n{}",
            render_multi_day(
                "Oslo",
                20.0,
                &forecast,
                false,
                ChipFlags::default(),
                Language::Norwegian
            )
        );
        eprintln!(
            "--- VERBOSE ---\n{}",
            render_multi_day(
                "Oslo",
                20.0,
                &forecast,
                true,
                ChipFlags::default(),
                Language::Norwegian
            )
        );
    }

    #[test]
    fn verbose_multi_day_lists_more_penalties_than_default() {
        use crate::aggregate::DayAggregate;
        use grusindeks_core::daily::compute_day;

        // Cold + windy + rainy + saturated → multiple penalties.
        let win = RideWindow::from_hours(t(6), 12);
        let now = t(5);
        let center = Point::new(59.9139, 10.7522);
        let hours: Vec<HourlyConditions> = (6..18)
            .map(|h| HourlyConditions {
                thunder: false,
                probability_of_precip: Some(80.0),
                ..HourlyConditions::minimal(t(h), 0.0, 9.0, 0.6)
            })
            .collect();
        let day = DayAggregate::from_points(
            NaiveDate::from_ymd_opt(2026, 4, 26).unwrap(),
            win,
            center,
            vec![(
                center,
                compute_day(
                    &hours,
                    win,
                    Some(SurfaceState::new(5.0)),
                    now,
                    Language::Norwegian,
                    grusindeks_core::daily::BestWindowConfig::default(),
                ),
            )],
            Language::Norwegian,
        );
        let forecast = MultiDayForecast {
            days: vec![day],
            rain_history: None,
            nowcast_alert: None,
        };
        let default_out = render_multi_day(
            "Oslo",
            20.0,
            &forecast,
            false,
            ChipFlags::default(),
            Language::Norwegian,
        );
        let verbose_out = render_multi_day(
            "Oslo",
            20.0,
            &forecast,
            true,
            ChipFlags::default(),
            Language::Norwegian,
        );
        // Penalty rows are tree-prefixed (`├─`/`└─`) AND contain `: ` between
        // the component label and its message. Breakdown rows share the
        // tree prefix but never carry a colon, which is what lets us count
        // penalty rows specifically.
        let count_penalty_rows = |s: &str| {
            s.lines()
                .filter(|line| (line.contains("├─") || line.contains("└─")) && line.contains(": "))
                .count()
        };
        assert!(
            count_penalty_rows(&verbose_out) > count_penalty_rows(&default_out),
            "verbose should add penalty rows: default={}, verbose={}\n--- default ---\n{default_out}\n--- verbose ---\n{verbose_out}",
            count_penalty_rows(&default_out),
            count_penalty_rows(&verbose_out),
        );
    }

    fn at(date: NaiveDate, h: u32) -> DateTime<Utc> {
        Utc.from_utc_datetime(&date.and_hms_opt(h, 0, 0).unwrap())
    }

    fn one_day_forecast(date: NaiveDate) -> MultiDayForecast {
        use crate::aggregate::DayAggregate;
        use grusindeks_core::daily::compute_day;

        let win = RideWindow::from_hours(at(date, 6), 12);
        let now = at(date, 5);
        let center = Point::new(59.9139, 10.7522);
        let hours: Vec<HourlyConditions> = (6..18)
            .map(|h| HourlyConditions::minimal(at(date, h), 17.0, 2.0, 0.0))
            .collect();
        let day = DayAggregate::from_points(
            date,
            win,
            center,
            vec![(
                center,
                compute_day(
                    &hours,
                    win,
                    Some(SurfaceState::default()),
                    now,
                    Language::Norwegian,
                    grusindeks_core::daily::BestWindowConfig::default(),
                ),
            )],
            Language::Norwegian,
        );
        MultiDayForecast {
            days: vec![day],
            rain_history: None,
            nowcast_alert: None,
        }
    }

    fn day_with(date: NaiveDate, mean: u8, confidence: Confidence) -> DayAggregate {
        use crate::aggregate::DayPointScore;
        use grusindeks_core::daily::DayScore;
        use grusindeks_core::score::{score, ScoreBreakdown};

        let win = RideWindow::from_hours(at(date, 6), 12);
        let center = Point::new(59.9139, 10.7522);
        let dummy_score = score(
            &[] as &[HourlyConditions],
            win,
            Some(SurfaceState::default()),
            Language::Norwegian,
        );
        let day_score = DayScore {
            window: win,
            score: grusindeks_core::score::Grusindeks {
                total: mean,
                breakdown: ScoreBreakdown {
                    temperature: 0,
                    wind: 0,
                    precipitation: 0,
                    precip_probability: 0,
                    ground: 0,
                },
                ..dummy_score
            },
            confidence,
            hours_with_data: 12,
            optimal_window: None,
            weather_icon: "☀".to_string(),
        };
        DayAggregate {
            date,
            window: win,
            min: mean,
            mean,
            max: mean,
            confidence,
            optimal_window: None,
            points: vec![DayPointScore {
                point: center,
                bearing_deg: 0.0,
                bearing_label: "senter".to_string(),
                is_center: true,
                day_score,
            }],
        }
    }

    #[test]
    fn pick_best_day_breaks_ties_by_confidence_then_earliest_date() {
        use chrono::Duration;
        let today = NaiveDate::from_ymd_opt(2026, 4, 26).unwrap();

        // Three days, all score 90: high-conf tomorrow, low-conf today,
        // medium-conf in two days. Best should be tomorrow (highest
        // confidence wins the mean tie).
        let days = vec![
            day_with(today, 90, Confidence::Lav),
            day_with(today + Duration::days(1), 90, Confidence::Hoy),
            day_with(today + Duration::days(2), 90, Confidence::Middels),
        ];
        let best = pick_best_day(&days).expect("non-empty");
        assert_eq!(best.date, today + Duration::days(1));

        // All same confidence + same mean → earliest date wins.
        let days_same_conf = vec![
            day_with(today + Duration::days(2), 80, Confidence::Hoy),
            day_with(today + Duration::days(1), 80, Confidence::Hoy),
            day_with(today, 80, Confidence::Hoy),
        ];
        let best = pick_best_day(&days_same_conf).expect("non-empty");
        assert_eq!(best.date, today, "earliest date should win on full tie");

        // Higher mean still beats higher confidence.
        let days_mean_wins = vec![
            day_with(today, 70, Confidence::Hoy),
            day_with(today + Duration::days(3), 95, Confidence::Lav),
        ];
        let best = pick_best_day(&days_mean_wins).expect("non-empty");
        assert_eq!(
            best.mean, 95,
            "higher mean should still win over confidence"
        );
    }

    #[test]
    fn multi_day_render_shows_breakdown_for_today_by_default() {
        // Today is the day the user actually acts on, so its sub-axis
        // tree is rendered in default mode. Future days only get it
        // with --verbose.
        let today = Local::now().date_naive();
        let forecast = one_day_forecast(today);
        let out = render_multi_day(
            "Oslo",
            20.0,
            &forecast,
            false,
            ChipFlags::default(),
            Language::Norwegian,
        );
        assert!(
            out.contains("Temp "),
            "expected today's breakdown in default output:\n{out}"
        );
    }

    #[test]
    fn multi_day_render_omits_breakdown_for_non_today_by_default() {
        let today = Local::now().date_naive();
        let future = today + chrono::Duration::days(2);
        let forecast = one_day_forecast(future);
        let out = render_multi_day(
            "Oslo",
            20.0,
            &forecast,
            false,
            ChipFlags::default(),
            Language::Norwegian,
        );
        assert!(
            !out.contains("Temp "),
            "future day should not show breakdown by default:\n{out}"
        );
    }

    #[test]
    fn multi_day_render_verbose_extends_breakdown_to_every_day() {
        let today = Local::now().date_naive();
        let mut forecast = one_day_forecast(today);
        forecast
            .days
            .extend(one_day_forecast(today + chrono::Duration::days(1)).days);

        let count = |s: &str| s.matches("Temp ").count();
        let default_chips = count(&render_multi_day(
            "Oslo",
            20.0,
            &forecast,
            false,
            ChipFlags::default(),
            Language::Norwegian,
        ));
        let verbose_chips = count(&render_multi_day(
            "Oslo",
            20.0,
            &forecast,
            true,
            ChipFlags::default(),
            Language::Norwegian,
        ));
        // Default: today only → 1 "Temp " hit. Verbose: today + tomorrow → 2.
        assert!(
            verbose_chips > default_chips,
            "verbose should add breakdown rows: default={default_chips}, verbose={verbose_chips}"
        );
    }

    // ---- Footer chip helpers ----

    fn at_local(date: NaiveDate, h: u32) -> DateTime<Utc> {
        Utc.from_utc_datetime(&date.and_hms_opt(h, 0, 0).unwrap())
    }

    fn forecast_with_history(date: NaiveDate, rain: Option<RainHistory>) -> MultiDayForecast {
        let mut f = one_day_forecast(date);
        f.rain_history = rain;
        f
    }

    #[test]
    fn rain_history_chip_returns_none_when_dry() {
        // Dry weeks: Bakke chip already says "(N døgn uten regn)" with the
        // same number, so a parallel "tørt siste N døgn" Regn 7d row was
        // pure redundancy. Skip it entirely instead.
        let h = RainHistory {
            total_mm: 0.0,
            wettest_day_mm: 0.0,
            wettest_day: NaiveDate::from_ymd_opt(2026, 4, 21).unwrap(),
            rain_days: 0,
            lookback_hours: 168,
        };
        assert!(rain_history_chip_content(&h, Language::Norwegian).is_none());
        assert!(rain_history_chip_content(&h, Language::Swedish).is_none());
    }

    #[test]
    fn rain_history_chip_full_form_norwegian_and_swedish() {
        let h = RainHistory {
            total_mm: 14.2,
            wettest_day_mm: 8.4,
            wettest_day: NaiveDate::from_ymd_opt(2026, 4, 18).unwrap(),
            rain_days: 3,
            lookback_hours: 168,
        };
        let no = rain_history_chip_content(&h, Language::Norwegian).expect("Some");
        assert!(no.contains("14.2 mm siste 7 døgn"), "got {no}");
        assert!(no.contains("våtest 18. apr"), "got {no}");
        assert!(no.contains("(8.4 mm)"), "got {no}");
        assert!(no.contains("3 regndøgn"), "got {no}");
        let sv = rain_history_chip_content(&h, Language::Swedish).expect("Some");
        assert!(sv.contains("14.2 mm senaste 7 dygn"), "got {sv}");
        assert!(sv.contains("blötast 18. apr"), "got {sv}");
        assert!(sv.contains("3 regndagar"), "got {sv}");
    }

    #[test]
    fn window_stats_chip_renders_temp_range() {
        let stats = WindowStats {
            mean_temp_c: 15.0,
            felt_temp_c: 15.0,
            min_temp_c: 12.0,
            max_temp_c: 18.0,
            total_precip_mm: 0.4,
            max_hourly_precip_mm: 0.2,
            max_wind_ms: 7.0,
            max_gust_ms: Some(11.0),
            mean_humidity_pct: None,
            wind_from_deg: None,
        };
        let s = window_stats_chip_content(&stats, Language::Norwegian).expect("Some");
        assert!(s.contains("12–18 °C"), "got {s}");
        assert!(s.contains("nedbør 0.4 mm"), "got {s}");
        assert!(s.contains("vind 7 m/s (kast 11)"), "got {s}");
    }

    #[test]
    fn window_stats_chip_omits_gust_when_none() {
        let stats = WindowStats {
            mean_temp_c: 15.0,
            felt_temp_c: 15.0,
            min_temp_c: 15.0,
            max_temp_c: 15.0,
            total_precip_mm: 0.0,
            max_hourly_precip_mm: 0.0,
            max_wind_ms: 5.0,
            max_gust_ms: None,
            mean_humidity_pct: None,
            wind_from_deg: None,
        };
        let s = window_stats_chip_content(&stats, Language::Norwegian).expect("Some");
        assert!(!s.contains("kast"), "expected no gust mention: {s}");
        // Single-hour-style window: equal min/max collapses to a single number.
        assert!(s.contains("15 °C"), "got {s}");
        assert!(!s.contains("15–15"), "should not render n–n: {s}");
    }

    #[test]
    fn window_stats_chip_returns_none_for_empty_stats() {
        let stats = WindowStats::empty();
        assert!(window_stats_chip_content(&stats, Language::Norwegian).is_none());
    }

    #[test]
    fn footer_renders_both_chips_when_data_available() {
        let date = NaiveDate::from_ymd_opt(2026, 4, 26).unwrap();
        let history = RainHistory {
            total_mm: 14.2,
            wettest_day_mm: 8.4,
            wettest_day: NaiveDate::from_ymd_opt(2026, 4, 18).unwrap(),
            rain_days: 3,
            lookback_hours: 168,
        };
        let forecast = forecast_with_history(date, Some(history));
        // verbose=true so the breakdown (and the Tall row inside it) renders
        // regardless of how `today_local` resolves at test time. The chip
        // logic itself is what we're asserting, not the today-detection.
        let out = render_multi_day(
            "Oslo",
            20.0,
            &forecast,
            true,
            ChipFlags::default(),
            Language::Norwegian,
        );
        assert!(out.contains("Regn 7d"), "missing rain history chip: {out}");
        assert!(out.contains("Tall"), "missing window stats row: {out}");
    }

    #[test]
    fn footer_omits_rain_chip_when_history_is_none() {
        let date = NaiveDate::from_ymd_opt(2026, 4, 26).unwrap();
        let forecast = forecast_with_history(date, None);
        let out = render_multi_day(
            "Oslo",
            20.0,
            &forecast,
            false,
            ChipFlags::default(),
            Language::Norwegian,
        );
        assert!(
            !out.contains("Regn 7d"),
            "should not render rain chip without history: {out}"
        );
    }

    #[test]
    fn flag_no_rain_history_hides_chip_even_with_data() {
        let date = NaiveDate::from_ymd_opt(2026, 4, 26).unwrap();
        let history = RainHistory {
            total_mm: 14.2,
            wettest_day_mm: 8.4,
            wettest_day: NaiveDate::from_ymd_opt(2026, 4, 18).unwrap(),
            rain_days: 3,
            lookback_hours: 168,
        };
        let forecast = forecast_with_history(date, Some(history));
        let flags = ChipFlags {
            rain_history: false,
            window_stats: true,
        };
        // verbose=true so the per-day Tall row renders independently of
        // `today_local` resolution at test time.
        let out = render_multi_day("Oslo", 20.0, &forecast, true, flags, Language::Norwegian);
        assert!(
            !out.contains("Regn 7d"),
            "flag should hide rain chip: {out}"
        );
        assert!(
            out.contains("Tall"),
            "window stats row should still render: {out}"
        );
    }

    #[test]
    fn flag_no_window_stats_hides_chip() {
        let date = NaiveDate::from_ymd_opt(2026, 4, 26).unwrap();
        let forecast = forecast_with_history(date, None);
        let flags = ChipFlags {
            rain_history: true,
            window_stats: false,
        };
        let out = render_multi_day("Oslo", 20.0, &forecast, false, flags, Language::Norwegian);
        assert!(!out.contains("Tall"), "flag should hide stats chip: {out}");
    }

    #[test]
    fn footer_renders_swedish_labels() {
        let date = NaiveDate::from_ymd_opt(2026, 4, 26).unwrap();
        let history = RainHistory {
            total_mm: 14.2,
            wettest_day_mm: 8.4,
            wettest_day: NaiveDate::from_ymd_opt(2026, 4, 18).unwrap(),
            rain_days: 3,
            lookback_hours: 168,
        };
        let mut forecast = forecast_with_history(date, Some(history));
        // Localise the forecast by rebuilding the day with Swedish.
        let win = RideWindow::from_hours(at_local(date, 6), 12);
        let now = at_local(date, 5);
        let center = Point::new(59.9139, 10.7522);
        let hours: Vec<HourlyConditions> = (6..18)
            .map(|h| HourlyConditions::minimal(at_local(date, h), 17.0, 2.0, 0.0))
            .collect();
        let day = crate::aggregate::DayAggregate::from_points(
            date,
            win,
            center,
            vec![(
                center,
                grusindeks_core::daily::compute_day(
                    &hours,
                    win,
                    Some(SurfaceState::default()),
                    now,
                    Language::Swedish,
                    grusindeks_core::daily::BestWindowConfig::default(),
                ),
            )],
            Language::Swedish,
        );
        forecast.days = vec![day];
        // verbose=true so per-day Tall renders for the assertion below
        // regardless of today_local resolution at test time.
        let out = render_multi_day(
            "Oslo",
            20.0,
            &forecast,
            true,
            ChipFlags::default(),
            Language::Swedish,
        );
        assert!(out.contains("Tal"), "missing Swedish stats label: {out}");
        assert!(out.contains("Regn 7d"), "missing Swedish rain label: {out}");
        assert!(
            out.contains("regndagar"),
            "expected Swedish day word: {out}"
        );
    }

    #[test]
    fn colored_bar_glyph_counts() {
        // Colour helpers degrade to plain text outside a TTY (test runner
        // is not a TTY), so the byte content is just the bar glyphs. The
        // half-block ramp `▏▎▍▌▋▊▉` lives in U+258F..U+2589, so we count
        // bar glyphs by checking the script range rather than enumerating.
        let is_bar = |c: char| matches!(c, '█' | '▉' | '▊' | '▋' | '▌' | '▍' | '▎' | '▏' | '▒');
        assert_eq!(
            colored_bar(0).chars().filter(|c| is_bar(*c)).count(),
            10,
            "score 0 should still render a 10-cell bar of empty glyphs"
        );
        // Score 100 fills all 9 full cells plus a trailing ▉ — visually
        // saturated, which the old 10-cell full-block bar also showed,
        // but distinct from score 95 (which lands on a different sub-cell).
        assert!(
            colored_bar(100).contains('█'),
            "score 100 should contain at least one full-block"
        );
        assert_ne!(
            colored_bar(95),
            colored_bar(100),
            "half-block sub-cells must distinguish 95 from 100"
        );
    }

    fn hour_score_at(date: NaiveDate, local_hour: u32, mean: u8) -> crate::aggregate::HourScore {
        // Build a UTC instant whose *local* hour is `local_hour` on `date`.
        // The hourly renderer matches columns by local clock hour, so the
        // tests must use local time when constructing fixtures.
        let local_dt = Local
            .from_local_datetime(&date.and_hms_opt(local_hour, 0, 0).unwrap())
            .single()
            .expect("local time should be unambiguous in these tests");
        crate::aggregate::HourScore {
            time: local_dt.with_timezone(&Utc),
            mean,
            min: mean,
            max: mean,
            breakdown: grusindeks_core::score::ScoreBreakdown {
                temperature: mean,
                wind: mean,
                precipitation: mean,
                precip_probability: mean,
                ground: mean,
            },
            confidence: Confidence::Hoy,
            raw: None,
        }
    }

    fn hourly_fixture(verbose_window: &[u8]) -> crate::aggregate::HourlyForecast {
        let date = Local::now().date_naive();
        // Score climbs linearly across the window so the test can assert
        // every glyph bucket appears at least once for a wide-enough window.
        let hours = verbose_window
            .iter()
            .enumerate()
            .map(|(i, &h)| hour_score_at(date, h.into(), 10 + (i as u8) * 8))
            .collect();
        let day = crate::aggregate::HourlyDayAggregate {
            date,
            daytime_window: RideWindow::from_hours(
                Local
                    .from_local_datetime(&date.and_hms_opt(verbose_window[0].into(), 0, 0).unwrap())
                    .single()
                    .unwrap()
                    .with_timezone(&Utc),
                verbose_window.len() as i64,
            ),
            hours,
            stats: None,
        };
        crate::aggregate::HourlyForecast {
            header_hours: verbose_window.to_vec(),
            days: vec![day],
            rain_history: None,
            nowcast_alert: None,
            sun: None,
        }
    }

    #[test]
    fn hourly_block_glyph_maps_buckets_at_boundaries() {
        // 5 score buckets, 4 distinct glyphs (`dårlig` and `marginalt`
        // share `░░` and lean on colour to differentiate). Boundaries here
        // mirror `theme::score_color`'s match arms — if those drift, this
        // test catches the divergence.
        assert_eq!(hourly_block_glyph(0), "░░");
        assert_eq!(hourly_block_glyph(24), "░░");
        assert_eq!(hourly_block_glyph(25), "░░");
        assert_eq!(hourly_block_glyph(44), "░░");
        assert_eq!(hourly_block_glyph(45), "▒▒");
        assert_eq!(hourly_block_glyph(64), "▒▒");
        assert_eq!(hourly_block_glyph(65), "▓▓");
        assert_eq!(hourly_block_glyph(84), "▓▓");
        assert_eq!(hourly_block_glyph(85), "██");
        assert_eq!(hourly_block_glyph(100), "██");
    }

    #[test]
    fn hourly_render_uses_block_glyphs_not_score_digits() {
        let forecast = hourly_fixture(&[10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21]);
        let out = render_hourly(
            "Oslo",
            20.0,
            &forecast,
            false,
            ChipFlags::default(),
            Language::Norwegian,
        );
        // Wide score range across the day → all four glyph variants should
        // surface somewhere in the output (header + cells + legend).
        assert!(out.contains("░░"), "expected ░░ glyph in {out}");
        assert!(out.contains("▒▒"), "expected ▒▒ glyph in {out}");
        assert!(out.contains("▓▓"), "expected ▓▓ glyph in {out}");
        assert!(out.contains("██"), "expected ██ glyph in {out}");
    }

    #[test]
    fn hourly_render_default_omits_breakdown_rows() {
        let forecast = hourly_fixture(&[10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21]);
        let out = render_hourly(
            "Oslo",
            20.0,
            &forecast,
            false,
            ChipFlags::default(),
            Language::Norwegian,
        );
        // Tree-branch glyphs only appear in the verbose breakdown — their
        // absence proves the default view stayed lean.
        assert!(!out.contains("├─"), "default should not draw tree: {out}");
        assert!(!out.contains("└─"), "default should not draw tree: {out}");
        // Axis labels also live only in verbose rows for hourly.
        assert!(
            !out.contains("Temp "),
            "default should hide axis labels: {out}"
        );
    }

    #[test]
    fn hourly_render_verbose_emits_four_breakdown_rows_per_day() {
        let forecast = hourly_fixture(&[10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21]);
        let out = render_hourly(
            "Oslo",
            20.0,
            &forecast,
            true,
            ChipFlags::default(),
            Language::Norwegian,
        );
        // Three intermediate branches and one closing branch per day.
        assert_eq!(
            out.matches("├─").count(),
            3,
            "expected 3 ├─ branches in {out}"
        );
        assert_eq!(
            out.matches("└─").count(),
            1,
            "expected 1 └─ branch in {out}"
        );
        // All four axis labels show up under the day.
        assert!(out.contains("Temp"), "missing Temp row: {out}");
        assert!(out.contains("Vind"), "missing Vind row: {out}");
        assert!(out.contains("Nedbør"), "missing Nedbør row: {out}");
        assert!(out.contains("Bakke"), "missing Bakke row: {out}");
    }

    #[test]
    fn hourly_render_verbose_breakdown_aligns_under_day_cells() {
        let forecast = hourly_fixture(&[10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21]);
        let out = render_hourly(
            "Oslo",
            20.0,
            &forecast,
            true,
            ChipFlags::default(),
            Language::Norwegian,
        );
        // A breakdown row's first cell must occupy the same *visible*
        // column as the day row's first cell. Colour helpers are no-ops
        // off-TTY, but `├─` is a 3-byte UTF-8 sequence whose visible
        // width is 1 col per char — so we measure with `unicode-width`,
        // not byte offsets.
        let lines: Vec<&str> = out.lines().collect();
        let day_idx = lines
            .iter()
            .position(|l| l.contains("i dag"))
            .expect("day row must exist");
        let temp_idx = lines
            .iter()
            .position(|l| l.contains("Temp"))
            .expect("Temp breakdown row must exist");
        let first_cell_col = |line: &str| {
            let byte_idx = line
                .char_indices()
                .find(|(_, c)| matches!(c, '░' | '▒' | '▓' | '█'))
                .map(|(i, _)| i)?;
            Some(UnicodeWidthStr::width(&line[..byte_idx]))
        };
        assert_eq!(
            first_cell_col(lines[day_idx]),
            first_cell_col(lines[temp_idx]),
            "breakdown cells must align under day cells:\n  day: {}\n temp: {}",
            lines[day_idx],
            lines[temp_idx],
        );
    }

    #[test]
    fn hourly_verbose_renders_per_day_tall_when_stats_present() {
        let mut forecast = hourly_fixture(&[10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21]);
        forecast.days[0].stats = Some(WindowStats {
            mean_temp_c: 14.0,
            felt_temp_c: 14.0,
            min_temp_c: 12.0,
            max_temp_c: 18.0,
            total_precip_mm: 0.0,
            max_hourly_precip_mm: 0.0,
            max_wind_ms: 4.0,
            max_gust_ms: Some(7.0),
            mean_humidity_pct: None,
            wind_from_deg: None,
        });
        let out = render_hourly(
            "Oslo",
            20.0,
            &forecast,
            true,
            ChipFlags::default(),
            Language::Norwegian,
        );
        assert!(
            out.contains("Tall"),
            "expected Tall row in hourly verbose: {out}"
        );
        assert!(
            out.contains("12–18 °C"),
            "expected temp range in Tall: {out}"
        );
        assert!(out.contains("(kast 7)"), "expected gust segment: {out}");
    }

    #[test]
    fn hourly_default_omits_per_day_tall_even_with_stats() {
        let mut forecast = hourly_fixture(&[10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21]);
        forecast.days[0].stats = Some(WindowStats {
            mean_temp_c: 14.0,
            felt_temp_c: 14.0,
            min_temp_c: 12.0,
            max_temp_c: 18.0,
            total_precip_mm: 0.0,
            max_hourly_precip_mm: 0.0,
            max_wind_ms: 4.0,
            max_gust_ms: None,
            mean_humidity_pct: None,
            wind_from_deg: None,
        });
        let out = render_hourly(
            "Oslo",
            20.0,
            &forecast,
            false,
            ChipFlags::default(),
            Language::Norwegian,
        );
        assert!(
            !out.contains("Tall"),
            "default hourly should not show Tall row: {out}"
        );
    }

    #[test]
    fn hourly_verbose_no_window_stats_flag_hides_tall() {
        let mut forecast = hourly_fixture(&[10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21]);
        forecast.days[0].stats = Some(WindowStats {
            mean_temp_c: 14.0,
            felt_temp_c: 14.0,
            min_temp_c: 12.0,
            max_temp_c: 18.0,
            total_precip_mm: 0.0,
            max_hourly_precip_mm: 0.0,
            max_wind_ms: 4.0,
            max_gust_ms: None,
            mean_humidity_pct: None,
            wind_from_deg: None,
        });
        let flags = ChipFlags {
            rain_history: true,
            window_stats: false,
        };
        let out = render_hourly("Oslo", 20.0, &forecast, true, flags, Language::Norwegian);
        assert!(
            !out.contains("Tall"),
            "flag should suppress Tall in hourly verbose: {out}"
        );
    }

    #[test]
    fn hourly_render_handles_empty_forecast() {
        let forecast = crate::aggregate::HourlyForecast {
            header_hours: vec![],
            days: vec![],
            rain_history: None,
            nowcast_alert: None,
            sun: None,
        };
        let out = render_hourly(
            "Oslo",
            20.0,
            &forecast,
            false,
            ChipFlags::default(),
            Language::Norwegian,
        );
        assert!(
            out.contains("Ingen timer"),
            "expected empty-window message: {out}"
        );
    }
}
