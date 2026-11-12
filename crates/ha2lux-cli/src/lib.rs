//! `ha2lux` CLI: talks to the daemon over D-Bus; a single command per call.
//!
//! Exit codes: 0 success · 1 usage · 2 daemon absent · 3 command failure ·
//! 4 daemon not answering. The same validations exist in the daemon, which remains
//! the authority: the CLI only provides an earlier and clearer error message.
#![forbid(unsafe_code)]

pub mod cli;
pub mod client;
pub mod output;
pub mod run;
pub mod select;
