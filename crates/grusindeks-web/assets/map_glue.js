// Thin MapLibre shim. Keeps all MapLibre API churn in JS behind a tiny,
// stable global surface the Rust/wasm side calls:
//   gi_map_init(containerId, lng, lat, zoom)
//   gi_map_set_points(geojsonString)   -- 9 sample points, coloured by score
//   gi_map_set_ring(geojsonString)     -- the sampling-radius ring
//   gi_map_set_radar(enabled)          -- approximate MET radar/2.0 Nordic overlay
// Tiles come from OpenStreetMap raster (no API key); attribution is shown.
//
// Points are a GeoJSON circle layer (rendered in the same WebGL canvas as the
// ring, so the two always align). Clicking a point opens a popup with that
// point's numbers; wet points get a blue stroke. (Always-on score labels would
// need a glyph/font server, which we deliberately don't ship.)
(function () {
  let map = null;
  let ready = false;
  let pending = { points: null, ring: null };
  let radarEnabled = false;

  // MET radar/2.0 Nordic PNG is Lambert Conformal Conic, while MapLibre is
  // Web Mercator. This first-pass overlay uses the known Nordic LCC grid extent
  // and lets MapLibre warp the four image corners. It is useful for checking
  // MET data visually; pixel-accurate placement requires server-side
  // reprojection tile-by-tile.
  const RADAR_URL = "https://api.met.no/weatherapi/radar/2.0/5level_reflectivity.png?area=nordic";
  let radarUrlPromise = null;
  let radarObjectUrl = null;
  const RADAR_GRID = {
    xfirst: -897442,
    yfirst: -1104322,
    width: 719,
    height: 929,
    dx: 2500,
    dy: 2500,
  };

  function lccToLonLat(x, y) {
    const deg = Math.PI / 180;
    const R = 6371000;
    const lat0 = 63 * deg;
    const lon0 = 15 * deg;
    const lat1 = 63 * deg;
    const n = Math.sin(lat1);
    const F = (Math.cos(lat1) * Math.pow(Math.tan(Math.PI / 4 + lat1 / 2), n)) / n;
    const rho0 = (R * F) / Math.pow(Math.tan(Math.PI / 4 + lat0 / 2), n);
    const rho = Math.sqrt(x * x + (rho0 - y) * (rho0 - y));
    const theta = Math.atan2(x, rho0 - y);
    const lat = 2 * Math.atan(Math.pow((R * F) / rho, 1 / n)) - Math.PI / 2;
    const lon = lon0 + theta / n;
    return [lon / deg, lat / deg];
  }

  function radarCoordinates() {
    const g = RADAR_GRID;
    // Treat xfirst/yfirst as the centre of the lower-left grid cell and expand
    // by half a cell to get outer image edges.
    const minX = g.xfirst - g.dx / 2;
    const maxX = g.xfirst + (g.width - 1) * g.dx + g.dx / 2;
    const minY = g.yfirst - g.dy / 2;
    const maxY = g.yfirst + (g.height - 1) * g.dy + g.dy / 2;
    return [
      lccToLonLat(minX, maxY),
      lccToLonLat(maxX, maxY),
      lccToLonLat(maxX, minY),
      lccToLonLat(minX, minY),
    ];
  }

  // Selectable basemaps (all CORS-enabled raster XYZ; no API key). CyclOSM
  // renders gravel/unpaved tracks distinctly — the default for a gravel app.
  const BASEMAPS = {
    osm: {
      tiles: ["https://tile.openstreetmap.org/{z}/{x}/{y}.png"],
      attribution: "© OpenStreetMap contributors",
    },
    cyclosm: {
      tiles: [
        "https://a.tile-cyclosm.openstreetmap.fr/cyclosm/{z}/{x}/{y}.png",
        "https://b.tile-cyclosm.openstreetmap.fr/cyclosm/{z}/{x}/{y}.png",
        "https://c.tile-cyclosm.openstreetmap.fr/cyclosm/{z}/{x}/{y}.png",
      ],
      attribution: "© CyclOSM · © OpenStreetMap contributors",
    },
  };
  const DEFAULT_BASEMAP = "osm";

  function baseSource(key) {
    const bm = BASEMAPS[key] || BASEMAPS.osm;
    return { type: "raster", tiles: bm.tiles, tileSize: 256, attribution: bm.attribution };
  }

  const STYLE = {
    version: 8,
    sources: { base: baseSource(DEFAULT_BASEMAP) },
    layers: [{ id: "base", type: "raster", source: "base" }],
  };

  // Swap the basemap tiles, keeping the base layer below radar/ring/points.
  function setBasemap(key) {
    if (!map || !BASEMAPS[key]) return;
    if (map.getLayer("base")) map.removeLayer("base");
    if (map.getSource("base")) map.removeSource("base");
    map.addSource("base", baseSource(key));
    const before = map.getLayer("gi-radar") ? "gi-radar" : (map.getLayer("gi-ring-fill") ? "gi-ring-fill" : undefined);
    map.addLayer({ id: "base", type: "raster", source: "base" }, before);
  }

  function filteredRadarUrl() {
    if (radarUrlPromise) return radarUrlPromise;
    radarUrlPromise = new Promise((resolve, reject) => {
      const img = new Image();
      img.crossOrigin = "anonymous";
      img.onload = function () {
        const canvas = document.createElement("canvas");
        canvas.width = img.naturalWidth;
        canvas.height = img.naturalHeight;
        const ctx = canvas.getContext("2d", { willReadFrequently: true });
        ctx.drawImage(img, 0, 0);
        const data = ctx.getImageData(0, 0, canvas.width, canvas.height);
        for (let i = 0; i < data.data.length; i += 4) {
          const r = data.data[i], g = data.data[i + 1], b = data.data[i + 2];
          const max = Math.max(r, g, b), min = Math.min(r, g, b);
          const sat = max === 0 ? 0 : (max - min) / max;
          const hue = (() => {
            if (max === min) return 0;
            if (max === r) return (60 * ((g - b) / (max - min)) + 360) % 360;
            if (max === g) return 60 * ((b - r) / (max - min)) + 120;
            return 60 * ((r - g) / (max - min)) + 240;
          })();
          // MET's public PNG is a finished web map with coastlines, labels and
          // grey land/sea background. Keep only the saturated precipitation
          // palette (green/yellow/orange/red), and make labels/background fully
          // transparent so MapLibre's own basemap remains visible.
          const precip = sat > 0.24 && max > 120 && (hue < 45 || (hue > 50 && hue < 110));
          if (!precip) data.data[i + 3] = 0;
          else data.data[i + 3] = 185;
        }
        ctx.putImageData(data, 0, 0);
        canvas.toBlob((blob) => {
          if (!blob) {
            reject(new Error("Could not encode filtered radar image"));
            return;
          }
          if (radarObjectUrl) URL.revokeObjectURL(radarObjectUrl);
          radarObjectUrl = URL.createObjectURL(blob);
          resolve(radarObjectUrl);
        }, "image/png");
      };
      img.onerror = reject;
      img.src = RADAR_URL;
    });
    return radarUrlPromise;
  }

  function removeRadar() {
    if (map.getLayer("gi-radar")) map.removeLayer("gi-radar");
    if (map.getSource("gi-radar")) map.removeSource("gi-radar");
  }

  function addRadar(url) {
    if (!radarEnabled || !ready || !map) return;
    removeRadar();
    map.addSource("gi-radar", {
      type: "image",
      url: url,
      coordinates: radarCoordinates(),
    });
    const before = map.getLayer("gi-ring-fill") ? "gi-ring-fill" : undefined;
    map.addLayer({
      id: "gi-radar",
      type: "raster",
      source: "gi-radar",
      paint: { "raster-opacity": 0.75, "raster-fade-duration": 0 },
    }, before);
  }

  function setRadar(enabled) {
    radarEnabled = !!enabled;
    if (!ready || !map) return;
    if (!radarEnabled) {
      removeRadar();
      return;
    }
    filteredRadarUrl()
      .then(addRadar)
      .catch((e) => console.error("Could not load MET radar", e));
  }

  function emptyFC() {
    return { type: "FeatureCollection", features: [] };
  }

  // Dark-theme popup styling (the popup HTML itself needs no glyph server).
  function injectStyleOnce() {
    if (document.getElementById("gi-map-style")) return;
    const s = document.createElement("style");
    s.id = "gi-map-style";
    s.textContent = [
      ".maplibregl-popup-content{background:#3c3836;color:#ebdbb2;border-radius:10px;padding:8px 11px;",
      "box-shadow:0 6px 20px rgba(0,0,0,.5);font:13px ui-sans-serif,system-ui,sans-serif;}",
      ".maplibregl-popup-anchor-top .maplibregl-popup-tip{border-bottom-color:#3c3836;}",
      ".maplibregl-popup-anchor-bottom .maplibregl-popup-tip{border-top-color:#3c3836;}",
      ".maplibregl-popup-anchor-left .maplibregl-popup-tip{border-right-color:#3c3836;}",
      ".maplibregl-popup-anchor-right .maplibregl-popup-tip{border-left-color:#3c3836;}",
      ".gi-pop-h{display:flex;justify-content:space-between;gap:14px;font-weight:600;margin-bottom:2px;}",
      ".gi-pop-score{font-weight:800;font-variant-numeric:tabular-nums;}",
      ".gi-pop-row{font-size:12px;color:rgba(235,219,178,.8);}",
    ].join("");
    document.head.appendChild(s);
  }

  function popupHtml(p) {
    const temp = p.temp == null ? "–" : Math.round(p.temp) + "°C";
    const wind = p.wind == null ? "–" : Math.round(p.wind) + " m/s";
    const rain = p.precip == null ? "–" : p.precip > 0 ? p.precip.toFixed(1) + " mm" : "tørt";
    const wet = p.wet ? " 💧" : "";
    return (
      '<div class="gi-pop-h"><span>' + p.label + "</span>" +
      '<span class="gi-pop-score" style="color:' + p.color + '">' + p.total + "</span></div>" +
      '<div class="gi-pop-row">' + temp + " · " + wind + " · " + rain + wet + "</div>"
    );
  }

  function ensureLayers() {
    if (!map || map.getSource("gi-points")) return;
    map.addSource("gi-ring", { type: "geojson", data: emptyFC() });
    map.addSource("gi-points", { type: "geojson", data: emptyFC() });
    map.addLayer({
      id: "gi-ring-fill",
      type: "fill",
      source: "gi-ring",
      paint: { "fill-color": "#83a598", "fill-opacity": 0.12 },
    });
    map.addLayer({
      id: "gi-ring-line",
      type: "line",
      source: "gi-ring",
      paint: { "line-color": "#83a598", "line-width": 2.5, "line-dasharray": [2, 2] },
    });
    map.addLayer({
      id: "gi-points",
      type: "circle",
      source: "gi-points",
      paint: {
        "circle-radius": ["case", ["get", "is_center"], 10, 7],
        "circle-color": ["get", "color"],
        // Wet points get a blue ring; the centre keeps a thick dark ring.
        "circle-stroke-width": [
          "case",
          ["get", "is_center"], 3,
          ["case", ["get", "wet"], 2.5, 1.5],
        ],
        "circle-stroke-color": [
          "case",
          ["all", ["get", "wet"], ["!", ["get", "is_center"]]], "#83a598",
          "#282828",
        ],
        "circle-radius-transition": { duration: 500 },
        "circle-color-transition": { duration: 500 },
      },
    });

    // Click a point → popup with its numbers; pointer cursor on hover.
    map.on("click", "gi-points", function (e) {
      const f = e.features && e.features[0];
      if (!f) return;
      new maplibregl.Popup({ offset: 14, closeButton: false })
        .setLngLat(f.geometry.coordinates)
        .setHTML(popupHtml(f.properties || {}))
        .addTo(map);
    });
    map.on("mouseenter", "gi-points", function () {
      map.getCanvas().style.cursor = "pointer";
    });
    map.on("mouseleave", "gi-points", function () {
      map.getCanvas().style.cursor = "";
    });
  }

  function ringBounds(ring) {
    const coords = ring?.features?.[0]?.geometry?.coordinates?.[0];
    if (!coords || coords.length === 0 || !window.maplibregl) return null;
    const bounds = coords.reduce(
      (b, coord) => b.extend(coord),
      new maplibregl.LngLatBounds(coords[0], coords[0]),
    );
    return bounds;
  }

  function fitRing(ring, animate) {
    const bounds = ringBounds(ring);
    if (!bounds || bounds.isEmpty()) return;
    // Snug fit: just enough padding to keep the ring off the edges, and a high
    // maxZoom cap so a small sampling radius still fills the map.
    map.fitBounds(bounds, {
      padding: 28,
      maxZoom: 13,
      duration: animate ? 500 : 0,
    });
  }

  function applyPending() {
    if (!ready || !map) return;
    ensureLayers();
    setRadar(radarEnabled);
    if (pending.ring) {
      map.getSource("gi-ring").setData(pending.ring);
      fitRing(pending.ring, false);
    }
    if (pending.points) map.getSource("gi-points").setData(pending.points);
  }

  window.gi_map_init = function (id, lng, lat, zoom) {
    if (!window.maplibregl) {
      console.error("maplibre-gl not loaded");
      return;
    }
    injectStyleOnce();
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
      attributionControl: false,
    });
    map.addControl(new maplibregl.NavigationControl({ showCompass: false }), "top-right");
    map.on("load", function () {
      ready = true;
      ensureLayers();
      applyPending();
    });
    // The map container grows after init (the timeline loads → the right column
    // stretches taller). MapLibre keeps centre+zoom on resize, which would leave
    // the ring tiny in a now-bigger map — so re-fit on resize until the user
    // pans/zooms themselves (`originalEvent` is only set for user gestures).
    let userMoved = false;
    map.on("moveend", function (e) {
      if (e && e.originalEvent) userMoved = true;
    });
    map.on("resize", function () {
      if (!userMoved && pending.ring) fitRing(pending.ring, false);
    });
  };

  window.gi_map_set_points = function (geojson) {
    pending.points = JSON.parse(geojson);
    if (ready) map.getSource("gi-points").setData(pending.points);
  };

  window.gi_map_set_ring = function (geojson) {
    pending.ring = JSON.parse(geojson);
    if (ready) {
      map.getSource("gi-ring").setData(pending.ring);
      fitRing(pending.ring, true);
    }
  };

  window.gi_map_set_basemap = function (key) {
    if (ready) setBasemap(key);
  };

  window.gi_map_set_radar = function (enabled) {
    setRadar(enabled);
  };
})();
