//! Human-readable formatting of an `AggregateScore`. Pure functions that
//! return `String`s — easy to insta-snapshot.

use std::fmt::Write as _;

use chrono::{DateTime, Local, Utc};
use medvind_core::types::RideWindow;

use crate::aggregate::AggregateScore;

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
    let _ = writeln!(out, "Nedbør         {} {:>3}", bar(agg_precip), agg_precip);
    let _ = writeln!(out, "Regnsannsynl.  {} {:>3}", bar(agg_prob), agg_prob);
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
    }

    #[test]
    fn bar_renders_at_correct_width() {
        assert_eq!(bar(0).chars().filter(|c| *c == '█').count(), 0);
        assert_eq!(bar(100).chars().filter(|c| *c == '█').count(), BAR_WIDTH);
        // Halfway: 5 filled.
        assert_eq!(bar(50).chars().filter(|c| *c == '█').count(), 5);
    }
}
