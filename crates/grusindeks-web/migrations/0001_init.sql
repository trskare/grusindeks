-- Initial schema. Multiuser-ready: every domain row is scoped by `user_id`
-- with a single seeded user (id = 1). Auth, when added, just replaces the
-- hardcoded DEFAULT_USER_ID with the session user — no schema change.
--
-- PRAGMAs (WAL, foreign_keys, busy_timeout) are set per-connection at pool
-- creation, not here (PRAGMAs in migrations are unreliable).

CREATE TABLE users (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL DEFAULT 'default',
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE places (
    id              INTEGER PRIMARY KEY,
    user_id         INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name            TEXT NOT NULL,
    lat             REAL NOT NULL,
    lon             REAL NOT NULL,
    radius_km       REAL NOT NULL DEFAULT 20.0,
    frost_source_id TEXT,
    UNIQUE(user_id, name)
);

CREATE TABLE prefs (
    user_id            INTEGER PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    user_agent_contact TEXT NOT NULL,
    default_place_id   INTEGER REFERENCES places(id) ON DELETE SET NULL,
    language           TEXT NOT NULL DEFAULT 'norwegian',
    daytime_start      TEXT NOT NULL DEFAULT '10:00',
    daytime_end        TEXT NOT NULL DEFAULT '22:00',
    show_rain_history  INTEGER NOT NULL DEFAULT 1,
    show_window_stats  INTEGER NOT NULL DEFAULT 1,
    frost_client_id    TEXT,
    frost_source_id    TEXT
);

CREATE TABLE work_hours (
    user_id      INTEGER PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    enabled      INTEGER NOT NULL DEFAULT 0,
    days_csv     TEXT NOT NULL DEFAULT 'mon,tue,wed,thu,fri',
    window_start TEXT NOT NULL DEFAULT '08:00',
    window_end   TEXT NOT NULL DEFAULT '15:00'
);

CREATE TABLE score_history (
    id                    INTEGER PRIMARY KEY,
    user_id               INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    place_id              INTEGER NOT NULL REFERENCES places(id) ON DELETE CASCADE,
    kind                  TEXT NOT NULL,             -- 'score' | 'forecast_day' | 'hourly'
    target_time           TEXT NOT NULL,             -- UTC RFC3339 window/day start
    computed_at           TEXT NOT NULL DEFAULT (datetime('now')),
    mean                  INTEGER NOT NULL,
    min                   INTEGER NOT NULL,
    max                   INTEGER NOT NULL,
    bd_temperature        INTEGER,
    bd_wind               INTEGER,
    bd_precipitation      INTEGER,
    bd_precip_probability INTEGER,
    bd_ground             INTEGER,
    confidence            TEXT,
    UNIQUE(user_id, place_id, kind, target_time)
);

CREATE INDEX idx_history_lookup ON score_history(user_id, place_id, kind, target_time);

-- Seed a single user with a starter Oslo place so the dashboard works on
-- first boot. The contact is a placeholder; the operator sets a real one via
-- the settings page (or the GRUSINDEKS_CONTACT env var on startup).
INSERT INTO users (id, name) VALUES (1, 'default');
INSERT INTO places (id, user_id, name, lat, lon, radius_km)
    VALUES (1, 1, 'oslo', 59.9139, 10.7522, 20.0);
INSERT INTO prefs (user_id, user_agent_contact, default_place_id)
    VALUES (1, 'CHANGE_ME@example.com', 1);
INSERT INTO work_hours (user_id) VALUES (1);
