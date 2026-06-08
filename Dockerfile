# ---- builder ----
# cargo-leptos downloads the Tailwind standalone binary itself, so no Node
# toolchain is needed in the image.
FROM rust:1.96-bookworm AS builder

# cmake + a C toolchain are needed to build aws-lc-rs (reqwest 0.13's default
# rustls crypto provider).
RUN apt-get update \
    && apt-get install -y --no-install-recommends cmake \
    && rm -rf /var/lib/apt/lists/*

# Pre-install wasm-bindgen-cli (compiled from source, on PATH) so cargo-leptos
# uses it instead of trying to download a prebuilt binary — there's no
# aarch64-linux release asset for every wasm-bindgen version. KEEP THIS VERSION
# IN SYNC with the `wasm-bindgen` version in Cargo.lock.
RUN rustup target add wasm32-unknown-unknown \
    && cargo install cargo-leptos --version 0.3.6 --locked \
    && cargo install wasm-bindgen-cli --version 0.2.122

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
