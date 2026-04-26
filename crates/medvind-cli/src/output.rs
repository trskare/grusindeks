//! Human-readable formatting of an `AggregateScore`. Pure functions that
//! return `String`s — easy to insta-snapshot.

use std::fmt::Write as _;

use chrono::{DateTime, Local, NaiveDate, Utc};
use medvind_core::daily::Confidence;
use medvind_core::types::RideWindow;

use crate::aggregate::{AggregateScore, MultiDayForecast};

const BAR_WIDTH: usize = 10;

fn bar(value: u8) -> String {
    let filled = (usize::from(value) * BAR_WIDTH + 50) / 100;
    let filled = filled.min(BAR_WIDTH);
    let mut s = String::with_capacity(BAR_WIDTH);
    for _ in 0..filled {
        s.push('█');
    }
    for _ in filled..BAR_WIDTH {
        s.push('░');
    }
    s
}

fn local_hm(t: DateTime<Utc>) -> String {
    t.with_timezone(&Local).format("%H:%M").to_string()
}

fn local_date(t: DateTime<Utc>) -> String {
    t.with_timezone(&Local).format("%Y-%m-%d").to_string()
}

/// Render the aggregate score in a human-readable format suitable for a
/// terminal (Norwegian, with bars and a worst/best summary line).
pub fn render_human(
    label: &str,
    radius_km: f64,
    window: RideWindow,
    agg: &AggregateScore,
) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "Grusindeks for {label} ({radius_km:.0}km radius) — {} {}–{}",
        local_date(window.start),
        local_hm(window.start),
        local_hm(window.end),
    );
    let _ = writeln!(
        out,
        "─────────────────────────────────────────────────────────"
    );
    let _ = writeln!(out, "Total: {}/100  ⭐ {}", agg.mean, mean_label(agg.mean));
    let _ = writeln!(out);

    // Aggregate breakdown is built from the mean of each sub-score across
    // points (not the mean of totals — that's already on the Total line).
    let n = agg.points.len() as u32;
    let agg_temp = avg(
        agg.points
            .iter()
            .map(|p| u32::from(p.score.breakdown.temperature))
            .sum::<u32>(),
        n,
    );
    let agg_wind = avg(
        agg.points
            .iter()
            .map(|p| u32::from(p.score.breakdown.wind))
            .sum::<u32>(),
        n,
    );
    let agg_precip = avg(
        agg.points
            .iter()
            .map(|p| u32::from(p.score.breakdown.precipitation))
            .sum::<u32>(),
        n,
    );
    let agg_prob = avg(
        agg.points
            .iter()
            .map(|p| u32::from(p.score.breakdown.precip_probability))
            .sum::<u32>(),
        n,
    );
    let agg_ground = avg(
        agg.points
            .iter()
            .map(|p| u32::from(p.score.breakdown.ground))
            .sum::<u32>(),
        n,
    );

    let _ = writeln!(out, "Temperatur     {} {:>3}", bar(agg_temp), agg_temp);
    let _ = writeln!(out, "Vind           {} {:>3}", bar(agg_wind), agg_wind);
    let (precip_combined, precip_detail) = combined_precip(agg_precip, agg_prob);
    let _ = writeln!(
        out,
        "Nedbør         {} {:>3}{}",
        bar(precip_combined),
        precip_combined,
        precip_detail,
    );
    let _ = writeln!(out, "Bakke          {} {:>3}", bar(agg_ground), agg_ground);

    if agg.points.len() > 1 {
        let _ = writeln!(out);
        let worst = agg.worst();
        let best = agg.best();
        let _ = writeln!(
            out,
            "Verste punkt:  {} ({})",
            worst.score.total, worst.bearing_label
        );
        let _ = writeln!(
            out,
            "Beste punkt:   {} ({})",
            best.score.total, best.bearing_label
        );
    }
    if agg.points.iter().any(|p| p.score.hard_capped) {
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "⚠ Minst ett punkt har kraftig regn eller storm — score er hard-cappet."
        );
    }
    out
}

