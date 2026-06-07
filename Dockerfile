# ---- builder ----
# cargo-leptos downloads matching wasm-bindgen-cli and the Tailwind standalone
# binary itself, so no Node toolchain is needed in the image.
FROM rust:1.96-bookworm AS builder

# cmake + a C toolchain are needed to build aws-lc-rs (reqwest 0.13's default
# rustls crypto provider).
RUN apt-get update \
    && apt-get install -y --no-install-recommends cmake \
    && rm -rf /var/lib/apt/lists/*

RUN rustup target add wasm32-unknown-unknown \
    && cargo install cargo-leptos --version 0.3.6 --locked

WORKDIR /app
COPY . .

# Builds the wasm client (profile: wasm-release) + the ssr server (release),
# emitting target/site (pkg + assets) and target/release/grusindeks-web.
RUN cd crates/grusindeks-web && cargo leptos build --release

# ---- runtime ----
FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /app/target/release/grusindeks-web /app/grusindeks-web
COPY --from=builder /app/target/site /app/site

ENV LEPTOS_OUTPUT_NAME=grusindeks-web \
    LEPTOS_SITE_ROOT=/app/site \
    LEPTOS_SITE_PKG_DIR=pkg \
    LEPTOS_SITE_ADDR=0.0.0.0:3000 \
    GRUSINDEKS_DB=sqlite:///data/grusindeks.db \
    GRUSINDEKS_CACHE_DIR=/data/cache

EXPOSE 3000
VOLUME ["/data"]

# SQLite DB + MET disk cache live under /data so they survive restarts.
CMD ["/app/grusindeks-web"]
