//! Interactive MapLibre map of the 9 sample points, coloured by score, with
//! the sampling-radius ring.
//!
//! All MapLibre calls go through the tiny global surface defined in
//! `assets/map_glue.js` (loaded as a classic script in the document head). The
//! Rust side only builds GeoJSON and calls three functions, and only on the
//! client (the init effect is `hydrate`-gated; server-side this is just an
//! empty `<div>` that the client fills after hydration).

use leptos::prelude::*;

use grusindeks_core::aggregate::PointScore;

#[cfg(feature = "hydrate")]
mod glue {
    use wasm_bindgen::prelude::*;
    #[wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(js_name = gi_map_init)]
        pub fn init(id: &str, lng: f64, lat: f64, zoom: f64);
        #[wasm_bindgen(js_name = gi_map_set_points)]
        pub fn set_points(geojson: &str);
        #[wasm_bindgen(js_name = gi_map_set_ring)]
        pub fn set_ring(geojson: &str);
    }
}

#[cfg(feature = "hydrate")]
fn points_geojson(points: &[PointScore]) -> String {
    let feats: Vec<_> = points
        .iter()
        .map(|p| {
            serde_json::json!({
                "type": "Feature",
                "geometry": { "type": "Point", "coordinates": [p.point.lon, p.point.lat] },
                "properties": {
                    "total": p.score.total,
                    "color": crate::color::hex(p.score.total),
                    "is_center": p.is_center,
                    "label": p.bearing_label,
                }
            })
        })
        .collect();
    serde_json::json!({ "type": "FeatureCollection", "features": feats }).to_string()
}

#[cfg(feature = "hydrate")]
fn ring_geojson(center: grusindeks_core::geo::Point, radius_km: f64) -> String {
    use grusindeks_core::geo::destination;
    let coords: Vec<[f64; 2]> = (0..=64)
        .map(|i| {
            let bearing = (i as f64) * (360.0 / 64.0);
            let p = destination(center, bearing, radius_km);
            [p.lon, p.lat]
        })
        .collect();
    serde_json::json!({
        "type": "FeatureCollection",
        "features": [{
            "type": "Feature",
            "geometry": { "type": "Polygon", "coordinates": [coords] },
            "properties": {}
        }]
    })
    .to_string()
}

#[component]
pub fn MapView(points: Vec<PointScore>) -> impl IntoView {
    #[cfg(feature = "hydrate")]
    {
        let points = points.clone();
        Effect::new(move |_| {
            use grusindeks_core::geo::{haversine_km, Point};
            let center = points
                .iter()
                .find(|p| p.is_center)
                .or_else(|| points.first());
            if let Some(c) = center {
                let center_pt = Point::new(c.point.lat, c.point.lon);
                // Radius = farthest sample from centre (= the sampling radius).
                let radius = points
                    .iter()
                    .map(|p| haversine_km(center_pt, Point::new(p.point.lat, p.point.lon)))
                    .fold(0.0_f64, f64::max)
                    .max(1.0);
                glue::init("gi-map", center_pt.lon, center_pt.lat, 9.0);
                glue::set_ring(&ring_geojson(center_pt, radius));
                glue::set_points(&points_geojson(&points));
            }
        });
    }
    #[cfg(not(feature = "hydrate"))]
    let _ = &points;

    view! {
        <div
            id="gi-map"
            class="mt-6 h-80 w-full overflow-hidden rounded-2xl border border-gruv-bg2"
        ></div>
    }
}
