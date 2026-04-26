//! Human-readable formatting of an `AggregateScore` / `MultiDayForecast`.
//!
//! Pure functions that return `String`s — easy to insta-snapshot, easy to
//! reason about. All colour goes through `theme::*` helpers, which silently
//! degrade to plain text when stdout isn't a TTY (covers tests, pipes, and
//! `NO_COLOR`).

use std::fmt::Write as _;

use chrono::{DateTime, Local, NaiveDate, Utc};
use medvind_core::daily::Confidence;
use medvind_core::score::Penalty;
use medvind_core::types::RideWindow;

use crate::aggregate::{AggregateScore, DayAggregate, MultiDayForecast};
use crate::theme;

const BAR_WIDTH: usize = 10;

/// Bar with the filled portion painted in the score colour and the empty
/// portion dimmed. Returns the styled string ready to drop into a row.
fn colored_bar(value: u8) -> String {
    let filled_count = (usize::from(value) * BAR_WIDTH + 50) / 100;
    let filled_count = filled_count.min(BAR_WIDTH);
    let filled: String = "█".repeat(filled_count);
    let empty: String = "░".repeat(BAR_WIDTH - filled_count);
    format!(
        "{}{}",
        theme::paint_bar_filled(&filled, value),
        theme::paint_bar_empty(&empty)
    )
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
    let _ = writeln!(
        out,
        "Total: {}/100  ⭐ {}",
        theme::paint_score(agg.mean),
        theme::paint_label(mean_label(agg.mean), agg.mean),
    );
    let _ = writeln!(out);

    // Per-axis bars (means across points). Same five-axis layout as before
    // — the new piece is colour, plus penalty lines below.
    let agg_temp = avg_axis(agg, |b| b.temperature);
    let agg_wind = avg_axis(agg, |b| b.wind);
    let agg_precip = avg_axis(agg, |b| b.precipitation);
    let agg_prob = avg_axis(agg, |b| b.precip_probability);
    let agg_ground = avg_axis(agg, |b| b.ground);

    write_axis_row(&mut out, "Temperatur", agg_temp, "");
    write_axis_row(&mut out, "Vind", agg_wind, "");
    let (precip_combined, precip_detail) = combined_precip(agg_precip, agg_prob);
    write_axis_row(&mut out, "Nedbør", precip_combined, &precip_detail);
    write_axis_row(&mut out, "Bakke", agg_ground, "");

    // Penalty list — pulled from the center point. Default caps at the
    // top three so the output stays scannable; `--verbose` lists all.
    let penalties = center_penalties(agg);
    if !penalties.is_empty() {
        let _ = writeln!(out);
        let cap = if verbose { penalties.len() } else { 3 };
        for p in penalties.iter().take(cap) {
            let _ = writeln!(out, "  {}", format_penalty_line(p));
        }
    }

    if agg.points.len() > 1 {
        let _ = writeln!(out);
        let worst = agg.worst();
        let best = agg.best();
        let _ = writeln!(
            out,
            "Verste punkt:  {} ({})",
            theme::paint_score(worst.score.total),
            worst.bearing_label,
        );
        let _ = writeln!(
            out,
            "Beste punkt:   {} ({})",
            theme::paint_score(best.score.total),
            best.bearing_label,
        );
    }
    out
}

