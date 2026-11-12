//! Faux moniteur DDC/CI : décode les vraies trames et répond comme un écran.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::{HashMap, VecDeque};
use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use ha2lux_backends::i2c::{I2cDevice, I2cOpener};
use ha2lux_core::ddc::{decode_request, encode_get_vcp_reply, encode_null_message};

pub const EREMOTEIO: i32 = 121;
pub const ENXIO: i32 = 6;
pub const EACCES: i32 = 13;
pub const ENODEV: i32 = 19;

#[derive(Default)]
pub struct State {
    pub current: u16,
    pub max: u16,
    pub pending: Option<Vec<u8>>,
    pub null_messages: usize,
    pub corrupt_next: usize,
    pub unsupported: bool,
    pub io_errors: VecDeque<i32>,
    pub frames: Vec<Vec<u8>>,
    pub set_values: Vec<u16>,
    pub hung: bool,
}

/// Instrumentation d'un bus : combien d'opérations s'y chevauchent.
#[derive(Default)]
pub struct Bus {
    pub active: AtomicUsize,
    pub max_active: AtomicUsize,
}

#[derive(Clone)]
pub struct MonitorControl {
    pub state: Arc<Mutex<State>>,
    pub gate: Arc<(Mutex<bool>, Condvar)>,
    pub bus: Arc<Bus>,
    /// Notifié quand un thread vient de se bloquer sur l'écran coincé.
    pub entered: Arc<tokio::sync::Notify>,
}

impl MonitorControl {
    pub fn new(current: u16, max: u16) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                current,
                max,
                ..State::default()
            })),
            gate: Arc::new((Mutex::new(true), Condvar::new())),
            bus: Arc::new(Bus::default()),
            entered: Arc::new(tokio::sync::Notify::new()),
        }
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
        f(&mut self.state.lock().unwrap_or_else(PoisonError::into_inner))
    }

    pub fn current(&self) -> u16 {
        self.with(|s| s.current)
    }

    pub fn set_values(&self) -> Vec<u16> {
        self.with(|s| s.set_values.clone())
    }

    pub fn frames(&self) -> Vec<Vec<u8>> {
        self.with(|s| s.frames.clone())
    }

    pub fn get_requests(&self) -> usize {
        self.frames()
            .iter()
            .filter(|f| f.get(2) == Some(&0x01))
            .count()
    }

    /// Bloque (`true`) ou libère (`false`) les écritures : écran coincé dans le noyau.
    pub fn hang(&self, hung: bool) {
        *self.gate.0.lock().unwrap_or_else(PoisonError::into_inner) = !hung;
        self.with(|s| s.hung = hung);
        self.gate.1.notify_all();
    }

    pub fn max_concurrency(&self) -> usize {
        self.bus.max_active.load(Ordering::SeqCst)
    }
}

pub struct FakeMonitor(MonitorControl);

fn os(errno: i32) -> io::Error {
    io::Error::from_raw_os_error(errno)
}

