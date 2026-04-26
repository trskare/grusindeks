//! Domain types and Grusindeks scoring for the `medvind` weather/cycling app.
//!
//! This crate has zero I/O — it deals only in pure data and computation, so
//! every module is trivially unit-testable. Network/parsing concerns live in
//! `medvind-met`; the CLI binary lives in `medvind-cli`.

pub mod drying;
pub mod geo;
pub mod score;
pub mod types;
