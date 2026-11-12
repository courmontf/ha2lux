//! Surveillance de logind (veille, réveil, verrouillage, session) contre un **faux logind**
//! sur un bus privé, avec le vrai démon et des écrans factices.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, missing_docs)]

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use ha2lux_backends::mock::MockControl;
use ha2lux_backends::task::TaskConfig;
use ha2lux_core::{DisplayKind, MinuteOfDay};
use ha2lux_ipc::{BUS_NAME, DaemonProxy};
use ha2luxd::clock::TokioClock;
use ha2luxd::hardware::MockHardware;
use ha2luxd::paths::Paths;
use ha2luxd::service::{BusKind, ServiceOptions, SystemOptions, run};
use tempfile::TempDir;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::OwnedObjectPath;

struct PrivateBus {
    child: Child,
    address: String,
    _dir: TempDir,
}

impl PrivateBus {
    fn start() -> Option<Self> {
        let dir = TempDir::new().unwrap();
        let config = dir.path().join("bus.conf");
        std::fs::write(
            &config,
            format!(
                "<!DOCTYPE busconfig PUBLIC \"-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN\"\n \"http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd\">\n<busconfig><type>session</type><listen>unix:dir={}</listen><auth>EXTERNAL</auth>\n<policy context=\"default\"><allow send_destination=\"*\" eavesdrop=\"true\"/><allow eavesdrop=\"true\"/><allow own=\"*\"/></policy></busconfig>\n",
                dir.path().display()
            ),
        )
        .unwrap();
        let mut child = Command::new("dbus-daemon")
            .arg(format!("--config-file={}", config.display()))
            .args(["--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let mut address = String::new();
        BufReader::new(child.stdout.take()?)
            .read_line(&mut address)
            .ok()?;
        let address = address.trim().to_owned();
        (!address.is_empty()).then_some(Self {
            child,
            address,
            _dir: dir,
        })
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        // Le bus privé n'a plus d'utilité ; une erreur ici n'a aucune conséquence.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

macro_rules! bus_or_skip {
    () => {
        match PrivateBus::start() {
            Some(bus) => bus,
            None => {
                eprintln!("SKIPPED : dbus-daemon introuvable");
                return;
            }
        }
    };
}

const SESSION: &str = "/org/freedesktop/login1/session/_31";
const USER: &str = "/org/freedesktop/login1/user/_1000";
const MANAGER: &str = "/org/freedesktop/login1";

struct FakeManager;

#[zbus::interface(name = "org.freedesktop.login1.Manager")]
impl FakeManager {
    fn get_session(&self, _id: String) -> OwnedObjectPath {
        OwnedObjectPath::try_from(SESSION).unwrap()
    }

    fn get_user(&self, _uid: u32) -> OwnedObjectPath {
        OwnedObjectPath::try_from(USER).unwrap()
    }

    #[zbus(signal)]
    async fn prepare_for_sleep(emitter: &SignalEmitter<'_>, start: bool) -> zbus::Result<()>;
}

struct FakeSession {
    active: bool,
}

#[zbus::interface(name = "org.freedesktop.login1.Session")]
impl FakeSession {
    #[zbus(property)]
    fn active(&self) -> bool {
        self.active
    }

    #[zbus(signal)]
    async fn lock(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn unlock(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

struct FakeUser;

#[zbus::interface(name = "org.freedesktop.login1.User")]
impl FakeUser {
    #[zbus(property)]
    fn display(&self) -> (String, OwnedObjectPath) {
        (
            "_31".to_owned(),
            OwnedObjectPath::try_from(SESSION).unwrap(),
        )
    }
}

async fn fake_logind(bus: &PrivateBus) -> zbus::Connection {
    zbus::connection::Builder::address(bus.address.as_str())
        .unwrap()
        .serve_at(MANAGER, FakeManager)
        .unwrap()
        .serve_at(SESSION, FakeSession { active: true })
        .unwrap()
        .serve_at(USER, FakeUser)
        .unwrap()
        .name("org.freedesktop.login1")
        .unwrap()
        .build()
        .await
        .unwrap()
}

struct Running {
    task: tokio::task::JoinHandle<Result<(), ha2luxd::service::ServiceError>>,
    stop: tokio::sync::oneshot::Sender<()>,
    control: MockControl,
    hardware: Arc<MockHardware>,
    _dir: TempDir,
}

fn start_daemon(bus: &PrivateBus, session_id: Option<&str>) -> Running {
    let dir = TempDir::new().unwrap();
    let hardware = MockHardware::new();
    let control = hardware.add("A", DisplayKind::External, 100, 50);
    let options = ServiceOptions {
        bus: BusKind::Address(bus.address.clone()),
        paths: Paths::in_dir(dir.path()),
        hardware: Arc::clone(&hardware) as _,
        clock: Arc::new(TokioClock::new(MinuteOfDay::parse("14:00").unwrap())),
        takeover_timeout: Duration::from_secs(5),
        honor_quit: true,
        task_config: Some(TaskConfig {
            min_interval: Duration::ZERO,
            ..TaskConfig::ddc()
        }),
        planning_tick: Duration::from_secs(60),
        ha: None,
        system: SystemOptions {
            logind_bus: Some(BusKind::Address(bus.address.clone())),
            session_id: session_id.map(str::to_owned),
            netlink: false,
            poll_dir: None,
        },
        idle: ha2luxd::service::IdleOptions::none(),
    };
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(run(options, async move {
        // Arrêt demandé par le test ; un canal fermé vaut aussi arrêt.
        let _ = stopped.await;
    }));
    Running {
        task,
        stop,
        control,
        hardware,
        _dir: dir,
    }
}

async fn until(what: &str, mut condition: impl FnMut() -> bool) {
    for _ in 0..1_500 {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("condition non atteinte : {what}");
}

async fn wait_daemon(connection: &zbus::Connection) {
    let dbus = zbus::fdo::DBusProxy::new(connection).await.unwrap();
    let name = zbus::names::BusName::try_from(BUS_NAME).unwrap();
    for _ in 0..1_000 {
        if dbus.name_has_owner(name.clone()).await.unwrap() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("démon absent");
}

async fn emit_sleep(login: &zbus::Connection, start: bool) {
    let iface = login
        .object_server()
        .interface::<_, FakeManager>(MANAGER)
        .await
        .unwrap();
    FakeManager::prepare_for_sleep(iface.signal_emitter(), start)
        .await
        .unwrap();
}

#[tokio::test]
async fn suspend_stops_hardware_writes_and_resume_restores_control() {
    let bus = bus_or_skip!();
    let login = fake_logind(&bus).await;
    let daemon = start_daemon(&bus, Some("31"));
    let client = zbus::connection::Builder::address(bus.address.as_str())
        .unwrap()
        .build()
        .await
        .unwrap();
    wait_daemon(&client).await;
    let proxy = DaemonProxy::new(&client).await.unwrap();
    until("énumération", || daemon.hardware.discoveries() >= 1).await;
    proxy.set_master(40).await.unwrap();
    until("écran à 40", || daemon.control.raw() == 40).await;

    // La surveillance s'abonne après le démarrage : on émet jusqu'à observer l'effet.
    let mut suspended = false;
    for _ in 0..200 {
        emit_sleep(&login, true).await;
        tokio::time::sleep(Duration::from_millis(15)).await;
        proxy.set_master(70).await.unwrap();
        tokio::time::sleep(Duration::from_millis(40)).await;
        if daemon.control.raw() == 40 {
            suspended = true;
            break;
        }
        proxy.set_master(40).await.unwrap();
        until("retour à 40", || daemon.control.raw() == 40).await;
    }
    assert!(
        suspended,
        "PrepareForSleep(true) : aucune écriture matérielle"
    );
    assert_eq!(
        proxy.master().await.unwrap(),
        70,
        "le maître décidé est mémorisé"
    );

    emit_sleep(&login, false).await;
    until("réveil : maître appliqué", || {
        // Les écritures sont de nouveau permises ; la ré-application arrive après le rescan débouncé.
        daemon.control.raw() == 70
    })
    .await;
    let _ = daemon.stop.send(());
    daemon.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn unlock_triggers_a_debounced_rescan_and_lock_does_not() {
    let bus = bus_or_skip!();
    let login = fake_logind(&bus).await;
    let daemon = start_daemon(&bus, Some("31"));
    let client = zbus::connection::Builder::address(bus.address.as_str())
        .unwrap()
        .build()
        .await
        .unwrap();
    wait_daemon(&client).await;
    until("énumération", || daemon.hardware.discoveries() >= 1).await;
    tokio::time::sleep(Duration::from_millis(300)).await; // laisse la surveillance s'abonner
    let calm = daemon.hardware.discoveries();
    let iface = login
        .object_server()
        .interface::<_, FakeSession>(SESSION)
        .await
        .unwrap();
    for _ in 0..5 {
        FakeSession::lock(iface.signal_emitter()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(daemon.hardware.discoveries(), calm, "verrouillage : rien");
    for _ in 0..40 {
        FakeSession::unlock(iface.signal_emitter()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        if daemon.hardware.discoveries() > calm {
            break;
        }
    }
    assert!(
        daemon.hardware.discoveries() > calm,
        "déverrouillage : rescan (après le délai de réveil)"
    );
    let _ = daemon.stop.send(());
    daemon.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn session_becoming_inactive_pauses_control() {
    let bus = bus_or_skip!();
    let login = fake_logind(&bus).await;
    let daemon = start_daemon(&bus, None); // sans XDG_SESSION_ID : session graphique de l'utilisateur
    let client = zbus::connection::Builder::address(bus.address.as_str())
        .unwrap()
        .build()
        .await
        .unwrap();
    wait_daemon(&client).await;
    let proxy = DaemonProxy::new(&client).await.unwrap();
    until("énumération", || daemon.hardware.discoveries() >= 1).await;
    proxy.set_master(35).await.unwrap();
    until("écran à 35", || daemon.control.raw() == 35).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let iface = login
        .object_server()
        .interface::<_, FakeSession>(SESSION)
        .await
        .unwrap();
    let mut paused = false;
    for _ in 0..100 {
        iface.get_mut().await.active = false;
        iface
            .get()
            .await
            .active_changed(iface.signal_emitter())
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        proxy.set_master(80).await.unwrap();
        tokio::time::sleep(Duration::from_millis(40)).await;
        if daemon.control.raw() == 35 {
            paused = true;
            break;
        }
        proxy.set_master(35).await.unwrap();
        until("retour à 35", || daemon.control.raw() == 35).await;
    }
    assert!(
        paused,
        "session inactive : le démon ne pilote plus les écrans"
    );
    iface.get_mut().await.active = true;
    iface
        .get()
        .await
        .active_changed(iface.signal_emitter())
        .await
        .unwrap();
    until("session active : maître appliqué", || {
        daemon.control.raw() == 80
    })
    .await;
    let _ = daemon.stop.send(());
    daemon.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn the_daemon_runs_without_logind() {
    let bus = bus_or_skip!();
    let daemon = start_daemon(&bus, Some("31")); // aucun logind sur ce bus
    let client = zbus::connection::Builder::address(bus.address.as_str())
        .unwrap()
        .build()
        .await
        .unwrap();
    wait_daemon(&client).await;
    let proxy = DaemonProxy::new(&client).await.unwrap();
    until("énumération", || daemon.hardware.discoveries() >= 1).await;
    proxy.set_master(55).await.unwrap();
    until("le démon fonctionne sans logind", || {
        daemon.control.raw() == 55
    })
    .await;
    let _ = daemon.stop.send(());
    daemon.task.await.unwrap().unwrap();
}
