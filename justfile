default:
    @just --list

test:
    cargo test --workspace

check:
    cargo check --workspace --all-targets

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

lint:
    cargo clippy --workspace --all-targets -- -D warnings

ci: fmt-check lint test

build:
    cargo build --workspace --release

run *args:
    cargo run --quiet -p grusindeks-cli -- {{args}}

install:
    cargo install --path crates/grusindeks-cli --locked

clean:
    cargo clean

# Refresh the captured MET fixtures from the live API.
# Set GRUSINDEKS_DEV_CONTACT (email or URL) — MET TOS requires a reachable contact in the User-Agent.
fixtures:
    @if [ -z "${GRUSINDEKS_DEV_CONTACT:-}" ]; then echo "set GRUSINDEKS_DEV_CONTACT=you@example.com (or a URL) first" >&2; exit 1; fi
    curl -sS -A "grusindeks-fixtures/0.1 $GRUSINDEKS_DEV_CONTACT" \
        "https://api.met.no/weatherapi/locationforecast/2.0/complete?lat=59.9139&lon=10.7522" \
        -o fixtures/locationforecast_oslo.json
    curl -sS -A "grusindeks-fixtures/0.1 $GRUSINDEKS_DEV_CONTACT" \
        "https://api.met.no/weatherapi/nowcast/2.0/complete?lat=59.9139&lon=10.7522" \
        -o fixtures/nowcast_oslo.json

# ---- Web GUI (Leptos) — local development, no Docker ----

# One-time setup: the wasm target + the cargo-leptos build tool.
web-setup:
    rustup target add wasm32-unknown-unknown
    cargo install cargo-leptos --locked

# Resolve the MET contact (api.met.no TOS) for local dev, in order:
#   1. $GRUSINDEKS_DEV_CONTACT
#   2. user_agent_contact from the grusindeks CLI config
#      ($GRUSINDEKS_CONFIG, else the platform default path)
# Prints the contact or exits non-zero. Hidden helper used by `web`/`web-serve`.
_dev-contact:
    #!/usr/bin/env bash
    set -euo pipefail
    contact="${GRUSINDEKS_DEV_CONTACT:-}"
    if [ -z "$contact" ]; then
        cfg="${GRUSINDEKS_CONFIG:-}"
        if [ -z "$cfg" ]; then
            if [ -f "$HOME/Library/Application Support/grusindeks/config.toml" ]; then
                cfg="$HOME/Library/Application Support/grusindeks/config.toml"
            else
                cfg="${XDG_CONFIG_HOME:-$HOME/.config}/grusindeks/config.toml"
            fi
        fi
        if [ -f "$cfg" ]; then
            contact=$(grep -E '^[[:space:]]*user_agent_contact[[:space:]]*=' "$cfg" \
                | head -1 | sed -E 's/^[^=]*=[[:space:]]*"?([^"]*)"?[[:space:]]*$/\1/')
        fi
    fi
    if [ -z "$contact" ]; then
        echo "no MET contact found — set user_agent_contact in your grusindeks config (\`grusindeks config path\`), or GRUSINDEKS_DEV_CONTACT" >&2
        exit 1
    fi
    printf '%s' "$contact"

# Run `just web-setup` once first. The MET contact (api.met.no TOS) is read from
# your grusindeks CLI config, or $GRUSINDEKS_DEV_CONTACT. The dev DB and MET
# response cache live under target/ (gitignored) so they persist across runs.
# Run the web GUI with hot reload at http://127.0.0.1:3000 (Ctrl-C to stop).
web:
    #!/usr/bin/env bash
    set -euo pipefail
    contact="$({{just_executable()}} _dev-contact)"
    cd crates/grusindeks-web
    GRUSINDEKS_CONTACT="$contact" \
        GRUSINDEKS_DB="sqlite://{{justfile_directory()}}/target/grusindeks-dev.db" \
        GRUSINDEKS_CACHE_DIR="{{justfile_directory()}}/target/grusindeks-cache" \
        cargo leptos watch

# Build once and serve, without file watching (same contact resolution as `web`).
web-serve:
    #!/usr/bin/env bash
    set -euo pipefail
    contact="$({{just_executable()}} _dev-contact)"
    cd crates/grusindeks-web
    GRUSINDEKS_CONTACT="$contact" \
        GRUSINDEKS_DB="sqlite://{{justfile_directory()}}/target/grusindeks-dev.db" \
        GRUSINDEKS_CACHE_DIR="{{justfile_directory()}}/target/grusindeks-cache" \
        cargo leptos serve

# Production build of the web GUI (wasm-release + ssr release; same as Docker).
web-build:
    cd crates/grusindeks-web && cargo leptos build --release
