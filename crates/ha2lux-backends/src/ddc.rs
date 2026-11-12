//! DDC/CI backend (external displays): Get/Set VCP 0x10 through `/dev/i2c-N`.
//!
//! All I²C operations are blocking: they run in
//! `spawn_blocking`, under the **bus lock**, with the protocol delays
//! (about 40 ms between request and reply, about 50 ms between commands).
//!
//! If an operation exceeds the display task's timeout, its thread may
//! stay stuck in the kernel. The backend knows this: until it has
//! finished, any new operation fails immediately (at most one abandoned
//! thread per display) and the bus lock stays held.
//!
//! Usage: call [`DdcBackend::probe`] **before** handing the backend
//! to the display task, to learn the real maximum.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use ha2lux_core::ddc::{
    DDC_ADDRESS, DdcError, GET_VCP_REPLY_LEN, VCP_BRIGHTNESS, VcpReading, encode_get_vcp,
    encode_set_vcp, parse_get_vcp_reply,
};
use ha2lux_core::{BackendError, DisplayBackend, DisplayInfo};

use crate::i2c::{BusLocks, I2cDevice, I2cOpener, classify_io_error};

/// DDC/CI protocol delays (to be confirmed on real hardware).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DdcTimings {
    /// Wait between sending a request and reading the reply.
    pub reply_delay: Duration,
    /// Minimum interval between two commands to the same display.
    pub command_gap: Duration,
    /// Wait between two attempts at the same read.
    pub retry_delay: Duration,
    /// Attempts for a probe or a read.
    pub read_attempts: u32,
}

impl DdcTimings {
    /// Valeurs usuelles : 40 ms, 50 ms, 50 ms, 3 tentatives.
    pub fn standard() -> Self {
        Self {
            reply_delay: Duration::from_millis(40),
            command_gap: Duration::from_millis(50),
            retry_delay: Duration::from_millis(50),
            read_attempts: 3,
        }
    }

    /// No delay (tests with a mock display: no real `sleep`).
    pub fn none() -> Self {
        Self {
            reply_delay: Duration::ZERO,
            command_gap: Duration::ZERO,
            retry_delay: Duration::ZERO,
            read_attempts: 3,
        }
    }
}

/// Time left to wait before the next command.
pub fn remaining_gap(since_last_command: Duration, gap: Duration) -> Duration {
    gap.saturating_sub(since_last_command)
}

#[derive(Clone, Copy)]
enum Op {
    Get(u8),
    Set(u8, u16),
}

#[derive(Default)]
struct DeviceState {
    device: Option<Box<dyn I2cDevice>>,
    last_done: Option<Instant>,
}

fn map_ddc_error(error: DdcError) -> BackendError {
    if error.is_transient() {
        BackendError::Transient(error.to_string())
    } else {
        BackendError::Invalid(error.to_string())
    }
}

/// Runs an operation (blocking). The device is (re)opened as needed.
fn perform(
    state: &mut DeviceState,
    opener: &dyn I2cOpener,
    bus: u32,
    timings: DdcTimings,
    op: Op,
) -> Result<Option<VcpReading>, BackendError> {
    if state.device.is_none() {
        state.device = Some(
            opener
                .open(bus, DDC_ADDRESS)
                .map_err(|e| classify_io_error(&e))?,
        );
    }
    if let Some(last) = state.last_done {
        let wait = remaining_gap(last.elapsed(), timings.command_gap);
        if !wait.is_zero() {
            std::thread::sleep(wait);
        }
    }
    let result = match state.device.as_mut() {
        Some(device) => transact(device.as_mut(), timings, op),
        None => Err(BackendError::Disconnected("I²C bus closed".to_owned())),
    };
    state.last_done = Some(Instant::now());
    if matches!(result, Err(BackendError::Disconnected(_))) {
        state.device = None; // closed; reopened (possibly on another bus) at rescan
    }
    result
}

fn transact(
    device: &mut dyn I2cDevice,
    timings: DdcTimings,
    op: Op,
) -> Result<Option<VcpReading>, BackendError> {
    match op {
        Op::Get(vcp) => {
            device
                .write(&encode_get_vcp(vcp))
                .map_err(|e| classify_io_error(&e))?;
            if !timings.reply_delay.is_zero() {
                std::thread::sleep(timings.reply_delay);
            }
            let mut reply = [0u8; GET_VCP_REPLY_LEN];
            device
                .read_exact(&mut reply)
                .map_err(|e| classify_io_error(&e))?;
            parse_get_vcp_reply(&reply, vcp)
                .map(Some)
                .map_err(map_ddc_error)
        }
        Op::Set(vcp, value) => {
            device
                .write(&encode_set_vcp(vcp, value))
                .map_err(|e| classify_io_error(&e))?;
            Ok(None)
        }
    }
}

