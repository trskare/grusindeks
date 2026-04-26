//! Multi-point score aggregation.
//!
//! For each sample point we get a `Grusindeks`. The CLI reports the
//! min/mean/max along with a per-point breakdown so the user can see
//! whether the worst patch (often the wettest forest road on the
//! windward side) is what's dragging the score down.

use chrono::NaiveDate;
use grusindeks_core::daily::{Confidence, DayScore, OptimalWindow};
use grusindeks_core::geo::{bearing_deg, bearing_label_no, Point};
use grusindeks_core::score::Grusindeks;
use grusindeks_core::types::RideWindow;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct PointScore {
    pub point: Point,
    pub bearing_deg: f64,
    pub bearing_label: &'static str,
    pub score: Grusindeks,
}

#[derive(Debug, Clone, Serialize)]
pub struct AggregateScore {
    pub min: u8,
    pub mean: u8,
    pub max: u8,
    pub points: Vec<PointScore>,
}

impl AggregateScore {
    pub fn from_points(center: Point, points: Vec<(Point, Grusindeks)>) -> Self {
        let scored: Vec<PointScore> = points
            .into_iter()
            .map(|(p, s)| {
                let b = if p == center {
                    0.0
                } else {
                    bearing_deg(center, p)
                };
                PointScore {
                    point: p,
                    bearing_deg: b,
                    bearing_label: if p == center {
                        "senter"
                    } else {
                        bearing_label_no(b)
                    },
                    score: s,
                }
            })
            .collect();

        let totals: Vec<u8> = scored.iter().map(|p| p.score.total).collect();
        let min = *totals.iter().min().expect("at least one point");
        let max = *totals.iter().max().expect("at least one point");
        let mean = (totals.iter().map(|&v| u32::from(v)).sum::<u32>() / totals.len() as u32) as u8;
        AggregateScore {
            min,
            mean,
            max,
            points: scored,
        }
    }

    /// Return `(point, score)` for the worst-scoring sample. Useful for
    /// the "verste punkt: …" line in human output.
    pub fn worst(&self) -> &PointScore {
        self.points
            .iter()
            .min_by_key(|p| p.score.total)
            .expect("non-empty")
    }

    /// Return `(point, score)` for the best-scoring sample.
    pub fn best(&self) -> &PointScore {
        self.points
            .iter()
            .max_by_key(|p| p.score.total)
            .expect("non-empty")
    }
}

/// One sample point's day-level score, including any optimal sub-window
/// the daily module flagged for that point's hours.
#[derive(Debug, Clone, Serialize)]
pub struct DayPointScore {
    pub point: Point,
    pub bearing_deg: f64,
    pub bearing_label: &'static str,
    pub day_score: DayScore,
}

/// One day in the multi-day forecast view, aggregated across all sample
/// points around the center.
#[derive(Debug, Clone, Serialize)]
pub struct DayAggregate {
    /// Local date label (e.g. "2026-04-26"). The window itself is in UTC.
    pub date: NaiveDate,
    pub window: RideWindow,
    pub min: u8,
    pub mean: u8,
    pub max: u8,
    /// The most conservative confidence across the points — if any point
    /// is `Lav`, we report `Lav`.
    pub confidence: Confidence,
    /// "Where to ride" hint for this day. Sourced from the center point —
    /// it's a suggestion, not a guarantee. `None` when the day has no
    /// stand-out window worth flagging.
    pub optimal_window: Option<OptimalWindow>,
    pub points: Vec<DayPointScore>,
}

impl DayAggregate {
    /// Build a `DayAggregate` from per-point `DayScore`s. The first entry
    /// in `points` is treated as the center for the optimal-window hint
    /// and bearing labeling.
    pub fn from_points(
        date: NaiveDate,
        window: RideWindow,
        center: Point,
        points: Vec<(Point, DayScore)>,
    ) -> Self {
        let scored: Vec<DayPointScore> = points
            .into_iter()
            .map(|(p, ds)| {
                let b = if p == center {
                    0.0
                } else {
                    bearing_deg(center, p)
                };
                DayPointScore {
                    point: p,
                    bearing_deg: b,
                    bearing_label: if p == center {
                        "senter"
                    } else {
                        bearing_label_no(b)
                    },
                    day_score: ds,
                }
            })
            .collect();

        let totals: Vec<u8> = scored.iter().map(|p| p.day_score.score.total).collect();
        let min = *totals.iter().min().expect("at least one point");
        let max = *totals.iter().max().expect("at least one point");
        let mean = (totals.iter().map(|&v| u32::from(v)).sum::<u32>() / totals.len() as u32) as u8;

        // Most conservative confidence wins — if any point loses fidelity,
        // surface that to the user.
        let confidence = scored
            .iter()
            .map(|p| p.day_score.confidence)
            .fold(Confidence::Hoy, lower_confidence);

        let optimal_window = scored
            .iter()
            .find(|p| p.point == center)
            .and_then(|p| p.day_score.optimal_window.clone());

        DayAggregate {
            date,
            window,
            min,
            mean,
            max,
            confidence,
            optimal_window,
            points: scored,
        }
    }
}

impl DayAggregate {
    /// The center sample's full `DayScore`. Used by the renderer to pick a
    /// representative weather icon and penalty list for the day — neither
    /// of which makes sense to "average" across points.
    pub fn center(&self) -> &DayScore {
        self.points
            .iter()
            .find(|p| p.bearing_label == "senter")
            .map(|p| &p.day_score)
            .unwrap_or(&self.points[0].day_score)
    }
}

