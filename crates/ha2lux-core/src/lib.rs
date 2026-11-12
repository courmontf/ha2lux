//! Pure core of ha2lux: deterministic business rules, with no I/O,
//! no system clock and no async runtime.
#![forbid(unsafe_code)]

pub mod backend;
pub mod clock;
pub mod config;
pub mod curve;
pub mod ddc;
pub mod edid;
pub mod filter;
pub mod gate;
pub mod health;
pub mod hw;
pub mod machine;
pub mod range;
pub mod rescan;
pub mod schedule;
pub mod state;
pub mod url;

pub use backend::{BackendError, DisplayBackend, DisplayId, DisplayInfo, DisplayKind};
pub use clock::{Clock, Now, VirtualClock};
pub use curve::{Curve, CurvePoint, LearnOutcome, LearnPolicy};
pub use edid::{Edid, EdidError};
pub use filter::{Filter, FilterParams};
pub use gate::{Gate, GateParams};
pub use health::{HealthConfig, HealthTracker, is_valid_lux};
pub use machine::{
    AutoInfo, BaseMode, Command, ControlError, Controller, ControllerConfig, DisplayState, Event,
    Feature, HaHealth, InitialState, Listed, Notice, Persist, Status,
};
pub use range::{MIN_RANGE_WIDTH, Range, clamp_percent, reconstruct_master};
pub use rescan::{
    Action as RescanAction, RescanConfig, RescanManager, ScanOutcome, Trigger as RescanTrigger,
};
pub use schedule::{MAX_POINTS, MinuteOfDay, Point, Schedule, ScheduleError, Suspension};
pub use state::State;