/// External display driven over DDC/CI.
pub struct DdcBackend {
    info: DisplayInfo,
    bus: u32,
    opener: Arc<dyn I2cOpener>,
    bus_lock: Arc<tokio::sync::Mutex<()>>,
    timings: DdcTimings,
    state: Arc<Mutex<DeviceState>>,
    busy: Arc<AtomicBool>,
}

impl DdcBackend {
    /// Creates the backend for a display attached to bus `bus`. `info.max` will be set by `probe`.
    pub fn new(
        info: DisplayInfo,
        bus: u32,
        opener: Arc<dyn I2cOpener>,
        locks: &BusLocks,
        timings: DdcTimings,
    ) -> Self {
        Self {
            info,
            bus,
            opener,
            bus_lock: locks.get(bus),
            timings,
            state: Arc::new(Mutex::new(DeviceState::default())),
            busy: Arc::new(AtomicBool::new(false)),
        }
    }

    /// One operation under the bus lock, in a blocking thread.
    async fn run(&self, op: Op) -> Result<Option<VcpReading>, BackendError> {
        if self.busy.load(Ordering::Acquire) {
            return Err(BackendError::Transient(
                "previous operation still running on this bus".to_owned(),
            ));
        }
        let guard = Arc::clone(&self.bus_lock).lock_owned().await;
        // Set only once the lock is obtained: a call cancelled while
        // waiting for the lock therefore never leaves `busy` stuck.
        self.busy.store(true, Ordering::Release);
        let (state, opener, busy) = (
            Arc::clone(&self.state),
            Arc::clone(&self.opener),
            Arc::clone(&self.busy),
        );
        let (bus, timings) = (self.bus, self.timings);
        let handle = tokio::task::spawn_blocking(move || {
            // The bus lock stays held until the operation really ends,
            // even if the caller gave up waiting (timeout).
            let _guard = guard;
            let result = {
                let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
                perform(&mut state, opener.as_ref(), bus, timings, op)
            };
            busy.store(false, Ordering::Release);
            result
        });
        handle
            .await
            .map_err(|e| BackendError::Transient(format!("tâche bloquante interrompue : {e}")))?
    }

    async fn get_with_retries(&self, attempts: u32) -> Result<VcpReading, BackendError> {
        let mut last = BackendError::Transient("aucune tentative".to_owned());
        for attempt in 0..attempts.max(1) {
            match self.run(Op::Get(VCP_BRIGHTNESS)).await {
                Ok(Some(reading)) => return Ok(reading),
                Ok(None) => return Err(BackendError::Invalid("empty response".to_owned())),
                Err(error) if error.is_transient() => {
                    last = error;
                    if attempt + 1 < attempts && !self.timings.retry_delay.is_zero() {
                        tokio::time::sleep(self.timings.retry_delay).await;
                    }
                }
                Err(error) => return Err(error),
            }
        }
        Err(last)
    }
}

impl DisplayBackend for DdcBackend {
    fn info(&self) -> &DisplayInfo {
        &self.info
    }

    /// Controllable iff a valid read of VCP 0x10 **and** maximum > 0. A
    /// "null message" reply right after a wake-up is only a transient failure.
    async fn probe(&mut self) -> Result<DisplayInfo, BackendError> {
        let reading = self.get_with_retries(self.timings.read_attempts).await?;
        if reading.maximum == 0 {
            return Err(BackendError::Invalid(
                "brightness maximum is zero".to_owned(),
            ));
        }
        self.info.max = u32::from(reading.maximum);
        Ok(self.info.clone())
    }

    async fn read(&mut self) -> Result<u32, BackendError> {
        self.get_with_retries(self.timings.read_attempts)
            .await
            .map(|r| u32::from(r.current))
    }

    async fn write(&mut self, raw: u32) -> Result<(), BackendError> {
        if self.info.max == 0 {
            return Err(BackendError::Invalid(
                "display not probed (maximum unknown)".to_owned(),
            ));
        }
        if raw > self.info.max {
            return Err(BackendError::Invalid(format!(
                "valeur {raw} au-dessus du maximum {}",
                self.info.max
            )));
        }
        let value = u16::try_from(raw)
            .map_err(|_| BackendError::Invalid(format!("valeur {raw} hors 16 bits")))?;
        self.run(Op::Set(VCP_BRIGHTNESS, value)).await.map(|_| ())
    }
}
