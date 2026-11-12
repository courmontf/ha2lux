//! Client Home Assistant de ha2lux : lecture REST, WebSocket persistant, santé du lien.
//!
//! Le jeton est traité comme un secret : type opaque ([`config::Token`]), jamais dans
//! une URL, un journal ou un message d'erreur, jamais renvoyé après une redirection.
#![forbid(unsafe_code)]

pub mod backoff;
pub mod config;
pub mod error;
pub mod reading;
pub mod rest;
pub mod sensors;
pub mod source;
pub mod ws;
