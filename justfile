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
    cargo run --quiet -p medvind-cli -- {{args}}

score *args:
    cargo run --quiet -p medvind-cli -- score {{args}}

install:
    cargo install --path crates/medvind-cli --locked

clean:
    cargo clean

# Refresh the captured MET fixtures from the live API.
# Set MEDVIND_DEV_CONTACT (email or URL) — MET TOS requires a reachable contact in the User-Agent.
fixtures:
    @if [ -z "${MEDVIND_DEV_CONTACT:-}" ]; then echo "set MEDVIND_DEV_CONTACT=you@example.com (or a URL) first" >&2; exit 1; fi
    curl -sS -A "medvind-fixtures/0.1 $MEDVIND_DEV_CONTACT" \
        "https://api.met.no/weatherapi/locationforecast/2.0/complete?lat=59.9139&lon=10.7522" \
        -o fixtures/locationforecast_oslo.json
    curl -sS -A "medvind-fixtures/0.1 $MEDVIND_DEV_CONTACT" \
        "https://api.met.no/weatherapi/nowcast/2.0/complete?lat=59.9139&lon=10.7522" \
        -o fixtures/nowcast_oslo.json
