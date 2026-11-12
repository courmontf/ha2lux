//! Source d'inactivité Wayland : protocole `ext-idle-notify-v1`.
//!
//! La connexion vit dans un thread dédié (boucle d'événements Wayland avec un
//! `poll` borné, pour pouvoir s'arrêter) ; les changements d'état sont renvoyés
//! au runtime par un canal. Le compositeur compte l'inactivité de la **session**
//! (clavier, souris, tactile) et respecte ses propres inhibiteurs d'inactivité.

use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use thiserror::Error;
use tokio::sync::mpsc;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols::ext::idle_notify::v1::client::ext_idle_notification_v1::{
    self, ExtIdleNotificationV1,
};
use wayland_protocols::ext::idle_notify::v1::client::ext_idle_notifier_v1::ExtIdleNotifierV1;

use crate::idle::{IdleEvent, IdleSource};

/// Impossible d'obtenir une source Wayland.
#[derive(Debug, Error)]
pub enum WaylandError {
    /// Pas de session Wayland ou connexion refusée.
    #[error("Wayland connection failed: {0}")]
    Connect(String),
    /// Le compositeur n'expose pas `ext_idle_notifier_v1` (ou pas de `wl_seat`).
    #[error("the compositor does not provide {0}")]
    MissingGlobal(&'static str),
    /// Erreur de protocole.
    #[error("Wayland error: {0}")]
    Protocol(String),
}

struct State {
    events: mpsc::UnboundedSender<IdleEvent>,
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as wayland_client::Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlSeat,
        _: <WlSeat as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtIdleNotifierV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ExtIdleNotifierV1,
        _: <ExtIdleNotifierV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtIdleNotificationV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ExtIdleNotificationV1,
        event: ext_idle_notification_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let mapped = match event {
            ext_idle_notification_v1::Event::Idled => IdleEvent::Idled,
            ext_idle_notification_v1::Event::Resumed => IdleEvent::Resumed,
            _ => return,
        };
        // Le récepteur disparaît à l'arrêt de la source : rien à faire.
        let _ = state.events.send(mapped);
    }
}

/// Source d'inactivité Wayland.
pub struct WaylandIdle {
    events: mpsc::UnboundedReceiver<IdleEvent>,
    stop: Arc<AtomicBool>,
}

impl WaylandIdle {
    /// Se connecte au compositeur de la session (`WAYLAND_DISPLAY`).
    pub fn connect(threshold_ms: u32) -> Result<Self, WaylandError> {
        let connection =
            Connection::connect_to_env().map_err(|e| WaylandError::Connect(e.to_string()))?;
        Self::start(connection, threshold_ms)
    }

    /// Se connecte par un socket déjà ouvert (compositeur de test).
    pub fn from_stream(stream: UnixStream, threshold_ms: u32) -> Result<Self, WaylandError> {
        let connection =
            Connection::from_socket(stream).map_err(|e| WaylandError::Connect(e.to_string()))?;
        Self::start(connection, threshold_ms)
    }

    fn start(connection: Connection, threshold_ms: u32) -> Result<Self, WaylandError> {
        let (events_tx, events) = mpsc::unbounded_channel();
        let (globals, mut queue) = registry_queue_init::<State>(&connection)
            .map_err(|e| WaylandError::Protocol(e.to_string()))?;
        let qh = queue.handle();
        let notifier: ExtIdleNotifierV1 = globals.bind(&qh, 1..=1, ()).map_err(|_| {
            WaylandError::MissingGlobal("ext_idle_notifier_v1 (ext-idle-notify-v1)")
        })?;
        let seat: WlSeat = globals
            .bind(&qh, 1..=9, ())
            .map_err(|_| WaylandError::MissingGlobal("wl_seat"))?;
        let _notification = notifier.get_idle_notification(threshold_ms, &seat, &qh, ());
        let mut state = State { events: events_tx };
        queue
            .roundtrip(&mut state)
            .map_err(|e| WaylandError::Protocol(e.to_string()))?;

        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = Arc::clone(&stop);
        std::thread::Builder::new()
            .name("ha2lux-wayland-idle".to_owned())
            .spawn(move || {
                // Garde la notification vivante tant que le thread tourne.
                let _keep = (_notification, notifier, seat);
                let poll_timeout = Timespec {
                    tv_sec: 0,
                    tv_nsec: 250_000_000,
                };
                while !stop_flag.load(Ordering::Acquire) {
                    if queue.dispatch_pending(&mut state).is_err() || connection.flush().is_err() {
                        break;
                    }
                    let Some(guard) = queue.prepare_read() else {
                        continue; // des événements sont déjà en attente
                    };
                    let readable = {
                        let fd = guard.connection_fd();
                        let mut fds = [PollFd::new(&fd, PollFlags::IN)];
                        matches!(poll(&mut fds, Some(&poll_timeout)), Ok(n) if n > 0)
                    };
                    if readable && guard.read().is_err() {
                        break; // connexion perdue : le canal se ferme, le pilote relancera
                    }
                }
            })
            .map_err(|e| WaylandError::Protocol(e.to_string()))?;
        Ok(Self { events, stop })
    }
}

impl Drop for WaylandIdle {
    fn drop(&mut self) {
        // Le thread s'arrête de lui-même au prochain tour de boucle (≤ 250 ms).
        self.stop.store(true, Ordering::Release);
    }
}

impl IdleSource for WaylandIdle {
    async fn next_event(&mut self) -> Option<IdleEvent> {
        self.events.recv().await
    }
}

/// Une demande d'attente : utile pour borner la durée d'une connexion dans un test.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
