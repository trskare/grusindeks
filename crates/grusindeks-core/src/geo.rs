//! Geographical helpers: a `Point` type, the haversine great-circle distance,
//! a destination-by-bearing solver, and the 9-point sampling pattern used to
//! cover a circular ride area around a center.

/// Earth's mean radius in kilometers (WGS-84-ish, good enough for ride
/// scoring at <100km scale).
pub const EARTH_RADIUS_KM: f64 = 6371.0088;

/// A WGS-84 point. Latitude/longitude are in **decimal degrees**.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub lat: f64,
    pub lon: f64,
}

impl Point {
    pub fn new(lat: f64, lon: f64) -> Self {
        Self { lat, lon }
    }

    /// Truncate both coordinates to 4 decimals, toward zero. Required by
    /// `api.met.no` — 5+ decimals returns 403 Forbidden.
    pub fn truncated(self) -> Self {
        Self {
            lat: truncate_coord(self.lat),
            lon: truncate_coord(self.lon),
        }
    }
}

/// Truncate a single coordinate to 4 decimals, toward zero.
///
/// We deliberately *truncate* (toward zero), not round: rounding 0.99995 up
/// to 1.0000 keeps 4 decimals but adds a tiny bias; the goal is just to stay
/// under MET's 5-decimal limit, and ~11m of horizontal slop is irrelevant at
/// weather-grid resolution.
pub fn truncate_coord(deg: f64) -> f64 {
    (deg * 10_000.0).trunc() / 10_000.0
}

/// Great-circle distance between two points, in kilometers.
pub fn haversine_km(a: Point, b: Point) -> f64 {
    let lat1 = a.lat.to_radians();
    let lat2 = b.lat.to_radians();
    let dlat = (b.lat - a.lat).to_radians();
    let dlon = (b.lon - a.lon).to_radians();

    let h = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_KM * h.sqrt().asin()
}

/// Compute the destination point reached by traveling `distance_km` along
/// the great circle from `start`, on the given `bearing_deg` (0° = north,
/// 90° = east, clockwise).
pub fn destination(start: Point, bearing_deg: f64, distance_km: f64) -> Point {
    let ang_dist = distance_km / EARTH_RADIUS_KM;
    let brng = bearing_deg.to_radians();
    let lat1 = start.lat.to_radians();
    let lon1 = start.lon.to_radians();

    let lat2 = (lat1.sin() * ang_dist.cos() + lat1.cos() * ang_dist.sin() * brng.cos()).asin();
    let lon2 = lon1
        + (brng.sin() * ang_dist.sin() * lat1.cos())
            .atan2(ang_dist.cos() - lat1.sin() * lat2.sin());

    // Normalize longitude to [-180, 180].
    let lon2_deg = ((lon2.to_degrees() + 540.0) % 360.0) - 180.0;
    Point {
        lat: lat2.to_degrees(),
        lon: lon2_deg,
    }
}

/// The 8 compass bearings in degrees (N, NE, E, SE, S, SW, W, NW).
const COMPASS_BEARINGS: [f64; 8] = [0.0, 45.0, 90.0, 135.0, 180.0, 225.0, 270.0, 315.0];

/// Sample the area around a center with 9 points: the center plus the 8
/// compass directions at `radius_km`. All returned coordinates are
/// 4-decimal-truncated and ready to send to `api.met.no`.
pub fn sample_around(center: Point, radius_km: f64) -> Vec<Point> {
    let mut out = Vec::with_capacity(9);
    out.push(center.truncated());
    for &b in &COMPASS_BEARINGS {
        out.push(destination(center, b, radius_km).truncated());
    }
    out
}

/// Convert a bearing (0–360°) to a short Norwegian compass label.
/// Useful for output formatting ("verste punkt: NV").
pub fn bearing_label_no(bearing_deg: f64) -> &'static str {
    let b = ((bearing_deg % 360.0) + 360.0) % 360.0;
    let idx = ((b / 45.0).round() as usize) % 8;
    ["N", "NØ", "Ø", "SØ", "S", "SV", "V", "NV"][idx]
}

