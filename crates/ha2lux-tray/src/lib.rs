//! ha2lux notification area icon (StatusNotifierItem via `ksni`).
//!
//! - [`model`]: displayed state and **pure** menu description (testable without a bus);
//! - [`wheel`]: accumulation of wheel notches (applied in one go);
//! - [`icon`]: icon image, drawn by the program;
//! - [`osd`]: level display through a replaceable notification;
//! - [`app`]: run loop (daemon D-Bus client, icon, OSD).
#![forbid(unsafe_code)]

pub mod app;
pub mod icon;
pub mod model;
pub mod osd;
pub mod wheel;
