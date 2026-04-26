//! Domain types and Grusindeks scoring for the `grusindeks` weather/cycling app.
//!
//! This crate has zero I/O — it deals only in pure data and computation, so
//! every module is trivially unit-testable. Network/parsing concerns live in
//! `grusindeks-met`; the CLI binary lives in `grusindeks-cli`.

pub mod daily;
pub mod drying;
pub mod geo;
pub mod score;
pub mod types;
