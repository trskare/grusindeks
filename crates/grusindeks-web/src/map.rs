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
        #[wasm_bindgen(js_name = gi_map_set_basemap)]
        pub fn set_basemap(key: &str);
        #[wasm_bindgen(js_name = gi_map_set_radar)]
        pub fn set_radar(enabled: bool);
    }
}

/// Switch the basemap tiles (client-only; no-op during SSR).
#[cfg(feature = "hydrate")]
fn set_basemap_js(key: &str) {
    glue::set_basemap(key);
}
#[cfg(not(feature = "hydrate"))]
fn set_basemap_js(_key: &str) {}

/// Toggle the MET radar image overlay (client-only; no-op during SSR).
#[cfg(feature = "hydrate")]
fn set_radar_js(enabled: bool) {
    glue::set_radar(enabled);
}
#[cfg(not(feature = "hydrate"))]
fn set_radar_js(_enabled: bool) {}

#[cfg(feature = "hydrate")]
fn points_geojson(points: &[PointScore]) -> String {
    let feats: Vec<_> = points
        .iter()
        .map(|p| {
            let s = &p.score.stats;
            let has = !s.is_empty();
            serde_json::json!({
                "type": "Feature",
                "geometry": { "type": "Point", "coordinates": [p.point.lon, p.point.lat] },
                "properties": {
                    "total": p.score.total,
                    "color": crate::color::hex(p.score.total),
                    "is_center": p.is_center,
                    "label": p.bearing_label,
                    // Per-point numbers for the click popup + the wet badge.
                    "temp": has.then_some(s.mean_temp_c),
                    "wind": has.then_some(s.max_wind_ms),
                    "precip": has.then_some(s.total_precip_mm),
                    "wet": has && s.total_precip_mm >= 0.05,
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
    // One wind indicator for the area: the centre point's mean direction + peak
    // speed. `None` when there's no direction data (empty window).
    let wind = points
        .iter()
        .find(|p| p.is_center)
        .or_else(|| points.first())
        .and_then(|p| {
            p.score
                .stats
                .wind_from_deg
                .map(|d| (d, p.score.stats.max_wind_ms))
        });

    // Basemap switcher. Defaults to plain OSM ("Standard"); matches the glue's
    // default tile source. ("Grus" = CyclOSM shows gravel/unpaved tracks.)
    let basemap = RwSignal::new("osm");
    let radar = RwSignal::new(false);

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
                glue::init("gi-map", center_pt.lon, center_pt.lat, 8.0);
                glue::set_ring(&ring_geojson(center_pt, radius));
                glue::set_points(&points_geojson(&points));
            }
        });
    }
    #[cfg(not(feature = "hydrate"))]
    let _ = &points;

    view! {
        // On wide screens the section fills its (stretched) column so the map can
        // grow to take up the slack; on narrow screens it's a normal block.
        <section class="overflow-hidden rounded-2xl bg-gruv-bg1 shadow-lg ring-1 ring-gruv-bg2/60 min-[2024px]:flex min-[2024px]:h-full min-[2024px]:flex-col">
            <div class="flex items-center justify-between gap-4 px-5 py-4">
                <div class="flex flex-wrap items-center gap-x-3 gap-y-2">
                    <h2 class="text-sm font-semibold uppercase tracking-wide text-gruv-gray">"Kart over området"</h2>
                    <a
                        class="text-xs text-gruv-fg/70 underline-offset-2 hover:text-gruv-fg hover:underline"
                        href=move || if basemap.get() == "cyclosm" { "https://www.cyclosm.org" } else { "https://www.openstreetmap.org/copyright" }
                        target="_blank"
                        rel="noopener noreferrer"
                    >{move || if basemap.get() == "cyclosm" { "© CyclOSM · OSM" } else { "© OSM" }}</a>
                    // Basemap switcher.
                    <div class="flex overflow-hidden rounded-lg text-xs font-semibold ring-1 ring-gruv-bg2">
                        {[("osm", "Standard"), ("cyclosm", "Grus")].into_iter().map(|(k, label)| {
                            let active = move || basemap.get() == k;
                            view! {
                                <button type="button"
                                    class=move || if active() {
                                        "px-2.5 py-1 bg-gruv-aqua text-gruv-bg0"
                                    } else {
                                        "px-2.5 py-1 bg-gruv-bg0/60 text-gruv-fg/80 hover:bg-gruv-bg2/60 hover:text-gruv-fg"
                                    }
                                    on:click=move |_| { basemap.set(k); set_basemap_js(k); }
                                >{label}</button>
                            }
                        }).collect_view()}
                    </div>
                    <button type="button"
                        title="Viser MET radar/2.0 Nordic som omtrentlig bilde-overlegg (ikke pikselnøyaktig reprojisert)."
                        class=move || if radar.get() {
                            "rounded-lg bg-gruv-blue px-2.5 py-1 text-xs font-semibold text-gruv-bg0 ring-1 ring-gruv-blue"
                        } else {
                            "rounded-lg bg-gruv-bg0/60 px-2.5 py-1 text-xs font-semibold text-gruv-fg/80 ring-1 ring-gruv-bg2 hover:bg-gruv-bg2/60 hover:text-gruv-fg"
                        }
                        on:click=move |_| {
                            let next = !radar.get_untracked();
                            radar.set(next);
                            set_radar_js(next);
                        }
                    >"MET-radar"</button>
                </div>
                // Legend mirrors the five score buckets the dots are coloured by.
                <div class="flex flex-wrap justify-end gap-x-2 gap-y-1 text-xs uppercase tracking-wide text-gruv-fg/70">
                    <span class="inline-flex items-center gap-1"><i class="h-2 w-2 rounded-full bg-gruv-green"></i>"strålende"</span>
                    <span class="inline-flex items-center gap-1"><i class="h-2 w-2 rounded-full bg-gruv-lime"></i>"bra"</span>
                    <span class="inline-flex items-center gap-1"><i class="h-2 w-2 rounded-full bg-gruv-yellow"></i>"brukbart"</span>
                    <span class="inline-flex items-center gap-1"><i class="h-2 w-2 rounded-full bg-gruv-orange"></i>"svakt"</span>
                    <span class="inline-flex items-center gap-1"><i class="h-2 w-2 rounded-full bg-gruv-red"></i>"dårlig"</span>
                </div>
            </div>
            <div class="relative border-t border-gruv-bg2 min-[2024px]:flex-1 min-[2024px]:min-h-0">
                // Fills the remaining column height on wide screens (so the map
                // grows to absorb the slack above the compact trend); fixed h-80
                // on narrow screens. MapLibre tracks the container and resizes.
                <div id="gi-map" class="h-80 w-full min-[2024px]:h-full"></div>
                {move || radar.get().then(|| view! {
                    <div class="pointer-events-none absolute bottom-3 right-3 z-[1] rounded-lg bg-gruv-bg0/85 px-2.5 py-1.5 text-xs font-semibold text-gruv-fg shadow-lg ring-1 ring-gruv-bg2/70 backdrop-blur">
                        "MET-radar på · kun nedbør vises"
                    </div>
                })}
                // Area wind: arrow points the way the wind blows (from-dir + 180°).
                {wind.map(|(deg, speed)| {
                    let rot = deg + 180.0;
                    view! {
                        <div class="pointer-events-none absolute bottom-3 left-3 z-[1] flex items-center gap-1.5 rounded-lg bg-gruv-bg0/85 px-2.5 py-1.5 text-sm font-semibold text-gruv-fg shadow-lg ring-1 ring-gruv-bg2/70 backdrop-blur">
                            <svg viewBox="0 0 24 24" class="h-4 w-4 text-gruv-aqua" fill="currentColor"
                                style=format!("transform: rotate({rot:.0}deg)")>
                                <path d="M12 2 L18 20 L12 16 L6 20 Z"/>
                            </svg>
                            <span class="tabular-nums">{format!("{speed:.0} m/s")}</span>
                            <span class="text-xs font-normal text-gruv-fg/60">"vind"</span>
                        </div>
                    }
                })}
            </div>
        </section>
    }
}
