//! ha2lux user daemon: decision actor, atomic persistence, D-Bus service.
//!
//! Architecture (see `specs/ARCHITECTURE.md`): a **single decision actor**
//! owns the state machine; D-Bus, timers, display tasks and
//! system events send it messages. No blocking I/O in
//! the actor.
#![forbid(unsafe_code)]

pub mod actor;
pub mod api;
pub mod blockers;
pub mod clock;
pub mod dbus;
pub mod dbus_idle;
pub mod ha;
pub mod hardware;
pub mod idle;
pub mod paths;
pub mod service;
pub mod store;
pub mod system;
pub mod wayland;
