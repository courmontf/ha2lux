//! Hardware backends of ha2lux: DDC/CI, backlight, mock backend, and the
//! display task that drives them.
// No `unsafe` is needed: it is forbidden (I²C access through `read`/`write` on the file).
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
