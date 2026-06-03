// Thin MapLibre shim. Keeps all MapLibre API churn in JS behind a tiny,
// stable global surface the Rust/wasm side calls:
//   gi_map_init(containerId, lng, lat, zoom)
//   gi_map_set_points(geojsonString)   -- 9 sample points, coloured by score
//   gi_map_set_ring(geojsonString)     -- the sampling-radius ring
//   gi_map_set_radar(enabled)          -- MET/Yr precipitation-nowcast XYZ tiles
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

  // Radar overlay = MET Norway's precipitation-nowcast, served by Yr as plain
  // Web-Mercator XYZ tiles (the exact source yr.no's own radar map uses).
  // MapLibre consumes them natively — correct projection, no warp, no
  // colour-keying. `available.json` lists the frames (latest observation + a
  // ~2 h nowcast) with ready-made {z}/{x}/{y} URL templates and the zoom range.
  const RADAR_AVAILABLE = "https://tiles.yr.no/api/precipitation-nowcast/available.json";

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

  // Radar animation state: the frames (each a {z}/{x}/{y} template + its time),
  // the current index, and the playback timer. The manifest runs from the
  // latest observation forward through the ~2 h nowcast — we loop over it like
  // yr.no, swapping the source's tiles per frame (setTiles, no layer rebuild).
  let radarFrames = [];
  let radarIdx = 0;
  let radarTimer = null;

  function stopRadarAnim() {
    if (radarTimer) {
      clearTimeout(radarTimer);
      radarTimer = null;
    }
  }

  function removeRadar() {
    stopRadarAnim();
    if (map.getLayer("gi-radar")) map.removeLayer("gi-radar");
    if (map.getSource("gi-radar")) map.removeSource("gi-radar");
    const clk = document.getElementById("gi-radar-clock");
    if (clk) clk.remove();
  }

  // A small frame-time chip in the map's top-left, managed entirely here.
  function radarClock() {
    let el = document.getElementById("gi-radar-clock");
    if (!el) {
      const host = document.getElementById("gi-map");
      if (!host) return null;
      if (!host.style.position) host.style.position = "relative";
      el = document.createElement("div");
      el.id = "gi-radar-clock";
      el.style.cssText =
        "position:absolute;top:10px;left:10px;z-index:2;pointer-events:none;" +
        "background:rgba(40,40,40,0.85);color:#ebdbb2;" +
        "font:600 12px/1.2 system-ui,-apple-system,sans-serif;" +
        "padding:5px 9px;border-radius:8px;border:1px solid rgba(80,73,69,0.7);" +
        "box-shadow:0 1px 4px rgba(0,0,0,0.4);backdrop-filter:blur(4px);";
      host.appendChild(el);
    }
    return el;
  }

  function showRadarFrame(i) {
    if (!radarFrames.length) return;
    const n = radarFrames.length;
    radarIdx = ((i % n) + n) % n;
    const f = radarFrames[radarIdx];
    const src = map.getSource("gi-radar");
    if (src && src.setTiles) src.setTiles([f.url]);
    const el = radarClock();
    if (el) {
      const d = new Date(f.time);
      const hhmm =
        String(d.getHours()).padStart(2, "0") + ":" + String(d.getMinutes()).padStart(2, "0");
      // Mark the frame nearest "now" as the live observation; later frames are
      // the nowcast (a short forecast of where the rain is heading).
      const ahead = d.getTime() - Date.now();
      const tag = Math.abs(ahead) < 4 * 60 * 1000 ? "nå " : ahead > 0 ? "+ " : "";
      el.textContent = "☔ " + tag + hhmm;
    }
  }

  // Recursive setTimeout (not setInterval) so we can pause a beat at the end of
  // the loop before restarting, like a radar timeline.
  function scheduleRadarFrame() {
    const atEnd = radarIdx === radarFrames.length - 1;
    radarTimer = setTimeout(
      () => {
        if (!radarEnabled || !map.getSource("gi-radar")) {
          stopRadarAnim();
          return;
        }
        showRadarFrame(atEnd ? 0 : radarIdx + 1);
        scheduleRadarFrame();
      },
      atEnd ? 1300 : 600,
    );
  }

  function startRadar(meta) {
    if (!radarEnabled || !ready || !map) return;
    radarFrames = (meta.times || [])
      .map((t) => ({ url: t.tiles && (t.tiles.webp || t.tiles.png), time: t.time }))
      .filter((f) => f.url)
      .sort((a, b) => new Date(a.time) - new Date(b.time));
    if (!radarFrames.length) throw new Error("no radar frames in manifest");
    removeRadar();
    map.addSource("gi-radar", {
      type: "raster",
      tiles: [radarFrames[0].url],
      tileSize: 256,
      minzoom: meta.minzoom || 0,
      maxzoom: meta.maxzoom || 6, // data's native max; MapLibre overzooms past it
      bounds: meta.bounds,
      attribution: "© MET Norway / Yr",
    });
    const before = map.getLayer("gi-ring-fill") ? "gi-ring-fill" : undefined;
    map.addLayer(
      {
        id: "gi-radar",
        type: "raster",
        source: "gi-radar",
        // A short fade gives a smooth crossfade between animation frames.
        paint: { "raster-opacity": 0.7, "raster-fade-duration": 200 },
      },
      before,
    );
    // Begin at the frame nearest "now", then play forward and loop.
    let startIdx = 0;
    let best = Infinity;
    radarFrames.forEach((f, i) => {
      const diff = Math.abs(new Date(f.time).getTime() - Date.now());
      if (diff < best) {
        best = diff;
        startIdx = i;
      }
    });
    showRadarFrame(startIdx);
    scheduleRadarFrame();
  }

  function setRadar(enabled) {
    radarEnabled = !!enabled;
    if (!ready || !map) return;
    if (!radarEnabled) {
      removeRadar();
      return;
    }
    fetch(RADAR_AVAILABLE)
      .then((r) => r.json())
      .then(startRadar)
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
      // Auto mode may have requested the radar before the map finished loading;
      // setRadar stored the flag but couldn't act. Apply it now.
      if (radarEnabled) setRadar(true);
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
