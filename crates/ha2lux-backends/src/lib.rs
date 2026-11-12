//! Backends matériels de ha2lux : DDC/CI, backlight, backend factice, et la
//! tâche d'écran qui les pilote.
// Aucun `unsafe` n'est nécessaire : il est interdit (accès I²C par `read`/`write` sur le fichier).
#![forbid(unsafe_code)]

pub mod any;
pub mod backlight;
pub mod ddc;
pub mod discovery;
pub mod doctor;
pub mod drm;
pub mod i2c;
pub mod mock;
pub mod task;