/// Render the multi-day forecast: a "best day" headline at the top, then
/// one row per day with weather emoji, coloured bar, score, label, and
/// confidence. Default mode shows each day's worst penalty on a `└─`
/// line directly below; `verbose` shows every penalty for that day.
pub fn render_multi_day(
    label: &str,
    radius_km: f64,
    forecast: &MultiDayForecast,
    verbose: bool,
) -> String {
    let mut out = String::new();
    let n = forecast.days.len();
    let header = format!(
        "Grusindeks for {label} ({radius_km:.0}km radius) — {n} {}",
        if n == 1 { "dag" } else { "dager" }
    );
    let _ = writeln!(out, "{}", theme::paint_accent(&header));
    let _ = writeln!(out, "{}", theme::paint_dim(&"═".repeat(63)));

    let today_local: NaiveDate = Local::now().date_naive();

    // Headline: the best day in the forecast. Always rendered (even on a
    // bad week) — the user explicitly asked for a permanent summary line.
    if let Some(best) = forecast.days.iter().max_by_key(|d| d.mean) {
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "🎯 Beste dag: {}  ·  {}/100  {}",
            day_label_no(best.date, today_local),
            theme::paint_score(best.mean),
            theme::paint_label(mean_label(best.mean), best.mean),
        );
        if let Some(ow) = &best.optimal_window {
            let _ = writeln!(
                out,
                "   Beste luke: {}–{} → {}/100  ({})",
                local_hm(ow.window.start),
                local_hm(ow.window.end),
                theme::paint_score(ow.score.total),
                theme::paint_label(mean_label(ow.score.total), ow.score.total),
            );
        }
        let _ = writeln!(out);
    }

    let _ = writeln!(out, "{}", theme::paint_dim(&"─".repeat(63)));

    let mut any_low_confidence = false;
    for day in &forecast.days {
        if day.confidence == Confidence::Lav {
            any_low_confidence = true;
        }
        write_day_row(&mut out, day, today_local, verbose);
    }

    if any_low_confidence {
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "{}",
            theme::paint_dim(
                "Konfidens faller etter ca. 60 t — siste dager er 6-t oppløsning fra MET."
            )
        );
    }
    out
}

/// Render one day's row plus its top penalty (if any). Low-confidence
/// rows are dimmed so the eye lands on the trustworthy near-term days.
/// `verbose` lists every penalty for the day instead of just the worst.
fn write_day_row(
    out: &mut String,
    day: &DayAggregate,
    today_local: NaiveDate,
    verbose: bool,
) {
    let center = day.center();
    let day_label = day_label_no(day.date, today_local);
    let icon = center.weather_icon;
    let label = mean_label(day.mean);
    let confidence_label = day.confidence.label_no();

    let dim = theme::dim_for_confidence(day.confidence);

    // Score gets right-aligned to 3 cells *before* painting, so dim and
    // non-dim rows align identically.
    let score_str = format!("{:>3}", day.mean);
    let score_p = if dim {
        theme::paint_dim(&score_str)
    } else {
        theme::paint_score_str(&score_str, day.mean)
    };

    // Label and day-label go through `pad_right` after painting, since the
    // colour escape codes inflate the byte length without adding cells.
    // We must NOT pre-pad before painting or the visible width doubles.
    let day_label_p = if dim {
        theme::paint_dim(&day_label)
    } else {
        theme::paint_fg(&day_label)
    };
    let label_p = if dim {
        theme::paint_dim(label)
    } else {
        theme::paint_label(label, day.mean)
    };
    let conf_col = format!("ⓘ {confidence_label}");
    let conf_p = theme::paint_dim(&conf_col);

    let _ = writeln!(
        out,
        "{} {}{}{} {}  {} {}",
        pad_right(&day_label_p, &day_label, 12),
        icon,
        icon_pad(icon),
        colored_bar(day.mean),
        score_p,
        pad_right(&label_p, label, 10),
        conf_p,
    );

    // Penalty line(s): default surfaces the worst one (HardCap is always
    // Critical and sorts first when present); --verbose lists every one.
    let take = if verbose {
        center.score.penalties.len()
    } else {
        center.score.penalties.len().min(1)
    };
    for penalty in center.score.penalties.iter().take(take) {
        let _ = writeln!(out, "             └─ {}", format_penalty_line(penalty));
    }

    // "Best luke" hint — only on days where the optimal window improves
    // by enough to clear the threshold (caller already filtered).
    if let Some(ow) = &center.optimal_window {
        let _ = writeln!(
            out,
            "             🎯 Beste luke: {}–{} → {} ({}, +{} poeng)",
            local_hm(ow.window.start),
            local_hm(ow.window.end),
            theme::paint_score(ow.score.total),
            theme::paint_label(mean_label(ow.score.total), ow.score.total),
            ow.improvement,
        );
    }
}

