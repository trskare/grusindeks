//! Domain types and Grusindeks scoring for the `grusindeks` weather/cycling app.
//!
//! This crate has zero I/O — it deals only in pure data and computation, so
//! every module is trivially unit-testable. Network/parsing concerns live in
//! `grusindeks-met`; the CLI binary lives in `grusindeks-cli`.

pub mod aggregate;
pub mod daily;
pub mod drying;
pub mod felt_temp;
pub mod geo;
pub mod lang;
pub mod score;
pub mod sun;
pub mod types;
