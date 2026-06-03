//! Multi-point score aggregation.
//!
//! The aggregate types and their constructors are **pure** (they depend only
//! on `grusindeks-core` domain types, `chrono`, and `serde`), so they live in
//! `grusindeks-core::aggregate` where the wasm web client can deserialize them
//! without pulling the HTTP/async dependency tree. This module re-exports them
//! so existing `crate::aggregate::…` paths in `run`/`output` keep working.

pub use grusindeks_core::aggregate::*;