/// Multi-day forecast: one `DayAggregate` per requested local date.
#[derive(Debug, Clone, Serialize)]
pub struct MultiDayForecast {
    pub days: Vec<DayAggregate>,
}

fn lower_confidence(a: Confidence, b: Confidence) -> Confidence {
    use Confidence::*;
    match (a, b) {
        (Lav, _) | (_, Lav) => Lav,
        (Middels, _) | (_, Middels) => Middels,
        _ => Hoy,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use grusindeks_core::score::{score, ScoreBreakdown};
    use grusindeks_core::types::{HourlyConditions, RideWindow};

    fn t(h: u32) -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 4, 26, h, 0, 0).unwrap()
    }

    fn perfect(time_h: u32) -> HourlyConditions {
        HourlyConditions {
            probability_of_precip: Some(5.0),
            ..HourlyConditions::minimal(t(time_h), 17.0, 2.0, 0.0)
        }
    }

    fn awful(time_h: u32) -> HourlyConditions {
        HourlyConditions {
            probability_of_precip: Some(95.0),
            ..HourlyConditions::minimal(t(time_h), 5.0, 12.0, 4.0)
        }
    }

    #[test]
    fn aggregate_picks_extremes() {
        let win = RideWindow::from_hours(t(14), 3);
        let good = score(&(14..17).map(perfect).collect::<Vec<_>>(), win, 0.0);
        let bad = score(&(14..17).map(awful).collect::<Vec<_>>(), win, 4.5);
        let center = Point::new(59.9139, 10.7522);
        let agg = AggregateScore::from_points(
            center,
            vec![
                (center, good.clone()),
                (Point::new(60.0, 10.7522), bad.clone()),
            ],
        );
        assert_eq!(agg.max, good.total);
        assert_eq!(agg.min, bad.total);
        assert_eq!(agg.mean, (good.total / 2 + bad.total / 2));
        assert_eq!(agg.worst().score.total, bad.total);
        assert_eq!(agg.best().score.total, good.total);
    }

    #[test]
    fn aggregate_labels_center_as_senter() {
        let win = RideWindow::from_hours(t(14), 3);
        let good = score(&(14..17).map(perfect).collect::<Vec<_>>(), win, 0.0);
        let center = Point::new(59.9139, 10.7522);
        let agg = AggregateScore::from_points(center, vec![(center, good)]);
        assert_eq!(agg.points[0].bearing_label, "senter");
    }

    #[test]
    fn day_aggregate_picks_worst_confidence_across_points() {
        use grusindeks_core::daily::compute_day;
        use grusindeks_core::types::{HourlyConditions, Resolution};
        let win = RideWindow::from_hours(t(6), 12);
        let now = t(5);

        let hourly: Vec<HourlyConditions> = (6..18).map(perfect).collect();
        let mut six_hourly = hourly.clone();
        for h in &mut six_hourly {
            h.resolution = Resolution::SixHourly;
        }
        let center = Point::new(59.9139, 10.7522);
        let other = Point::new(60.0, 10.7522);
        let agg = DayAggregate::from_points(
            chrono::NaiveDate::from_ymd_opt(2026, 4, 26).unwrap(),
            win,
            center,
            vec![
                (center, compute_day(&hourly, win, 0.0, now)),
                (other, compute_day(&six_hourly, win, 0.0, now)),
            ],
        );
        // Center is Hoy, the other is Lav (>50% six-hourly) → worst is Lav.
        assert_eq!(agg.confidence, Confidence::Lav);
    }

    #[test]
    fn day_aggregate_takes_optimal_window_from_center() {
        use grusindeks_core::daily::compute_day;
        let win = RideWindow::from_hours(t(6), 12);
        let now = t(5);

        // Center has a clear "luke": rainy 09..15, dry otherwise.
        let mut center_hours: Vec<_> = (6..9).map(perfect).collect();
        center_hours.extend((9..15).map(awful));
        center_hours.extend((15..18).map(perfect));
        // The other point is uniformly perfect — no luke flagged.
        let other_hours: Vec<_> = (6..18).map(perfect).collect();

        let center = Point::new(59.9139, 10.7522);
        let other = Point::new(60.0, 10.7522);
        let agg = DayAggregate::from_points(
            chrono::NaiveDate::from_ymd_opt(2026, 4, 26).unwrap(),
            win,
            center,
            vec![
                (center, compute_day(&center_hours, win, 0.0, now)),
                (other, compute_day(&other_hours, win, 0.0, now)),
            ],
        );
        assert!(
            agg.optimal_window.is_some(),
            "expected the center point's luke to be surfaced"
        );
    }

    #[test]
    #[allow(unused_variables)]
    fn aggregate_serializes_to_json() {
        let win = RideWindow::from_hours(t(14), 3);
        let good = score(&(14..17).map(perfect).collect::<Vec<_>>(), win, 0.0);
        let center = Point::new(59.9139, 10.7522);
        let agg = AggregateScore::from_points(center, vec![(center, good)]);
        let json = serde_json::to_string(&agg).unwrap();
        assert!(json.contains("\"min\""));
        assert!(json.contains("\"points\""));
        // ScoreBreakdown is included.
        let _: ScoreBreakdown = agg.points[0].score.breakdown;
    }
}
