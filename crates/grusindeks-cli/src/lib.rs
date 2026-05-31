//! Application/service layer shared by the `grusindeks` terminal binary and
//! the `grusindeks-web` server.
//!
//! The reusable pieces — multi-point orchestration ([`run`]), the
//! serializable score aggregates ([`aggregate`]), the user config shape
//! ([`config`]), and the local-time→UTC window builders ([`windows`]) — live
//! here so both front-ends call **one** implementation and can never drift on
//! timezone or scoring semantics.
//!
//! The terminal-only presentation layer ([`output`], [`progress`], [`theme`])
//! sits behind the default `cli` feature; library consumers that don't render
//! to a terminal (the web server) depend on this crate with
//! `default-features = false` and skip the `clap`/`indicatif`/`owo-colors`
//! dependency tree entirely.

pub mod aggregate;
pub mod config;
pub mod run;
pub mod windows;

#[cfg(feature = "cli")]
pub mod output;
#[cfg(feature = "cli")]
pub mod progress;
#[cfg(feature = "cli")]
pub mod theme;
