//! Mock backend: an in-memory, scriptable display, with no I/O at all.
//!
//! Its futures complete immediately; they can therefore be driven
//! without a runtime in tests (see `tests/support`).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::time::Instant;

use ha2lux_core::{BackendError, DisplayBackend, DisplayId, DisplayInfo, DisplayKind};

#[derive(Debug)]
struct State {
    max: u32,
    raw: u32,
    writes: Vec<u32>,
    write_times: Vec<(Instant, u32)>,
    attempts: u32,
    latency: Duration,
    hang: bool,
    reads: u32,
    scripted_failures: VecDeque<BackendError>,
    fail_always: Option<BackendError>,
}

/// Control handle shared with the backend: used to inspect
/// writes and to script failures from the test.
#[derive(Clone, Debug)]
pub struct MockControl(Arc<Mutex<State>>);

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

impl MockControl {
    /// Raw values written successfully, in order.
    pub fn writes(&self) -> Vec<u32> {
        lock(&self.0).writes.clone()
    }

    /// Successful writes with their instant (tokio clock, virtual under `start_paused`).
    pub fn write_times(&self) -> Vec<(Instant, u32)> {
        lock(&self.0).write_times.clone()
    }

    /// Total number of write attempts, successful or not.
    pub fn attempts(&self) -> u32 {
        lock(&self.0).attempts
    }

    /// Adds latency to each write (tokio time).
    pub fn set_latency(&self, latency: Duration) {
        lock(&self.0).latency = latency;
    }

    /// Reads and writes never complete (a display blocking the kernel).
    pub fn set_hang(&self, hang: bool) {
        lock(&self.0).hang = hang;
    }

    /// Current raw value of the simulated display.
    pub fn raw(&self) -> u32 {
        lock(&self.0).raw
    }

    /// Forces the current raw value (for example the display resetting to 100%).
    pub fn set_raw(&self, raw: u32) {
        lock(&self.0).raw = raw;
    }

    /// Number of reads performed.
    pub fn reads(&self) -> u32 {
        lock(&self.0).reads
    }

    /// Makes the next `count` write operations fail.
    pub fn fail_next_writes(&self, count: usize, error: BackendError) {
        let mut s = lock(&self.0);
        s.scripted_failures
            .extend(std::iter::repeat_n(error, count));
    }

    /// Makes all writes fail as long as `error` is `Some`.
    pub fn fail_always(&self, error: Option<BackendError>) {
        lock(&self.0).fail_always = error;
    }
}

/// Mock display implementing [`DisplayBackend`].
#[derive(Debug)]
pub struct MockBackend {
    info: DisplayInfo,
    state: Arc<Mutex<State>>,
}

impl MockBackend {
    /// Creates a mock display with maximum `max`, initially at `raw`.
    pub fn new(id: &str, kind: DisplayKind, max: u32, raw: u32) -> (Self, MockControl) {
        let state = Arc::new(Mutex::new(State {
            max,
            raw: raw.min(max),
            writes: Vec::new(),
            write_times: Vec::new(),
            attempts: 0,
            latency: Duration::ZERO,
            hang: false,
            reads: 0,
            scripted_failures: VecDeque::new(),
            fail_always: None,
        }));
        let backend = Self {
            info: DisplayInfo {
                id: DisplayId::new(id),
                name: format!("Mock {id}"),
                kind,
                max,
            },
            state: Arc::clone(&state),
        };
        (backend, MockControl(state))
    }
}

impl MockBackend {
    /// Creates a backend attached to an existing mock display: the hardware state
    /// (raw value, writes) survives the backend being recreated, just as a real
    /// display survives a rescan.
    pub fn attach(info: DisplayInfo, control: &MockControl) -> Self {
        Self {
            info,
            state: Arc::clone(&control.0),
        }
    }
}

impl DisplayBackend for MockBackend {
    fn info(&self) -> &DisplayInfo {
        &self.info
    }

    async fn probe(&mut self) -> Result<DisplayInfo, BackendError> {
        let s = lock(&self.state);
        if s.max == 0 {
            return Err(BackendError::Invalid("maximum nul".to_owned()));
        }
        Ok(self.info.clone())
    }

    async fn read(&mut self) -> Result<u32, BackendError> {
        let hang = {
            let mut s = lock(&self.state);
            s.reads += 1;
            s.hang
        };
        if hang {
            std::future::pending::<()>().await;
        }
        Ok(lock(&self.state).raw)
    }

    async fn write(&mut self, raw: u32) -> Result<(), BackendError> {
        let (latency, hang) = {
            let mut s = lock(&self.state);
            s.attempts += 1;
            (s.latency, s.hang)
        };
        if hang {
            std::future::pending::<()>().await;
        }
        if !latency.is_zero() {
            tokio::time::sleep(latency).await;
        }
        let mut s = lock(&self.state);
        if let Some(error) = s.scripted_failures.pop_front() {
            return Err(error);
        }
        if let Some(error) = s.fail_always.clone() {
            return Err(error);
        }
        if raw > s.max {
            return Err(BackendError::Invalid(format!(
                "valeur {raw} au-dessus du maximum {}",
                s.max
            )));
        }
        s.raw = raw;
        s.writes.push(raw);
        s.write_times.push((Instant::now(), raw));
        Ok(())
    }
}
