//! Multi-point score aggregation.
//!
//! For each sample point we get a `Grusindeks`. The CLI reports the
//! min/mean/max along with a per-point breakdown so the user can see
//! whether the worst patch (often the wettest forest road on the
//! windward side) is what's dragging the score down.

use medvind_core::geo::{bearing_deg, bearing_label_no, Point};
use medvind_core::score::Grusindeks;
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use medvind_core::score::{score, ScoreBreakdown};
    use medvind_core::types::{HourlyConditions, RideWindow};

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
