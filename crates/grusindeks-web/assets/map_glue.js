// Thin MapLibre shim. Keeps all MapLibre API churn in JS behind a tiny,
// stable global surface the Rust/wasm side calls:
//   gi_map_init(containerId, lng, lat, zoom)
//   gi_map_set_points(geojsonString)   -- 9 sample points, coloured by score
//   gi_map_set_ring(geojsonString)     -- the sampling-radius ring
// Tiles come from OpenStreetMap raster (no API key); attribution is shown.
(function () {
  let map = null;
  let ready = false;
  let pending = { points: null, ring: null };

  const STYLE = {
    version: 8,
    sources: {
      osm: {
        type: "raster",
        tiles: ["https://tile.openstreetmap.org/{z}/{x}/{y}.png"],
        tileSize: 256,
        attribution: "© OpenStreetMap contributors",
      },
    },
    layers: [{ id: "osm", type: "raster", source: "osm" }],
  };

  function emptyFC() {
    return { type: "FeatureCollection", features: [] };
  }

  function ensureLayers() {
    if (!map || map.getSource("gi-points")) return;
    map.addSource("gi-ring", { type: "geojson", data: emptyFC() });
    map.addSource("gi-points", { type: "geojson", data: emptyFC() });
    map.addLayer({
      id: "gi-ring-fill",
      type: "fill",
      source: "gi-ring",
      paint: { "fill-color": "#83a598", "fill-opacity": 0.07 },
    });
    map.addLayer({
      id: "gi-ring-line",
      type: "line",
      source: "gi-ring",
      paint: { "line-color": "#83a598", "line-width": 1.5, "line-dasharray": [2, 2] },
    });
    map.addLayer({
      id: "gi-points",
      type: "circle",
      source: "gi-points",
      paint: {
        "circle-radius": ["case", ["get", "is_center"], 10, 7],
        "circle-color": ["get", "color"],
        "circle-stroke-width": ["case", ["get", "is_center"], 3, 1.5],
        "circle-stroke-color": "#282828",
        "circle-radius-transition": { duration: 500 },
        "circle-color-transition": { duration: 500 },
      },
    });
  }

  function applyPending() {
    if (!ready || !map) return;
    ensureLayers();
    if (pending.ring) map.getSource("gi-ring").setData(pending.ring);
    if (pending.points) map.getSource("gi-points").setData(pending.points);
  }

  window.gi_map_init = function (id, lng, lat, zoom) {
    if (!window.maplibregl) {
      console.error("maplibre-gl not loaded");
      return;
    }
    if (map) {
      map.jumpTo({ center: [lng, lat], zoom: zoom });
      applyPending();
      return;
    }
    map = new maplibregl.Map({
      container: id,
      style: STYLE,
      center: [lng, lat],
      zoom: zoom,
    });
    map.addControl(new maplibregl.NavigationControl({ showCompass: false }), "top-right");
    map.on("load", function () {
      ready = true;
      ensureLayers();
      applyPending();
    });
  };

  window.gi_map_set_points = function (geojson) {
    pending.points = JSON.parse(geojson);
    if (ready) map.getSource("gi-points").setData(pending.points);
  };

  window.gi_map_set_ring = function (geojson) {
    pending.ring = JSON.parse(geojson);
    if (ready) map.getSource("gi-ring").setData(pending.ring);
  };
})();