/// Format a single penalty line: "Vind: snitt 8.2 m/s" with the
/// component label coloured by component and the message dimmed.
fn format_penalty_line(p: &Penalty) -> String {
    format!(
        "{}: {}",
        theme::paint_component_label(p.component),
        theme::paint_severity(&p.message_no, p.severity),
    )
}

/// Width-padding for the weather emoji column so single-cell glyphs
/// (☀ ☁ ·) and double-cell glyphs (🌧 🌨 🌬 ⛅ 🎯) leave the same gap
/// before the bar. Hand-tuned because `unicode-width` doesn't agree
/// with what most terminals actually render for these characters.
fn icon_pad(icon: &str) -> &'static str {
    match icon {
        "☀" | "☁" | "·" => "  ",
        _ => " ",
    }
}

/// Norwegian-friendly day label. Uses "i dag" / "i morgen" for the
/// nearest two days and a short weekday + date otherwise.
fn day_label_no(date: NaiveDate, today: NaiveDate) -> String {
    use chrono::Datelike;
    let delta = (date - today).num_days();
    match delta {
        0 => "i dag".to_string(),
        1 => "i morgen".to_string(),
        _ => format!(
            "{} {}. {}",
            weekday_no(date),
            date.day(),
            month_no(date.month()),
        ),
    }
}

/// Two-letter Norwegian weekday abbreviation: ma/ti/on/to/fr/lø/sø.
fn weekday_no(date: NaiveDate) -> &'static str {
    use chrono::Datelike;
    match date.weekday() {
        chrono::Weekday::Mon => "ma",
        chrono::Weekday::Tue => "ti",
        chrono::Weekday::Wed => "on",
        chrono::Weekday::Thu => "to",
        chrono::Weekday::Fri => "fr",
        chrono::Weekday::Sat => "lø",
        chrono::Weekday::Sun => "sø",
    }
}

/// Three-letter Norwegian month abbreviation. Norwegian doesn't capitalise
/// month names — `chrono`'s `%b` does, so we render our own.
fn month_no(month: u32) -> &'static str {
    match month {
        1 => "jan",
        2 => "feb",
        3 => "mar",
        4 => "apr",
        5 => "mai",
        6 => "jun",
        7 => "jul",
        8 => "aug",
        9 => "sep",
        10 => "okt",
        11 => "nov",
        12 => "des",
        _ => "?",
    }
}

