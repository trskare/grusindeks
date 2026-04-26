//! Human-readable formatting of an `AggregateScore` / `MultiDayForecast`.
//!
//! Pure functions that return `String`s — easy to insta-snapshot, easy to
//! reason about. All colour goes through `theme::*` helpers, which silently
//! degrade to plain text when stdout isn't a TTY (covers tests, pipes, and
//! `NO_COLOR`).

use std::fmt::Write as _;

use chrono::{DateTime, Local, NaiveDate, Utc};
use grusindeks_core::daily::Confidence;
use grusindeks_core::lang::Language;
use grusindeks_core::score::Penalty;
use grusindeks_core::types::RideWindow;
use unicode_width::UnicodeWidthStr;

use crate::aggregate::{AggregateScore, DayAggregate, MultiDayForecast};
use crate::theme;

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
    let (precip_combined, precip_detail) = combined_precip(agg_precip, agg_prob, lang);
    write_axis_row(
        &mut out,
        axis_label_long("precipitation", lang),
        precip_combined,
        &precip_detail,
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

    if agg.points.len() > 1 {
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
            localize_bearing(worst.bearing_label, lang),
        );
        let _ = writeln!(
            out,
            "{best_word}  {} ({})",
            theme::paint_score(best.score.total),
            localize_bearing(best.bearing_label, lang),
        );
    }
    out
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
    let title = format!(
        "Grusindeks · {label} · {radius_km:.0} km · {n} {day_word}"
    );
    let _ = writeln!(out, "{}", theme::paint_accent(&title));
    let _ = writeln!(out);

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
        write_day_row(&mut out, day, today_local, verbose, lang);
    }

    let _ = writeln!(out);
    write_footer(&mut out, forecast, verbose, any_low_confidence, lang);
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
    let star_painted = mid.replacen(
        '★',
        &theme::paint_score_soft("★", best.mean),
        1,
    );
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
    lang: Language,
) {
    if let Some(msg) = ground_chip(forecast) {
        let label = match lang {
            Language::Norwegian => "Bakke",
            Language::Swedish => "Mark",
        };
        let _ = writeln!(out, "  {}   {msg}", theme::paint_dim(label));
    }

    let scale_label = match lang {
        Language::Norwegian => "Skala",
        Language::Swedish => "Skala",
    };
    let _ = writeln!(out, "  {}   {}", theme::paint_dim(scale_label), bucket_legend(lang));

    if any_low_confidence {
        let note = match lang {
            Language::Norwegian => "~       lav konfidens (>60 t — 6-t oppløsning fra MET)",
            Language::Swedish => "~       låg tillförlitlighet (>60 h — 6-h upplösning från MET)",
        };
        let _ = writeln!(out, "  {}", theme::paint_dim(note));
    }
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
    Some(penalty.message_no.clone())
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
    lang: Language,
) {
    let center = day.center();
    let day_label = day_label(day.date, today_local, lang);
    let icon = center.weather_icon;

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
    if is_today || verbose {
        write_day_breakdown(out, day, dim, verbose && penalty_take > 0, lang);
    }

    // "Beste luke" — a sub-day window that scores significantly higher
    // than the day's mean. The score layer only emits an `optimal_window`
    // when the improvement clears its threshold, so rendering it
    // whenever it's `Some` is correct. Surface it in default mode too:
    // knowing "today's mean is 70 but 06–09 is 95" is one of the
    // highest-value signals the renderer can show.
    if let Some(ow) = &center.optimal_window {
        let (luke_phrase, points_word) = match lang {
            Language::Norwegian => ("Beste luke", "poeng"),
            Language::Swedish => ("Bästa lucka", "poäng"),
        };
        let _ = writeln!(
            out,
            "{BREAKDOWN_INDENT}★ {luke_phrase}: {}–{} → {} ({}, +{} {})",
            local_hm(ow.window.start),
            local_hm(ow.window.end),
            theme::paint_score(ow.score.total),
            theme::paint_label(mean_label(ow.score.total, lang), ow.score.total),
            ow.improvement,
            points_word,
        );
    }

    if !verbose {
        return;
    }

    // Verbose-only: full penalty list under each day.
    for (i, penalty) in center.score.penalties.iter().take(penalty_take).enumerate() {
        let is_last = i + 1 == penalty_take;
        let branch = if is_last { "└─" } else { "├─" };
        let _ = writeln!(
            out,
            "{BREAKDOWN_INDENT}{branch} {}",
            format_penalty_line(penalty, lang)
        );
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
    // Capture the precip detail (`(mengde X, sjanse Y)`) so we can append
    // it to the Nedbør row when amount and probability diverge — that's
    // exactly the case where the combined number alone is misleading
    // (e.g. 0 mm forecast but 50 % chance lands at ~85, not 100).
    let (precip_combined, precip_detail) = combined_precip(precip, prob, lang);

    let (temp_label, wind_label, precip_label, ground_label) = match lang {
        Language::Norwegian => ("Temp", "Vind", "Nedbør", "Bakke"),
        Language::Swedish => ("Temp", "Vind", "Nederbörd", "Mark"),
    };
    let rows: [(&str, u8, &str); 4] = [
        (temp_label, temp, ""),
        (wind_label, wind, ""),
        (precip_label, precip_combined, precip_detail.as_str()),
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
        theme::paint_severity(&p.message_no, p.severity),
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

/// Combine the precipitation amount and probability sub-scores into one
/// number plus an optional ` (amount X, chance Y)` suffix in the
/// requested language.
fn combined_precip(amount: u8, probability: u8, lang: Language) -> (u8, String) {
    use grusindeks_core::score::thresholds::{W_PRECIP, W_PROB};
    let w_sum = u32::from(W_PRECIP) + u32::from(W_PROB);
    let combined = ((u32::from(amount) * u32::from(W_PRECIP)
        + u32::from(probability) * u32::from(W_PROB))
        / w_sum) as u8;
    let detail = if amount.abs_diff(probability) > 5 {
        match lang {
            Language::Norwegian => format!("  (mengde {amount}, sjanse {probability})"),
            Language::Swedish => format!("  (mängd {amount}, chans {probability})"),
        }
    } else {
        String::new()
    };
    (combined, detail)
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
        .find(|p| p.bearing_label == "senter")
        .unwrap_or(&agg.points[0]);
    &center.score.penalties
}

fn mean_label(total: u8, lang: Language) -> &'static str {
    grusindeks_core::score::label_for(total, lang)
}

/// Translate the Norwegian bearing/center label stored in the
/// `PointScore` (NØ, SØ, …, "senter") to the localized form. The data
/// pipeline produces Norwegian labels so we can keep `"senter"` as an
/// internal sentinel for center detection; this helper bridges to the
/// renderer's chosen language.
fn localize_bearing<'a>(label_no: &'a str, lang: Language) -> std::borrow::Cow<'a, str> {
    use std::borrow::Cow;
    match (lang, label_no) {
        (Language::Swedish, "senter") => Cow::Borrowed("centrum"),
        (Language::Swedish, _) if label_no.contains('Ø') => Cow::Owned(label_no.replace('Ø', "Ö")),
        _ => Cow::Borrowed(label_no),
    }
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
            probability_of_precip: Some(5.0),
            ..HourlyConditions::minimal(t(time_h), 17.0, 2.0, 0.0)
        }
    }

    #[test]
    fn human_output_includes_total_and_breakdown_labels() {
        let win = RideWindow::from_hours(t(14), 3);
        let s = score(
            &(14..17).map(perfect).collect::<Vec<_>>(),
            win,
            SurfaceState::default(),
            Language::Norwegian,
        );
        let center = Point::new(59.9139, 10.7522);
        let agg = AggregateScore::from_points(center, vec![(center, s)]);
        let out = render_human("Oslo", 20.0, win, &agg, false, Language::Norwegian);
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
        let s = score(&hours, win, SurfaceState::default(), Language::Norwegian);
        let center = Point::new(59.9139, 10.7522);
        let agg = AggregateScore::from_points(center, vec![(center, s)]);
        let out = render_human("Oslo", 20.0, win, &agg, false, Language::Norwegian);
        // Component label + a number from the wind speed should be present.
        assert!(
            out.contains("Vind") && out.contains("9"),
            "expected wind penalty in {out}"
        );
    }

    #[test]
    fn combined_precip_hides_breakdown_when_amount_and_probability_agree() {
        let (combined, detail) = combined_precip(100, 100, Language::Norwegian);
        assert_eq!(combined, 100);
        assert!(detail.is_empty(), "got {detail:?}");
    }

    #[test]
    fn combined_precip_shows_breakdown_when_amount_and_probability_diverge() {
        let (combined, detail) = combined_precip(100, 40, Language::Norwegian);
        assert!(detail.contains("mengde 100"), "got {detail:?}");
        assert!(detail.contains("sjanse 40"), "got {detail:?}");
        assert_eq!(combined, 82);
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
                    SurfaceState::default(),
                    now,
                    Language::Norwegian,
                ),
            )],
        );
        let forecast = MultiDayForecast { days: vec![day] };
        let out = render_multi_day("Oslo", 20.0, &forecast, false, Language::Norwegian);
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
                    SurfaceState::default(),
                    now,
                    Language::Norwegian,
                ),
            )],
        );
        let forecast = MultiDayForecast { days: vec![day] };
        let out = render_multi_day("Oslo", 20.0, &forecast, false, Language::Norwegian);
        // The "best day" callout uses the short prefix "Beste:" inside a
        // round-cornered box, plus the "★" star in front. Either pin
        // suffices to confirm the headline rendered.
        assert!(
            out.contains("★ Beste:") && out.contains("╭"),
            "expected best-day callout in {out}"
        );
    }

    #[test]
    fn multi_day_render_calls_out_optimal_luke_when_present() {
        use crate::aggregate::DayAggregate;
        use grusindeks_core::daily::compute_day;
        use grusindeks_core::types::HourlyConditions;

        fn awful(time_h: u32) -> HourlyConditions {
            HourlyConditions {
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
                    SurfaceState::default(),
                    now,
                    Language::Norwegian,
                ),
            )],
        );
        let forecast = MultiDayForecast { days: vec![day] };
        // Verbose surfaces "Beste luke" under days with a usable
        // optimal window. Default mode keeps the per-day rows lean and
        // the luke callout lives only in --verbose now.
        let out = render_multi_day("Oslo", 20.0, &forecast, true, Language::Norwegian);
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
                    SurfaceState::new(5.0),
                    now,
                    Language::Norwegian,
                ),
            )],
        );
        let forecast = MultiDayForecast { days: vec![day] };
        let out = render_multi_day("Oslo", 20.0, &forecast, false, Language::Norwegian);
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
        let verbose = render_multi_day("Oslo", 20.0, &forecast, true, Language::Norwegian);
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
                wind_gust_ms: Some(22.0),
                cloud_area_fraction: Some(95.0),
                probability_of_precip: Some(40.0),
                ..HourlyConditions::minimal(t(h), 9.0, 16.0, 0.5)
            })
            .collect();
        let s = score(&hours, win, SurfaceState::new(3.0), Language::Norwegian);
        let center = Point::new(59.9139, 10.7522);
        let agg = AggregateScore::from_points(center, vec![(center, s)]);
        eprintln!(
            "\n--- DEFAULT ---\n{}",
            render_human("Oslo", 20.0, win, &agg, false, Language::Norwegian)
        );
        eprintln!(
            "--- VERBOSE ---\n{}",
            render_human("Oslo", 20.0, win, &agg, true, Language::Norwegian)
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
                wind_gust_ms: Some(13.0),
                cloud_area_fraction: Some(80.0),
                ..HourlyConditions::minimal(at(time_h, day_offset), 6.0, 9.0, 0.0)
            }
        }
        fn rainy(time_h: u32, day_offset: i64) -> HourlyConditions {
            HourlyConditions {
                probability_of_precip: Some(85.0),
                cloud_area_fraction: Some(95.0),
                ..HourlyConditions::minimal(at(time_h, day_offset), 8.0, 4.0, 1.5)
            }
        }
        fn nice(time_h: u32, day_offset: i64) -> HourlyConditions {
            HourlyConditions {
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
                        SurfaceState::new(1.0),
                        now,
                        Language::Norwegian,
                    ),
                )],
            );
            days.push(day);
        }
        let forecast = MultiDayForecast { days };
        eprintln!(
            "\n--- DEFAULT ---\n{}",
            render_multi_day("Oslo", 20.0, &forecast, false, Language::Norwegian)
        );
        eprintln!(
            "--- VERBOSE ---\n{}",
            render_multi_day("Oslo", 20.0, &forecast, true, Language::Norwegian)
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
                    SurfaceState::new(5.0),
                    now,
                    Language::Norwegian,
                ),
            )],
        );
        let forecast = MultiDayForecast { days: vec![day] };
        let default_out = render_multi_day("Oslo", 20.0, &forecast, false, Language::Norwegian);
        let verbose_out = render_multi_day("Oslo", 20.0, &forecast, true, Language::Norwegian);
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
                    SurfaceState::default(),
                    now,
                    Language::Norwegian,
                ),
            )],
        );
        MultiDayForecast { days: vec![day] }
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
            SurfaceState::default(),
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
            weather_icon: "☀",
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
                bearing_label: "senter",
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
        let out = render_multi_day("Oslo", 20.0, &forecast, false, Language::Norwegian);
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
        let out = render_multi_day("Oslo", 20.0, &forecast, false, Language::Norwegian);
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
            Language::Norwegian,
        ));
        let verbose_chips = count(&render_multi_day(
            "Oslo",
            20.0,
            &forecast,
            true,
            Language::Norwegian,
        ));
        // Default: today only → 1 "Temp " hit. Verbose: today + tomorrow → 2.
        assert!(
            verbose_chips > default_chips,
            "verbose should add breakdown rows: default={default_chips}, verbose={verbose_chips}"
        );
    }

    #[test]
    fn colored_bar_glyph_counts() {
        // Colour helpers degrade to plain text outside a TTY (test runner
        // is not a TTY), so the byte content is just the bar glyphs. The
        // half-block ramp `▏▎▍▌▋▊▉` lives in U+258F..U+2589, so we count
        // bar glyphs by checking the script range rather than enumerating.
        let is_bar =
            |c: char| matches!(c, '█' | '▉' | '▊' | '▋' | '▌' | '▍' | '▎' | '▏' | '▒');
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
}