/// Render a multi-day forecast in Norwegian. Each day gets one row with
/// the score bar, label, confidence, and (when applicable) a "best luke"
/// callout pointing at the optimal sub-window.
pub fn render_multi_day(label: &str, radius_km: f64, forecast: &MultiDayForecast) -> String {
    let mut out = String::new();
    let n = forecast.days.len();
    let _ = writeln!(
        out,
        "Grusindeks for {label} ({radius_km:.0}km radius) — {n} {}",
        if n == 1 { "dag" } else { "dager" }
    );
    let _ = writeln!(
        out,
        "─────────────────────────────────────────────────────────"
    );

    let today_local: NaiveDate = Local::now().date_naive();
    let mut any_low_confidence = false;

    for day in &forecast.days {
        if day.confidence == Confidence::Lav {
            any_low_confidence = true;
        }
        let day_label = day_label_no(day.date, today_local);
        let _ = writeln!(
            out,
            "{:<13} {} {:>3}  {:<10}  ⓘ {}",
            day_label,
            bar(day.mean),
            day.mean,
            mean_label(day.mean),
            day.confidence.label_no(),
        );
        if let Some(ow) = &day.optimal_window {
            let _ = writeln!(
                out,
                "              ↳ best luke: {}–{} → {} ({}, +{} poeng)",
                local_hm(ow.window.start),
                local_hm(ow.window.end),
                ow.score.total,
                mean_label(ow.score.total),
                ow.improvement,
            );
        }
    }

    if any_low_confidence {
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "Konfidens faller utover i prognosen — etter ca. 60 timer er kun 6-timers oppløsning tilgjengelig fra MET."
        );
    }
    out
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
///
/// The combined value is the weight-respecting average of the two — same
/// ratio (25:10) the total Grusindeks uses, so the bar matches what
/// actually feeds the score. The detail suffix is only emitted when the
/// two diverge by more than 5 points; otherwise it would just clutter the
/// usual stable-weather case where they're nearly identical.
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

fn avg(sum: u32, n: u32) -> u8 {
    if n == 0 {
        0
    } else {
        (sum / n) as u8
    }
}

fn mean_label(total: u8) -> &'static str {
    medvind_core::score::label_for(total)
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
        let out = render_human("Oslo", 20.0, win, &agg);
        assert!(out.contains("Grusindeks for Oslo"));
        assert!(out.contains("Total:"));
        assert!(out.contains("Temperatur"));
        assert!(out.contains("Vind"));
        assert!(out.contains("Nedbør"));
        assert!(out.contains("Bakke"));
        // The standalone "Regnsannsynl." row was merged into Nedbør.
        assert!(
            !out.contains("Regnsannsynl."),
            "precipitation rows should now be merged: {out}"
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
        // Dry forecast (100) but a 60% chance — model is hedging.
        let (combined, detail) = combined_precip(100, 40);
        assert!(detail.contains("mengde 100"), "got {detail:?}");
        assert!(detail.contains("sjanse 40"), "got {detail:?}");
        // 25:10 weighting → (100*25 + 40*10) / 35 = 82
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
        // Norwegian month abbreviation, lower-case.
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
        let out = render_multi_day("Oslo", 20.0, &forecast);
        assert!(out.contains("Grusindeks for Oslo"), "got {out}");
        assert!(out.contains("dag"), "got {out}");
        assert!(out.contains("ⓘ"), "missing confidence glyph: {out}");
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
        let out = render_multi_day("Oslo", 20.0, &forecast);
        assert!(out.contains("best luke"), "expected luke callout in {out}");
    }

    #[test]
    fn bar_renders_at_correct_width() {
        assert_eq!(bar(0).chars().filter(|c| *c == '█').count(), 0);
        assert_eq!(bar(100).chars().filter(|c| *c == '█').count(), BAR_WIDTH);
        // Halfway: 5 filled.
        assert_eq!(bar(50).chars().filter(|c| *c == '█').count(), 5);
    }
}
