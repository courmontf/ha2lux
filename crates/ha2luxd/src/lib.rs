//! Démon utilisateur ha2lux : acteur décideur, persistance atomique, service D-Bus.
//!
//! Architecture (voir `specs/ARCHITECTURE.md`) : un **unique acteur décideur**
//! possède la machine d'états ; D-Bus, minuteries, tâches d'écran et
//! événements système lui envoient des messages. Aucune E/S bloquante dans
//! l'acteur.
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
