//! Service D-Bus de bout en bout sur un **bus privé** (jamais la session de l'utilisateur).
//!
//! Chaque test lance son propre `dbus-daemon`. S'il est introuvable, les tests sont
//! ignorés avec un message (ils ne passent pas pour autant en silence : voir `SKIPPED`).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use ha2lux_backends::mock::MockControl;
use ha2lux_backends::task::TaskConfig;
use ha2lux_core::{DisplayKind, MinuteOfDay};
use ha2lux_ipc::{BUS_NAME, DaemonProxy, ErrorKind, classify};
use ha2luxd::clock::TokioClock;
use ha2luxd::hardware::MockHardware;
use ha2luxd::paths::Paths;
use ha2luxd::service::{BusKind, ServiceError, ServiceOptions, run};
use tempfile::TempDir;
use tokio::task::JoinHandle;

/// Bus de session privé, détruit à la fin du test.
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
                "<!DOCTYPE busconfig PUBLIC \"-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN\"\n \"http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd\">\n<busconfig>\n  <type>session</type>\n  <listen>unix:dir={}</listen>\n  <auth>EXTERNAL</auth>\n  <policy context=\"default\">\n    <allow send_destination=\"*\" eavesdrop=\"true\"/>\n    <allow eavesdrop=\"true\"/>\n    <allow own=\"*\"/>\n  </policy>\n</busconfig>\n",
                dir.path().display()
            ),
        )
        .unwrap();
        let mut child = Command::new("dbus-daemon")
            .arg(format!("--config-file={}", config.display()))
            .arg("--nofork")
            .arg("--print-address=1")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let mut address = String::new();
        BufReader::new(child.stdout.take()?)
            .read_line(&mut address)
            .ok()?;
        let address = address.trim().to_owned();
        if address.is_empty() {
            return None;
        }
        Some(Self {
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
                eprintln!("SKIPPED : dbus-daemon introuvable, test D-Bus non exécuté");
                return;
            }
        }
    };
}

struct Instance {
    task: JoinHandle<Result<(), ServiceError>>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    paths: Paths,
    hardware: Arc<MockHardware>,
    control: MockControl,
    _dir: Option<TempDir>,
}

struct Setup {
    paths: Option<Paths>,
    config: Option<&'static str>,
    honor_quit: bool,
    takeover: Duration,
}

impl Default for Setup {
    fn default() -> Self {
        Self {
            paths: None,
            config: None,
            honor_quit: true,
            takeover: Duration::from_secs(5),
        }
    }
}

fn spawn_service(bus: &PrivateBus, setup: Setup) -> Instance {
    let (dir, paths) = match setup.paths {
        Some(paths) => (None, paths),
        None => {
            let dir = TempDir::new().unwrap();
            let paths = Paths::in_dir(dir.path());
            (Some(dir), paths)
        }
    };
    if let Some(text) = setup.config {
        std::fs::write(&paths.config_file, text).unwrap();
    }
    let hardware = MockHardware::new();
    let control = hardware.add("A", DisplayKind::External, 100, 50);
    let options = ServiceOptions {
        bus: BusKind::Address(bus.address.clone()),
        paths: paths.clone(),
        hardware: Arc::clone(&hardware) as _,
        clock: Arc::new(TokioClock::new(MinuteOfDay::parse("14:00").unwrap())),
        takeover_timeout: setup.takeover,
        honor_quit: setup.honor_quit,
        task_config: Some(TaskConfig {
            min_interval: Duration::ZERO,
            ..TaskConfig::ddc()
        }),
        planning_tick: Duration::from_secs(60),
        ha: None,
        system: ha2luxd::service::SystemOptions::none(),
        idle: ha2luxd::service::IdleOptions::none(),
    };
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(run(options, async move {
        // Arrêt demandé par le test ; un canal fermé vaut aussi arrêt.
        let _ = stopped.await;
    }));
    Instance {
        task,
        stop: Some(stop),
        paths,
        hardware,
        control,
        _dir: dir,
    }
}

impl Instance {
    async fn stop(mut self) -> Result<(), ServiceError> {
        if let Some(stop) = self.stop.take() {
            // Le service a pu déjà s'arrêter (Quit) : sans importance.
            let _ = stop.send(());
        }
        self.task.await.unwrap()
    }
}

