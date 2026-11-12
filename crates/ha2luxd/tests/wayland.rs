//! Wayland `ext-idle-notify-v1` source against a fake compositor (real protocol, over
//! a socket pair: no environment variable or real session).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, missing_docs)]

use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::mpsc as std_mpsc;
use std::time::Duration;

use ha2luxd::idle::{IdleEvent, IdleSource};
use ha2luxd::wayland::{WaylandError, WaylandIdle};
use wayland_protocols::ext::idle_notify::v1::server::ext_idle_notification_v1::{
    self, ExtIdleNotificationV1,
};
use wayland_protocols::ext::idle_notify::v1::server::ext_idle_notifier_v1::{
    self, ExtIdleNotifierV1,
};
use wayland_server::backend::{ClientData, ClientId, DisconnectReason};
use wayland_server::protocol::wl_seat::{self, WlSeat};
use wayland_server::{Client, DataInit, Dispatch, Display, DisplayHandle, GlobalDispatch, New};

struct NoData;

impl ClientData for NoData {
    fn initialized(&self, _: ClientId) {}
    fn disconnected(&self, _: ClientId, _: DisconnectReason) {}
}

#[derive(Default)]
struct Compositor {
    notifications: Vec<ExtIdleNotificationV1>,
    timeouts: Vec<u32>,
}

impl GlobalDispatch<WlSeat, ()> for Compositor {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        resource: New<WlSeat>,
        _: &(),
        init: &mut DataInit<'_, Self>,
    ) {
        init.init(resource, ());
    }
}

impl Dispatch<WlSeat, ()> for Compositor {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &WlSeat,
        _: wl_seat::Request,
        _: &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
    }
}

impl GlobalDispatch<ExtIdleNotifierV1, ()> for Compositor {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        resource: New<ExtIdleNotifierV1>,
        _: &(),
        init: &mut DataInit<'_, Self>,
    ) {
        init.init(resource, ());
    }
}

impl Dispatch<ExtIdleNotifierV1, ()> for Compositor {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &ExtIdleNotifierV1,
        request: ext_idle_notifier_v1::Request,
        _: &(),
        _: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        if let ext_idle_notifier_v1::Request::GetIdleNotification { id, timeout, .. } = request {
            state.notifications.push(init.init(id, ()));
            state.timeouts.push(timeout);
        }
    }
}

impl Dispatch<ExtIdleNotificationV1, ()> for Compositor {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &ExtIdleNotificationV1,
        _: ext_idle_notification_v1::Request,
        _: &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
    }
}

enum Command {
    Idled,
    Resumed,
    Timeouts(std_mpsc::Sender<Vec<u32>>),
    Quit,
}

/// Starts the fake compositor in a thread; returns the client socket and the command channel.
fn compositor(
    with_notifier: bool,
) -> (
    UnixStream,
    std_mpsc::Sender<Command>,
    std::thread::JoinHandle<()>,
) {
    let (client, server) = UnixStream::pair().unwrap();
    let (commands, inbox) = std_mpsc::channel::<Command>();
    let thread = std::thread::spawn(move || {
        let mut display = Display::<Compositor>::new().unwrap();
        let mut handle = display.handle();
        handle.create_global::<Compositor, WlSeat, ()>(1, ());
        if with_notifier {
            handle.create_global::<Compositor, ExtIdleNotifierV1, ()>(1, ());
        }
        handle.insert_client(server, Arc::new(NoData)).unwrap();
        let mut state = Compositor::default();
        loop {
            display.dispatch_clients(&mut state).unwrap();
            display.flush_clients().unwrap();
            while let Ok(command) = inbox.try_recv() {
                match command {
                    Command::Idled => state
                        .notifications
                        .iter()
                        .for_each(ExtIdleNotificationV1::idled),
                    Command::Resumed => state
                        .notifications
                        .iter()
                        .for_each(ExtIdleNotificationV1::resumed),
                    Command::Timeouts(reply) => {
                        // The requester may have given up waiting: irrelevant.
                        let _ = reply.send(state.timeouts.clone());
                    }
                    Command::Quit => return,
                }
                display.flush_clients().unwrap();
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    });
    (client, commands, thread)
}

async fn connect(stream: UnixStream, threshold: u32) -> Result<WaylandIdle, WaylandError> {
    tokio::task::spawn_blocking(move || WaylandIdle::from_stream(stream, threshold))
        .await
        .unwrap()
}

async fn next(source: &mut WaylandIdle) -> IdleEvent {
    tokio::time::timeout(Duration::from_secs(5), source.next_event())
        .await
        .expect("événement attendu")
        .expect("source ouverte")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_and_resume_events_are_relayed_and_the_timeout_is_the_configured_delay() {
    let (stream, commands, thread) = compositor(true);
    let mut source = connect(stream, 300_000)
        .await
        .expect("le faux compositeur annonce ext_idle_notifier_v1");

    let (reply, timeouts) = std_mpsc::channel();
    commands.send(Command::Timeouts(reply)).unwrap();
    assert_eq!(
        timeouts.recv_timeout(Duration::from_secs(5)).unwrap(),
        [300_000],
        "le compositeur reçoit le délai d'inactivité demandé"
    );

    commands.send(Command::Idled).unwrap();
    assert_eq!(next(&mut source).await, IdleEvent::Idled);
    commands.send(Command::Resumed).unwrap();
    assert_eq!(next(&mut source).await, IdleEvent::Resumed);
    commands.send(Command::Idled).unwrap();
    commands.send(Command::Resumed).unwrap();
    assert_eq!(next(&mut source).await, IdleEvent::Idled);
    assert_eq!(next(&mut source).await, IdleEvent::Resumed);

    drop(source);
    commands.send(Command::Quit).unwrap();
    thread.join().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_compositor_without_the_protocol_is_reported_clearly() {
    let (stream, commands, thread) = compositor(false);
    let Err(error) = connect(stream, 60_000).await else {
        panic!("la connexion aurait dû échouer")
    };
    assert!(matches!(error, WaylandError::MissingGlobal(_)), "{error}");
    assert!(
        error.to_string().contains("ext_idle_notifier_v1"),
        "{error}"
    );
    commands.send(Command::Quit).unwrap();
    thread.join().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_source_ends_when_the_compositor_goes_away() {
    let (stream, commands, thread) = compositor(true);
    let mut source = connect(stream, 60_000).await.unwrap();
    commands.send(Command::Quit).unwrap();
    thread.join().unwrap(); // the compositor goes away: its socket closes
    let ended = tokio::time::timeout(Duration::from_secs(5), source.next_event())
        .await
        .expect("la source doit se terminer");
    assert_eq!(ended, None, "le pilote relancera la source plus tard");
}

#[test]
fn connecting_without_a_wayland_session_is_an_error_not_a_panic() {
    // A socket to nothing: the protocol fails cleanly.
    let (a, b) = UnixStream::pair().unwrap();
    drop(b);
    assert!(WaylandIdle::from_stream(a, 1_000).is_err());
}

/// Smoke test on the session's **real compositor** (read-only: creates an idle
/// notification of 24 h that never fires, then closes). Ignored by default.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "se connecte au compositeur Wayland réel de la session ; à lancer à la main"]
async fn real_compositor_supports_ext_idle_notify() {
    let source = tokio::task::spawn_blocking(|| WaylandIdle::connect(24 * 3_600_000))
        .await
        .unwrap();
    match source {
        Ok(source) => {
            println!("ext-idle-notify-v1 pris en charge par le compositeur réel");
            drop(source);
        }
        Err(error) => panic!("{error}"),
    }
}
