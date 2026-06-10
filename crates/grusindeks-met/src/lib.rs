//! HTTP clients for `api.met.no` (locationforecast, nowcast) and
//! `frost.met.no` (historical observations).
//!
//! This crate is the only place that talks to the network. It enforces the
//! MET Terms of Service in one place: identifying `User-Agent`, 4-decimal
//! coordinate truncation, and `Expires` / `If-Modified-Since` cache
//! revalidation.

pub mod cache;
pub mod client;
pub mod frost;
pub mod locationforecast;
pub mod metalerts;
pub mod nowcast;