/// Combine the precipitation amount and probability sub-scores into one
/// number plus an optional ` (mengde X, sjanse Y)` suffix.
fn combined_precip(amount: u8, probability: u8) -> (u8, String) {
    use medvind_core::score::thresholds::{W_PRECIP, W_PROB};
    let w_sum = u32::from(W_PRECIP) + u32::from(W_PROB);
    let combined = ((u32::from(amount) * u32::from(W_PRECIP)
        + u32::from(probability) * u32::from(W_PROB))
        / w_sum) as u8;
    let detail = if amount.abs_diff(probability) > 5 {
        format!("  (mengde {amount}, sjanse {probability})")
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
    F: Fn(&medvind_core::score::ScoreBreakdown) -> u8,
{
    let n = agg.points.len() as u32;
    if n == 0 {
        return 0;
    }
    let sum: u32 = agg.points.iter().map(|p| u32::from(f(&p.score.breakdown))).sum();
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

fn mean_label(total: u8) -> &'static str {
    medvind_core::score::label_for(total)
}

/// Pad a *coloured* string to `target_visible_width`. We can't trust the
/// formatter's `{:<N}` since ANSI escape codes inflate the length without
/// adding visible cells. The plain `visible` slice is what the eye sees.
fn pad_right(coloured: &str, visible: &str, target_visible_width: usize) -> String {
    let visible_len = visible.chars().count();
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
    use medvind_core::geo::Point;
    use medvind_core::score::score;
    use medvind_core::types::HourlyConditions;

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
        let s = score(&(14..17).map(perfect).collect::<Vec<_>>(), win, 0.0);
        let center = Point::new(59.9139, 10.7522);
        let agg = AggregateScore::from_points(center, vec![(center, s)]);
        let out = render_human("Oslo", 20.0, win, &agg, false);
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
        let s = score(&hours, win, 0.0);
        let center = Point::new(59.9139, 10.7522);
        let agg = AggregateScore::from_points(center, vec![(center, s)]);
        let out = render_human("Oslo", 20.0, win, &agg, false);
        // Component label + a number from the wind speed should be present.
        assert!(
            out.contains("Vind") && out.contains("9"),
            "expected wind penalty in {out}"
        );
    }

    #[test]
    fn combined_precip_hides_breakdown_when_amount_and_probability_agree() {
        let (combined, detail) = combined_precip(100, 100);
        assert_eq!(combined, 100);
        assert!(detail.is_empty(), "got {detail:?}");
    }

    #[test]
    fn combined_precip_shows_breakdown_when_amount_and_probability_diverge() {
        let (combined, detail) = combined_precip(100, 40);
        assert!(detail.contains("mengde 100"), "got {detail:?}");
        assert!(detail.contains("sjanse 40"), "got {detail:?}");
        assert_eq!(combined, 82);
    }

    #[test]
    fn day_label_for_today_and_tomorrow_uses_norwegian_phrasing() {
        let today = NaiveDate::from_ymd_opt(2026, 4, 26).unwrap();
        assert_eq!(day_label_no(today, today), "i dag");
        assert_eq!(
            day_label_no(today + chrono::Duration::days(1), today),
            "i morgen"
        );
    }

    #[test]
    fn day_label_for_distant_day_uses_weekday_and_date() {
        let today = NaiveDate::from_ymd_opt(2026, 4, 26).unwrap(); // Sunday
        let in_three = today + chrono::Duration::days(3); // Wednesday
        let label = day_label_no(in_three, today);
        assert!(label.starts_with("on "), "got {label}");
        assert!(label.contains("29"), "got {label}");
        assert!(label.ends_with("apr"), "got {label}");
    }

    #[test]
    fn multi_day_render_includes_per_day_rows_and_confidence_label() {
        use crate::aggregate::DayAggregate;
        use medvind_core::daily::compute_day;

        let win = RideWindow::from_hours(t(6), 12);
        let now = t(5);
        let center = Point::new(59.9139, 10.7522);
        let hours: Vec<_> = (6..18).map(perfect).collect();
        let day = DayAggregate::from_points(
            NaiveDate::from_ymd_opt(2026, 4, 26).unwrap(),
            win,
            center,
            vec![(center, compute_day(&hours, win, 0.0, now))],
        );
        let forecast = MultiDayForecast { days: vec![day] };
        let out = render_multi_day("Oslo", 20.0, &forecast, false);
        assert!(out.contains("Grusindeks for Oslo"), "got {out}");
        assert!(out.contains("dag"), "got {out}");
        assert!(out.contains("ⓘ"), "missing confidence glyph: {out}");
    }

    #[test]
    fn multi_day_render_shows_headline_with_best_day() {
        use crate::aggregate::DayAggregate;
        use medvind_core::daily::compute_day;

        let win = RideWindow::from_hours(t(6), 12);
        let now = t(5);
        let center = Point::new(59.9139, 10.7522);
        let hours: Vec<_> = (6..18).map(perfect).collect();
        let day = DayAggregate::from_points(
            NaiveDate::from_ymd_opt(2026, 4, 26).unwrap(),
            win,
            center,
            vec![(center, compute_day(&hours, win, 0.0, now))],
        );
        let forecast = MultiDayForecast { days: vec![day] };
        let out = render_multi_day("Oslo", 20.0, &forecast, false);
        assert!(
            out.contains("Beste dag"),
            "expected headline 'Beste dag' in {out}"
        );
    }

    #[test]
    fn multi_day_render_calls_out_optimal_luke_when_present() {
        use crate::aggregate::DayAggregate;
        use medvind_core::daily::compute_day;
        use medvind_core::types::HourlyConditions;

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
            vec![(center, compute_day(&hours, win, 0.0, now))],
        );
        let forecast = MultiDayForecast { days: vec![day] };
        let out = render_multi_day("Oslo", 20.0, &forecast, false);
        assert!(
            out.contains("Beste luke") || out.contains("luke"),
            "expected luke callout in {out}"
        );
    }

    #[test]
    fn multi_day_render_shows_top_penalty_under_each_day() {
        use crate::aggregate::DayAggregate;
        use medvind_core::daily::compute_day;

        // Saturated ground → ground penalty should appear under the row.
        let win = RideWindow::from_hours(t(6), 12);
        let now = t(5);
        let center = Point::new(59.9139, 10.7522);
        let hours: Vec<_> = (6..18).map(perfect).collect();
        let day = DayAggregate::from_points(
            NaiveDate::from_ymd_opt(2026, 4, 26).unwrap(),
            win,
            center,
            vec![(center, compute_day(&hours, win, 5.0, now))],
        );
        let forecast = MultiDayForecast { days: vec![day] };
        let out = render_multi_day("Oslo", 20.0, &forecast, false);
        assert!(out.contains("└─"), "expected penalty marker in {out}");
        assert!(
            out.to_lowercase().contains("bakke"),
            "expected ground penalty in {out}"
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
        let s = score(&hours, win, 3.0);
        let center = Point::new(59.9139, 10.7522);
        let agg = AggregateScore::from_points(center, vec![(center, s)]);
        eprintln!("\n--- DEFAULT ---\n{}", render_human("Oslo", 20.0, win, &agg, false));
        eprintln!("--- VERBOSE ---\n{}", render_human("Oslo", 20.0, win, &agg, true));
    }

    #[test]
    #[ignore = "demo: run with --ignored --nocapture to inspect the rendered multi-day output"]
    fn demo_render_multi_day_with_mixed_weather() {
        use crate::aggregate::DayAggregate;
        use medvind_core::daily::compute_day;

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
                vec![(center, compute_day(&hours, win, 1.0, now))],
            );
            days.push(day);
        }
        let forecast = MultiDayForecast { days };
        eprintln!("\n--- DEFAULT ---\n{}", render_multi_day("Oslo", 20.0, &forecast, false));
        eprintln!("--- VERBOSE ---\n{}", render_multi_day("Oslo", 20.0, &forecast, true));
    }

    #[test]
    fn verbose_multi_day_lists_more_penalties_than_default() {
        use crate::aggregate::DayAggregate;
        use medvind_core::daily::compute_day;

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
            vec![(center, compute_day(&hours, win, 5.0, now))],
        );
        let forecast = MultiDayForecast { days: vec![day] };
        let default_out = render_multi_day("Oslo", 20.0, &forecast, false);
        let verbose_out = render_multi_day("Oslo", 20.0, &forecast, true);
        let count = |s: &str| s.matches("└─").count();
        assert!(
            count(&verbose_out) > count(&default_out),
            "verbose should add penalty rows: default={}, verbose={}\n--- default ---\n{default_out}\n--- verbose ---\n{verbose_out}",
            count(&default_out),
            count(&verbose_out),
        );
    }

    #[test]
    fn colored_bar_glyph_counts() {
        // Colour helpers degrade to plain text outside a TTY (test runner
        // is not a TTY), so the byte content is just the █/░ sequence.
        assert_eq!(colored_bar(0).chars().filter(|c| *c == '█').count(), 0);
        assert_eq!(colored_bar(100).chars().filter(|c| *c == '█').count(), BAR_WIDTH);
        assert_eq!(colored_bar(50).chars().filter(|c| *c == '█').count(), 5);
    }
}