/// Initial bearing from `from` to `to`, in degrees clockwise from north.
pub fn bearing_deg(from: Point, to: Point) -> f64 {
    let lat1 = from.lat.to_radians();
    let lat2 = to.lat.to_radians();
    let dlon = (to.lon - from.lon).to_radians();

    let y = dlon.sin() * lat2.cos();
    let x = lat1.cos() * lat2.sin() - lat1.sin() * lat2.cos() * dlon.cos();
    let brng = y.atan2(x).to_degrees();
    (brng + 360.0) % 360.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn assert_close(actual: f64, expected: f64, tol: f64, label: &str) {
        assert!(
            (actual - expected).abs() < tol,
            "{label}: expected {expected}, got {actual} (tol {tol})"
        );
    }

    // ---- truncate_coord ----

    #[rstest]
    #[case(59.91391, 59.9139)]
    #[case(59.91399, 59.9139)] // toward zero, never rounds up
    #[case(10.75221, 10.7522)]
    #[case(-10.75229, -10.7522)] // toward zero for negatives too
    #[case(0.0, 0.0)]
    #[case(59.9139, 59.9139)] // already 4 decimals — unchanged
    #[case(59.91, 59.91)] // fewer decimals — unchanged
    fn truncate_coord_examples(#[case] input: f64, #[case] expected: f64) {
        let got = truncate_coord(input);
        assert_close(got, expected, 1e-9, "truncate");
    }

    #[test]
    fn truncated_point_has_at_most_4_decimals() {
        let p = Point::new(59.913918, 10.752230).truncated();
        // The literal "5+ decimals" check from the TOS: stringifying should
        // never yield more than 4 fractional digits.
        let lat_str = format!("{:.10}", p.lat);
        let lon_str = format!("{:.10}", p.lon);
        let frac_lat = lat_str.split('.').nth(1).unwrap();
        let frac_lon = lon_str.split('.').nth(1).unwrap();
        // After truncation everything past digit 4 must be '0'.
        assert!(frac_lat[4..].chars().all(|c| c == '0'), "lat: {lat_str}");
        assert!(frac_lon[4..].chars().all(|c| c == '0'), "lon: {lon_str}");
    }

    // ---- haversine_km ----

    #[test]
    fn haversine_oslo_to_bergen() {
        let oslo = Point::new(59.9139, 10.7522);
        let bergen = Point::new(60.3913, 5.3221);
        // Reference value ~308 km; allow a small tolerance for radius choice.
        let d = haversine_km(oslo, bergen);
        assert_close(d, 308.0, 3.0, "Oslo→Bergen");
    }

    #[test]
    fn haversine_zero_for_same_point() {
        let p = Point::new(59.9139, 10.7522);
        assert!(haversine_km(p, p) < 1e-9);
    }

    #[test]
    fn haversine_symmetric() {
        let a = Point::new(59.9139, 10.7522);
        let b = Point::new(60.3913, 5.3221);
        assert_close(haversine_km(a, b), haversine_km(b, a), 1e-9, "symmetry");
    }

    // ---- destination ----

    #[rstest]
    #[case(0.0, "north")]
    #[case(90.0, "east")]
    #[case(180.0, "south")]
    #[case(270.0, "west")]
    fn destination_round_trips_distance(#[case] bearing: f64, #[case] label: &str) {
        let start = Point::new(59.9139, 10.7522);
        let dest = destination(start, bearing, 20.0);
        // The destination should be exactly 20 km away regardless of bearing.
        let d = haversine_km(start, dest);
        assert_close(d, 20.0, 0.05, label);
    }

    #[test]
    fn destination_north_increases_latitude() {
        let start = Point::new(59.9139, 10.7522);
        let north = destination(start, 0.0, 20.0);
        assert!(north.lat > start.lat);
        assert_close(north.lon, start.lon, 1e-6, "moving N keeps lon");
    }

    #[test]
    fn destination_east_increases_longitude() {
        let start = Point::new(59.9139, 10.7522);
        let east = destination(start, 90.0, 20.0);
        assert!(east.lon > start.lon);
        // Latitude moves only marginally when going east at this latitude.
        assert_close(east.lat, start.lat, 0.01, "moving E keeps lat ~stable");
    }

    // ---- sample_around ----

    #[test]
    fn sample_around_returns_9_points() {
        let center = Point::new(59.9139, 10.7522);
        let pts = sample_around(center, 20.0);
        assert_eq!(pts.len(), 9);
    }

    #[test]
    fn sample_around_first_is_center_truncated() {
        let center = Point::new(59.913918, 10.752230);
        let pts = sample_around(center, 20.0);
        assert_eq!(pts[0], center.truncated());
    }

    #[test]
    fn sample_around_all_at_radius() {
        let center = Point::new(59.9139, 10.7522);
        let radius = 20.0;
        let pts = sample_around(center, radius);
        for (i, p) in pts.iter().enumerate().skip(1) {
            // Truncation introduces up to ~11m error; allow 0.05 km tolerance.
            let d = haversine_km(pts[0], *p);
            assert_close(d, radius, 0.05, &format!("compass point #{i}"));
        }
    }

    #[test]
    fn sample_around_coords_are_4_decimal_truncated() {
        let pts = sample_around(Point::new(59.913918, 10.752230), 20.0);
        for p in pts {
            // After truncation, multiplying by 10_000 should give an integer.
            let lat_int = p.lat * 10_000.0;
            let lon_int = p.lon * 10_000.0;
            assert!(
                (lat_int - lat_int.round()).abs() < 1e-6,
                "lat {} not truncated",
                p.lat
            );
            assert!(
                (lon_int - lon_int.round()).abs() < 1e-6,
                "lon {} not truncated",
                p.lon
            );
        }
    }

    // ---- bearing_label_no ----

    #[rstest]
    #[case(0.0, "N")]
    #[case(45.0, "NØ")]
    #[case(90.0, "Ø")]
    #[case(135.0, "SØ")]
    #[case(180.0, "S")]
    #[case(225.0, "SV")]
    #[case(270.0, "V")]
    #[case(315.0, "NV")]
    #[case(360.0, "N")] // wraps
    #[case(22.0, "N")] // rounds to nearest
    #[case(23.0, "NØ")]
    fn bearing_labels(#[case] deg: f64, #[case] expected: &str) {
        assert_eq!(bearing_label_no(deg), expected);
    }

    // ---- bearing_deg ----

    #[test]
    fn bearing_north_is_zero() {
        let from = Point::new(59.0, 10.0);
        let to = destination(from, 0.0, 10.0);
        let b = bearing_deg(from, to);
        // 0 or ~360
        let normalized = if b > 180.0 { 360.0 - b } else { b };
        assert!(normalized.abs() < 0.5, "expected ~0°, got {b}");
    }
}