impl Instance {
    /// Arrête le service puis lit le fichier d'état avant de détruire le répertoire temporaire.
    async fn stop_and_read_state(self) -> String {
        let Instance {
            task,
            stop,
            paths,
            _dir,
            ..
        } = self;
        if let Some(stop) = stop {
            // Le service a pu déjà s'arrêter : sans importance.
            let _ = stop.send(());
        }
        task.await.unwrap().unwrap();
        let text = std::fs::read_to_string(&paths.state_file).unwrap();
        drop(_dir);
        text
    }
}

async fn client(bus: &PrivateBus) -> zbus::Connection {
    zbus::connection::Builder::address(bus.address.as_str())
        .unwrap()
        .build()
        .await
        .unwrap()
}

async fn wait_owner(connection: &zbus::Connection, present: bool) {
    let dbus = zbus::fdo::DBusProxy::new(connection).await.unwrap();
    let name = zbus::names::BusName::try_from(BUS_NAME).unwrap();
    for _ in 0..1_000 {
        if dbus.name_has_owner(name.clone()).await.unwrap() == present {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "{BUS_NAME} n'est pas {} après 10 s",
        if present { "apparu" } else { "parti" }
    );
}

async fn until(what: &str, mut condition: impl FnMut() -> bool) {
    for _ in 0..1_000 {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("condition non atteinte : {what}");
}

fn method_error(error: &zbus::Error) -> String {
    match error {
        zbus::Error::MethodError(name, detail, _) => {
            // Comme le client : le détail est une clé + arguments à traduire.
            let detail = ha2lux_i18n::wire::localize(&detail.clone().unwrap_or_default());
            format!("{} {detail}", name.as_str())
        }
        other => format!("{other}"),
    }
}

#[tokio::test]
async fn methods_drive_the_hardware_and_report_state() {
    let bus = bus_or_skip!();
    let service = spawn_service(&bus, Setup::default());
    let connection = client(&bus).await;
    wait_owner(&connection, true).await;
    let proxy = DaemonProxy::new(&connection).await.unwrap();

    until("écran listé", || service.hardware.discoveries() >= 1).await;
    proxy.set_master(40).await.unwrap();
    until("écran à 40", || service.control.raw() == 40).await;

    let list = proxy.list().await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(
        (list[0].id.as_str(), list[0].state.as_str(), list[0].level),
        ("A", "controllable", 40)
    );
    let status = proxy.status().await.unwrap();
    assert_eq!(
        i32::try_from(status["master"].try_clone().unwrap()).unwrap(),
        40
    );
    assert_eq!(
        String::try_from(status["mode"].try_clone().unwrap()).unwrap(),
        "static"
    );
    assert!(status.contains_key("version") && status.contains_key("writes_ok"));
    assert_eq!(proxy.master().await.unwrap(), 40);
    assert_eq!(proxy.mode().await.unwrap(), "static");

    proxy.apply_preset("night").await.unwrap();
    until("preset Night", || service.control.raw() == 30).await;
    proxy.step_master(5).await.unwrap();
    until("pas +5", || service.control.raw() == 35).await;
    proxy.set_range("A", 0, 50).await.unwrap();
    proxy.set_mode("schedule", true).await.unwrap();
    proxy.set_mode("schedule", false).await.unwrap();
    proxy.rescan().await.unwrap();
    proxy.reload().await.unwrap();
    proxy.reset_curve().await.unwrap();
    proxy.undo_learn().await.unwrap();
    service.stop().await.unwrap();
}

#[tokio::test]
async fn t_sec_01_errors_have_stable_names_and_come_from_the_daemon() {
    let bus = bus_or_skip!();
    let service = spawn_service(&bus, Setup::default());
    let connection = client(&bus).await;
    wait_owner(&connection, true).await;
    let proxy = DaemonProxy::new(&connection).await.unwrap();
    until("écran listé", || service.hardware.discoveries() >= 1).await;
    proxy.set_master(40).await.unwrap();

    let out_of_range = proxy.set_master(101).await.unwrap_err();
    assert!(
        method_error(&out_of_range).starts_with("org.ha2lux.Error.OutOfRange"),
        "{}",
        method_error(&out_of_range)
    );
    assert_eq!(classify(&out_of_range), Some(ErrorKind::Usage));
    let huge = proxy.set_master(u32::MAX).await.unwrap_err();
    assert_eq!(classify(&huge), Some(ErrorKind::Usage));
    assert_eq!(
        classify(&proxy.step_master(0).await.unwrap_err()),
        Some(ErrorKind::Usage)
    );
    assert_eq!(
        classify(&proxy.set_range("A", 0, 150).await.unwrap_err()),
        Some(ErrorKind::Usage)
    );
    assert_eq!(
        classify(&proxy.set_display("", 5).await.unwrap_err()),
        Some(ErrorKind::Usage)
    );

    let unknown = proxy.set_display("inconnu", 5).await.unwrap_err();
    assert!(method_error(&unknown).starts_with("org.ha2lux.Error.UnknownDisplay"));
    assert_eq!(classify(&unknown), Some(ErrorKind::Failed));
    let preset = proxy.apply_preset("Zzz").await.unwrap_err();
    let text = method_error(&preset);
    assert!(
        text.starts_with("org.ha2lux.Error.UnknownPreset") && text.contains("Night=30"),
        "{text}"
    );
    let auto = proxy.set_mode("auto", true).await.unwrap_err();
    assert!(method_error(&auto).starts_with("org.ha2lux.Error.NotConfigured"));
    let feature = proxy.set_mode("bogus", true).await.unwrap_err();
    assert_eq!(classify(&feature), Some(ErrorKind::Usage));

    assert_eq!(
        service.control.raw(),
        40,
        "aucune de ces requêtes n'a touché le matériel"
    );
    assert_eq!(proxy.master().await.unwrap(), 40);
    service.stop().await.unwrap();
}

#[tokio::test]
async fn daemon_absent_is_not_a_ha2lux_error() {
    let bus = bus_or_skip!();
    let connection = client(&bus).await;
    let proxy = DaemonProxy::new(&connection).await.unwrap();
    let error = proxy.set_master(10).await.unwrap_err();
    assert_eq!(
        classify(&error),
        None,
        "pas de démon : le client doit répondre « démon absent » (code 2)"
    );
}

#[tokio::test]
async fn signals_and_property_changes_are_emitted() {
    let bus = bus_or_skip!();
    let service = spawn_service(&bus, Setup::default());
    let connection = client(&bus).await;
    wait_owner(&connection, true).await;
    let proxy = DaemonProxy::new(&connection).await.unwrap();
    until("écran listé", || service.hardware.discoveries() >= 1).await;
    let mut levels = proxy.receive_level_changed().await.unwrap();
    let mut master_changes = proxy.receive_master_changed().await;
    proxy.set_master(63).await.unwrap();
    let signal = tokio::time::timeout(Duration::from_secs(5), levels.next())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(signal.args().unwrap().master, 63);
    let change = tokio::time::timeout(Duration::from_secs(5), master_changes.next())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(change.get().await.unwrap(), 63);

    service.stop().await.unwrap();
}

#[tokio::test]
async fn mode_changed_signal_follows_the_base_source() {
    let bus = bus_or_skip!();
    let config = "[schedule]\nenabled = false\npoints = [{ time = \"08:00\", level = 80 }, { time = \"20:00\", level = 20 }]\n";
    let service = spawn_service(
        &bus,
        Setup {
            config: Some(config),
            ..Setup::default()
        },
    );
    let connection = client(&bus).await;
    wait_owner(&connection, true).await;
    let proxy = DaemonProxy::new(&connection).await.unwrap();
    until("écran listé", || service.hardware.discoveries() >= 1).await;
    let mut modes = proxy.receive_mode_switched().await.unwrap();
    proxy.set_mode("schedule", true).await.unwrap();
    let signal = tokio::time::timeout(Duration::from_secs(5), modes.next())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(signal.args().unwrap().mode, "planning");
    until("planning de 14:00 appliqué", || {
        service.control.raw() == 50
    })
    .await;
    assert_eq!(proxy.mode().await.unwrap(), "planning");
    service.stop().await.unwrap();
}

#[tokio::test]
async fn concurrent_calls_from_several_clients_are_serialized() {
    let bus = bus_or_skip!();
    let service = spawn_service(&bus, Setup::default());
    let (c1, c2) = (client(&bus).await, client(&bus).await);
    wait_owner(&c1, true).await;
    let (p1, p2) = (
        DaemonProxy::new(&c1).await.unwrap(),
        DaemonProxy::new(&c2).await.unwrap(),
    );
    until("écran listé", || service.hardware.discoveries() >= 1).await;
    let (a, b) = tokio::join!(p1.set_master(25), p2.set_master(75));
    a.unwrap();
    b.unwrap();
    let master = p1.master().await.unwrap();
    assert!(master == 25 || master == 75);
    let expected = u32::try_from(master).unwrap();
    until("écran cohérent avec le maître", || {
        service.control.raw() == expected
    })
    .await;
    service.stop().await.unwrap();
}

#[tokio::test]
async fn quit_over_dbus_stops_the_service_and_releases_the_name() {
    let bus = bus_or_skip!();
    let service = spawn_service(&bus, Setup::default());
    let connection = client(&bus).await;
    wait_owner(&connection, true).await;
    let proxy = DaemonProxy::new(&connection).await.unwrap();
    until("écran listé", || service.hardware.discoveries() >= 1).await;
    proxy.set_master(58).await.unwrap();
    proxy.quit().await.unwrap();
    wait_owner(&connection, false).await;
    let paths = service.paths.clone();
    service.task.await.unwrap().unwrap();
    assert!(
        std::fs::read_to_string(&paths.state_file)
            .unwrap()
            .contains("master = 58"),
        "état écrit avant de libérer le nom"
    );
}

#[tokio::test]
async fn a_new_instance_replaces_the_old_one_cleanly() {
    let bus = bus_or_skip!();
    let first = spawn_service(&bus, Setup::default());
    let connection = client(&bus).await;
    wait_owner(&connection, true).await;
    let proxy = DaemonProxy::new(&connection).await.unwrap();
    until("écran listé", || first.hardware.discoveries() >= 1).await;
    proxy.set_master(33).await.unwrap();
    until("écran à 33", || first.control.raw() == 33).await;

    // Même fichiers d'état : le successeur doit retrouver ce que l'ancien a écrit en partant.
    let shared = first.paths.clone();
    let second = spawn_service(
        &bus,
        Setup {
            paths: Some(shared),
            ..Setup::default()
        },
    );
    let first_task = first.task;
    // L'ancienne instance s'arrête d'elle-même (demande de sortie) ; la nouvelle prend la place.
    tokio::time::timeout(Duration::from_secs(10), first_task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    until("nouvelle instance servante", || {
        second.hardware.discoveries() >= 1
    })
    .await;
    wait_owner(&connection, true).await;
    until("maître repris du fichier d'état", || {
        std::fs::read_to_string(&second.paths.state_file).is_ok_and(|t| t.contains("master = 33"))
    })
    .await;
    let proxy = DaemonProxy::new(&connection).await.unwrap();
    let mut master = proxy.master().await.unwrap();
    for _ in 0..200 {
        if master == 33 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        master = proxy.master().await.unwrap();
    }
    assert_eq!(
        master, 33,
        "le successeur répond et a repris l'état de l'ancien"
    );
    second.stop().await.unwrap();
}

#[tokio::test]
async fn a_new_instance_refuses_to_start_if_the_old_one_does_not_release_the_name() {
    let bus = bus_or_skip!();
    let stubborn = spawn_service(
        &bus,
        Setup {
            honor_quit: false,
            ..Setup::default()
        },
    );
    let connection = client(&bus).await;
    wait_owner(&connection, true).await;
    until("écran listé", || stubborn.hardware.discoveries() >= 1).await;

    let second = spawn_service(
        &bus,
        Setup {
            takeover: Duration::from_millis(400),
            ..Setup::default()
        },
    );
    let error = second.task.await.unwrap().unwrap_err();
    assert!(matches!(error, ServiceError::Takeover(_)), "{error}");
    assert!(error.to_string().contains("jamais deux démons"), "{error}");
    assert_eq!(
        second.hardware.discoveries(),
        0,
        "le second démon n'a jamais touché au matériel"
    );
    // L'ancienne instance sert toujours.
    let proxy = DaemonProxy::new(&connection).await.unwrap();
    proxy.set_master(21).await.unwrap();
    until("l'ancienne instance répond", || {
        stubborn.control.raw() == 21
    })
    .await;
    stubborn.stop().await.unwrap();
}

#[tokio::test]
async fn shutdown_signal_stops_cleanly_and_flushes_state() {
    let bus = bus_or_skip!();
    let service = spawn_service(&bus, Setup::default());
    let connection = client(&bus).await;
    wait_owner(&connection, true).await;
    let proxy = DaemonProxy::new(&connection).await.unwrap();
    until("écran listé", || service.hardware.discoveries() >= 1).await;
    proxy.set_master(47).await.unwrap();
    let state = service.stop_and_read_state().await;
    wait_owner(&connection, false).await;
    assert!(state.contains("master = 47"), "{state}");
}

/// Lance le **vrai binaire** `ha2luxd` (matériel réel, pas de faux) sur un bus privé, avec des
/// répertoires XDG temporaires. Ignoré par défaut : sur un poste avec backlight ou écrans liés en
/// DDC dans sysfs, le démon lirait leur luminosité (sondes en lecture). À lancer à la main :
/// `cargo test -p ha2luxd --test dbus -- --ignored --nocapture`.
#[tokio::test]
#[ignore = "lance le vrai binaire avec le matériel réel (sondes en lecture) ; à lancer à la main"]
async fn real_binary_smoke_on_a_private_bus() {
    let bus = bus_or_skip!();
    let dir = TempDir::new().unwrap();
    let log = dir.path().join("daemon.log");
    let spawn = || {
        Command::new(env!("CARGO_BIN_EXE_ha2luxd"))
            .env("DBUS_SESSION_BUS_ADDRESS", &bus.address)
            .env("XDG_CONFIG_HOME", dir.path().join("cfg"))
            .env("XDG_STATE_HOME", dir.path().join("st"))
            .env("HOME", dir.path())
            .env("RUST_LOG", "info")
            .stderr(std::fs::File::create(&log).unwrap())
            .spawn()
            .unwrap()
    };
    let connection = client(&bus).await;
    let mut child = spawn();
    wait_owner(&connection, true).await;
    let proxy = DaemonProxy::new(&connection).await.unwrap();
    println!("--- List : {:?}", proxy.list().await.unwrap());
    let status = proxy.status().await.unwrap();
    let mut keys: Vec<_> = status.keys().cloned().collect();
    keys.sort();
    println!("--- Status ({} clés) : {keys:?}", keys.len());
    for (name, call) in [
        ("SetMaster 101", proxy.set_master(101).await),
        ("StepMaster 0", proxy.step_master(0).await),
        ("ApplyPreset Zzz", proxy.apply_preset("Zzz").await),
        ("SetMode auto on", proxy.set_mode("auto", true).await),
        ("SetMode bogus", proxy.set_mode("bogus", true).await),
    ] {
        let error = call.unwrap_err();
        println!(
            "--- {name} → {} ({:?})",
            method_error(&error),
            classify(&error)
        );
        assert!(
            classify(&error).is_some(),
            "erreur de ha2lux attendue : {error}"
        );
    }
    println!(
        "--- Master={} Mode={}",
        proxy.master().await.unwrap(),
        proxy.mode().await.unwrap()
    );
    let config = std::fs::read_to_string(dir.path().join("cfg/ha2lux/config.toml")).unwrap();
    assert!(
        config.contains("[general]") && config.contains("Night"),
        "configuration par défaut créée"
    );

    // Arrêt par SIGTERM (comme `systemctl --user stop`).
    assert!(
        Command::new("kill")
            .arg("-TERM")
            .arg(child.id().to_string())
            .status()
            .unwrap()
            .success()
    );
    let status = child.wait().unwrap();
    assert!(status.success(), "arrêt propre sur SIGTERM : {status}");
    wait_owner(&connection, false).await;

    // Deuxième vie : arrêt par Quit.
    let mut child = spawn();
    wait_owner(&connection, true).await;
    DaemonProxy::new(&connection)
        .await
        .unwrap()
        .quit()
        .await
        .unwrap();
    assert!(child.wait().unwrap().success(), "arrêt propre sur Quit");
    wait_owner(&connection, false).await;
    println!("--- journal :\n{}", std::fs::read_to_string(&log).unwrap());
}
