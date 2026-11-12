//! Hardware discovery, behind a trait so the daemon can be tested
//! with fake displays.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use ha2lux_backends::any::AnyBackend;
use ha2lux_backends::backlight::{self, BacklightBackend, LogindSetter};
use ha2lux_backends::ddc::{DdcBackend, DdcTimings};
use ha2lux_backends::discovery::discover_external;
use ha2lux_backends::i2c::{BusLocks, I2cOpener, LinuxI2cOpener};
use ha2lux_backends::mock::{MockBackend, MockControl};
use ha2lux_core::hw::level_from_raw;
use ha2lux_core::{DisplayBackend, DisplayId, DisplayInfo, DisplayKind};

/// Boxed future (the discovery trait must be usable as an object).
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Maximum duration of a display probe (probe + read).
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// A display found by an enumeration.
pub struct Found {
    /// Identity (the maximum is known when the display is controllable).
    pub info: DisplayInfo,
    /// Backend ready to drive, present only if the display is controllable.
    pub backend: Option<AnyBackend>,
    /// Display level read (0-100).
    pub level: Option<u8>,
    /// I²C bus (DDC), if there is one.
    pub bus: Option<u32>,
    /// Backlight floor as a raw value.
    pub floor: u32,
}

/// Parameters of an enumeration.
#[derive(Clone, Debug, Default)]
pub struct DiscoverOptions {
    /// Match buses by EDID (NVIDIA driver); performs real I²C reads.
    pub edid_bus_matching: bool,
    /// Per-display backlight floors.
    pub floors: HashMap<DisplayId, u32>,
}

/// Display source.
pub trait Hardware: Send + Sync + 'static {
    /// Enumerates the displays (probes each one). May take a while: never called by the actor itself.
    fn discover(&self, options: DiscoverOptions) -> BoxFuture<'_, Vec<Found>>;
}

async fn probe_found(mut backend: AnyBackend, bus: Option<u32>, floor: u32) -> Found {
    let outcome = tokio::time::timeout(PROBE_TIMEOUT, async {
        let info = backend.probe().await?;
        let level = backend
            .read()
            .await
            .ok()
            .map(|raw| level_from_raw(raw, info.max));
        Ok::<_, ha2lux_core::BackendError>((info, level))
    })
    .await;
    match outcome {
        Ok(Ok((info, level))) => Found {
            info,
            backend: Some(backend),
            level,
            bus,
            floor,
        },
        Ok(Err(error)) => {
            tracing::debug!(display = %backend.info().id, %error, "display not controllable");
            Found {
                info: backend.info().clone(),
                backend: None,
                level: None,
                bus,
                floor,
            }
        }
        Err(_) => {
            tracing::debug!(display = %backend.info().id, "probe got no reply");
            Found {
                info: backend.info().clone(),
                backend: None,
                level: None,
                bus,
                floor,
            }
        }
    }
}

/// Real hardware: backlight (sysfs + logind) and DDC/CI (i2c-dev).
pub struct SystemHardware {
    root: PathBuf,
    locks: Arc<BusLocks>,
    opener: Arc<dyn I2cOpener>,
    timings: DdcTimings,
}

impl SystemHardware {
    /// Real system hardware.
    pub fn new() -> Self {
        Self {
            root: PathBuf::from("/"),
            locks: Arc::new(BusLocks::new()),
            opener: Arc::new(LinuxI2cOpener::new()),
            timings: DdcTimings::standard(),
        }
    }
}

impl Default for SystemHardware {
    fn default() -> Self {
        Self::new()
    }
}

