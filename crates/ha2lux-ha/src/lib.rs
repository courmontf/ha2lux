//! ha2lux's Home Assistant client: REST reads, persistent WebSocket, link health.
//!
//! The token is treated as a secret: opaque type ([`config::Token`]), never in
//! a URL, a log or an error message, never resent after a redirect.
#![forbid(unsafe_code)]

pub mod backoff;
pub mod config;
pub mod error;
pub mod reading;
pub mod rest;
pub mod sensors;
pub mod source;
pub mod ws;
