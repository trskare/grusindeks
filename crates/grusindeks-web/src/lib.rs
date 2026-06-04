#![recursion_limit = "512"]

//! Leptos web GUI for grusindeks.
//!
//! The crate compiles two ways:
//! * **ssr** — the Axum server binary ([`main`]); reuses `grusindeks-cli`'s
//!   orchestration and `grusindeks-met`'s HTTP clients.
//! * **hydrate** — a wasm bundle that hydrates the server-rendered HTML.
//!
//! Shared, wasm-safe types come from `grusindeks-core`; the heavy async/HTTP
//! machinery is confined to the `ssr` feature so it never reaches the client.

pub mod app;
pub mod color;
pub mod components;
pub mod dto;
pub mod icons;
pub mod map;
pub mod server;

// Server-only modules (HTTP client, SQLite). Never compiled for wasm.
#[cfg(feature = "ssr")]
pub mod db;
#[cfg(feature = "ssr")]
pub mod state;

/// wasm entry point — hydrates the body that the server rendered.
#[cfg(feature = "hydrate")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn hydrate() {
    use crate::app::App;
    console_error_panic_hook::set_once();
    leptos::mount::hydrate_body(App);
}