impl Hardware for SystemHardware {
    fn discover(&self, options: DiscoverOptions) -> BoxFuture<'_, Vec<Found>> {
        Box::pin(async move {
            let mut found = Vec::new();

            let panels = backlight::scan(&self.root.join("sys/class/backlight")).await;
            if let Some(device) = backlight::preferred(&panels) {
                let backend =
                    AnyBackend::Backlight(BacklightBackend::new(device, LogindSetter::new()));
                let floor = options.floors.get(&backend.info().id).copied().unwrap_or(1);
                found.push(probe_found(backend, None, floor).await);
            }

            let (root, opener, matching) = (
                self.root.clone(),
                Arc::clone(&self.opener),
                options.edid_bus_matching,
            );
            let discovery = tokio::task::spawn_blocking(move || {
                discover_external(&root, opener.as_ref(), matching)
            })
            .await
            .unwrap_or_default();
            for note in &discovery.notes {
                tracing::info!(%note, "DDC discovery");
            }
            for (candidate, source) in discovery.candidates {
                tracing::debug!(display = %candidate.info.id, bus = candidate.bus, ?source, "external display candidate");
                let backend = AnyBackend::Ddc(DdcBackend::new(
                    candidate.info.clone(),
                    candidate.bus,
                    Arc::clone(&self.opener),
                    &self.locks,
                    self.timings,
                ));
                found.push(probe_found(backend, Some(candidate.bus), 0).await);
            }
            found
        })
    }
}

struct MockScreen {
    id: DisplayId,
    kind: DisplayKind,
    max: u32,
    control: MockControl,
    present: bool,
    responds: bool,
}

/// Fake hardware for the daemon tests.
#[derive(Default)]
pub struct MockHardware {
    screens: Mutex<Vec<MockScreen>>,
    discoveries: AtomicUsize,
    delay: Mutex<Duration>,
}

impl MockHardware {
    /// Empty fake hardware.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Adds a display (initial level 0-100) and returns its control handle.
    pub fn add(&self, id: &str, kind: DisplayKind, max: u32, level: u8) -> MockControl {
        let (_, control) = MockBackend::new(
            id,
            kind,
            max,
            ha2lux_core::hw::raw_from_level(i64::from(level), max),
        );
        self.screens
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(MockScreen {
                id: DisplayId::new(id),
                kind,
                max,
                control: control.clone(),
                present: true,
                responds: true,
            });
        control
    }

    fn with_screen(&self, id: &str, f: impl FnOnce(&mut MockScreen)) {
        if let Some(screen) = self
            .screens
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter_mut()
            .find(|s| s.id.as_str() == id)
        {
            f(screen);
        }
    }

    /// Plugs or unplugs a display.
    pub fn set_present(&self, id: &str, present: bool) {
        self.with_screen(id, |s| s.present = present);
    }

    /// The display responds (or not) to the probe.
    pub fn set_responding(&self, id: &str, responds: bool) {
        self.with_screen(id, |s| s.responds = responds);
    }

    /// Simulated duration of an enumeration.
    pub fn set_discover_delay(&self, delay: Duration) {
        *self.delay.lock().unwrap_or_else(PoisonError::into_inner) = delay;
    }

    /// Number of enumerations performed.
    pub fn discoveries(&self) -> usize {
        self.discoveries.load(Ordering::SeqCst)
    }
}

impl Hardware for MockHardware {
    fn discover(&self, _options: DiscoverOptions) -> BoxFuture<'_, Vec<Found>> {
        Box::pin(async move {
            self.discoveries.fetch_add(1, Ordering::SeqCst);
            let delay = *self.delay.lock().unwrap_or_else(PoisonError::into_inner);
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            let screens = self.screens.lock().unwrap_or_else(PoisonError::into_inner);
            screens
                .iter()
                .filter(|s| s.present)
                .map(|s| {
                    let info = DisplayInfo {
                        id: s.id.clone(),
                        name: format!("Mock {}", s.id),
                        kind: s.kind,
                        max: s.max,
                    };
                    if s.responds {
                        let level = Some(level_from_raw(s.control.raw(), s.max));
                        let backend =
                            AnyBackend::Mock(MockBackend::attach(info.clone(), &s.control));
                        Found {
                            info,
                            backend: Some(backend),
                            level,
                            bus: None,
                            floor: 1,
                        }
                    } else {
                        Found {
                            info,
                            backend: None,
                            level: None,
                            bus: None,
                            floor: 1,
                        }
                    }
                })
                .collect()
        })
    }
}