impl I2cDevice for FakeMonitor {
    fn write(&mut self, data: &[u8]) -> io::Result<()> {
        let control = &self.0;
        let now_active = control.bus.active.fetch_add(1, Ordering::SeqCst) + 1;
        control
            .bus
            .max_active
            .fetch_max(now_active, Ordering::SeqCst);
        // Attente active (pas de sleep) : rend un chevauchement probable sans verrou.
        let start = Instant::now();
        while start.elapsed() < Duration::from_micros(500) {
            std::hint::spin_loop();
        }
        // Écran coincé : on attend la libération.
        {
            let (flag, cvar) = &*control.gate;
            let mut open = flag.lock().unwrap_or_else(PoisonError::into_inner);
            if !*open {
                control.entered.notify_one();
            }
            while !*open {
                open = cvar.wait(open).unwrap_or_else(PoisonError::into_inner);
            }
        }
        let result = control.with(|s| {
            s.frames.push(data.to_vec());
            if let Some(errno) = s.io_errors.pop_front() {
                return Err(os(errno));
            }
            match decode_request(data) {
                Some((0x01, vcp, None)) => {
                    let mut reply = if s.null_messages > 0 {
                        s.null_messages -= 1;
                        encode_null_message().to_vec()
                    } else if s.unsupported {
                        let mut frame = encode_get_vcp_reply(vcp, s.current, s.max).to_vec();
                        frame[3] = 0x01;
                        frame[10] = frame[..10].iter().fold(0x50, |a, b| a ^ b);
                        frame
                    } else {
                        encode_get_vcp_reply(vcp, s.current, s.max).to_vec()
                    };
                    if s.corrupt_next > 0 {
                        s.corrupt_next -= 1;
                        let last = reply.len() - 1;
                        reply[last] ^= 0xFF;
                    }
                    s.pending = Some(reply);
                }
                Some((0x03, _vcp, Some(value))) if value <= s.max => {
                    s.current = value;
                    s.set_values.push(value);
                }
                _ => {}
            }
            Ok(())
        });
        control.bus.active.fetch_sub(1, Ordering::SeqCst);
        result
    }

    fn read_exact(&mut self, buf: &mut [u8]) -> io::Result<()> {
        let reply = self.0.with(|s| s.pending.take());
        match reply {
            Some(bytes) => {
                buf.fill(0);
                let n = bytes.len().min(buf.len());
                buf[..n].copy_from_slice(&bytes[..n]);
                Ok(())
            }
            None => Err(os(EREMOTEIO)),
        }
    }
}

/// EEPROM EDID factice (adresse 0x50).
pub struct FakeEdid(Vec<u8>);

impl I2cDevice for FakeEdid {
    fn write(&mut self, _data: &[u8]) -> io::Result<()> {
        Ok(())
    }

    fn read_exact(&mut self, buf: &mut [u8]) -> io::Result<()> {
        buf.fill(0);
        let n = self.0.len().min(buf.len());
        buf[..n].copy_from_slice(&self.0[..n]);
        Ok(())
    }
}

/// Ouvreur factice : un moniteur DDC/CI (0x37) et/ou un EDID (0x50) par bus.
#[derive(Default)]
pub struct FakeOpener {
    monitors: Mutex<HashMap<u32, MonitorControl>>,
    edids: Mutex<HashMap<u32, Vec<u8>>>,
    open_errors: Mutex<HashMap<u32, i32>>,
    opened: Mutex<Vec<(u32, u16)>>,
}

impl FakeOpener {
    pub fn with_monitor(bus: u32, control: MonitorControl) -> Arc<Self> {
        let opener = Self::default();
        opener
            .monitors
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(bus, control);
        Arc::new(opener)
    }

    pub fn add(&self, bus: u32, control: MonitorControl) {
        self.monitors
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(bus, control);
    }

    pub fn add_edid(&self, bus: u32, edid: Vec<u8>) {
        self.edids
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(bus, edid);
    }

    pub fn fail_open(&self, bus: u32, errno: i32) {
        self.open_errors
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(bus, errno);
    }

    pub fn opens(&self) -> usize {
        self.opened
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// Toutes les ouvertures `(bus, adresse)` effectuées, dans l'ordre.
    pub fn opened(&self) -> Vec<(u32, u16)> {
        self.opened
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl I2cOpener for FakeOpener {
    fn open(&self, bus: u32, address: u16) -> io::Result<Box<dyn I2cDevice>> {
        self.opened
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((bus, address));
        if let Some(errno) = self
            .open_errors
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&bus)
        {
            return Err(os(*errno));
        }
        if address == 0x50 {
            return match self
                .edids
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&bus)
            {
                Some(edid) => Ok(Box::new(FakeEdid(edid.clone()))),
                None => Err(os(ENXIO)), // aucun écran sur ce port
            };
        }
        match self
            .monitors
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&bus)
        {
            Some(control) => Ok(Box::new(FakeMonitor(control.clone()))),
            None => Err(os(ENODEV)),
        }
    }
}
