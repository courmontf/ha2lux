//! End to end: the real `ha2lux` binary against the real daemon (fake displays),
//! on a **private bus** (never the user's session).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, missing_docs)]

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use ha2lux_backends::mock::MockControl;
use ha2lux_backends::task::TaskConfig;
use ha2lux_cli::cli::{Parsed, parse};
use ha2lux_cli::client::{Call, CallError, Daemon, ZbusDaemon};
use ha2lux_cli::run::execute;
use ha2lux_core::{DisplayKind, MinuteOfDay};
use ha2lux_ipc::{BUS_NAME, OBJECT_PATH};
use ha2luxd::clock::TokioClock;
use ha2luxd::hardware::MockHardware;
use ha2luxd::paths::Paths;
use ha2luxd::service::{BusKind, ServiceOptions, run};
use tempfile::TempDir;

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
        // The private bus is no longer needed; an error here has no consequence.
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

struct Service {
    task: tokio::task::JoinHandle<Result<(), ha2luxd::service::ServiceError>>,
    stop: tokio::sync::oneshot::Sender<()>,
    control: MockControl,
    _dir: TempDir,
}

fn start_service(bus: &PrivateBus, config: Option<&str>) -> Service {
    let dir = TempDir::new().unwrap();
    let paths = Paths::in_dir(dir.path());
    if let Some(text) = config {
        std::fs::write(&paths.config_file, text).unwrap();
    }
    let hardware = MockHardware::new();
    let control = hardware.add("A", DisplayKind::External, 100, 50);
    hardware.add("B", DisplayKind::External, 100, 70);
    let options = ServiceOptions {
        bus: BusKind::Address(bus.address.clone()),
        paths,
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
        system: ha2luxd::service::SystemOptions::none(),
        idle: ha2luxd::service::IdleOptions::none(),
    };
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(run(options, async move {
        // Shutdown requested by the test; a closed channel also means shutdown.
        let _ = stopped.await;
    }));
    Service {
        task,
        stop,
        control,
        _dir: dir,
    }
}

impl Service {
    async fn stop(self) {
        // The service may have already stopped: irrelevant.
        let _ = self.stop.send(());
        self.task.await.unwrap().unwrap();
    }
}

async fn wait_for_name(bus: &PrivateBus) {
    let connection = zbus::connection::Builder::address(bus.address.as_str())
        .unwrap()
        .build()
        .await
        .unwrap();
    let dbus = zbus::fdo::DBusProxy::new(&connection).await.unwrap();
    let name = zbus::names::BusName::try_from(BUS_NAME).unwrap();
    for _ in 0..1_000 {
        if dbus.name_has_owner(name.clone()).await.unwrap() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("démon introuvable sur le bus");
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

/// Runs the real `ha2lux` binary (outside the async loop so as not to block it).
async fn ha2lux(bus_address: &str, args: &[&str]) -> Run {
    let address = bus_address.to_owned();
    let args: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
    tokio::task::spawn_blocking(move || {
        let output = Command::new(env!("CARGO_BIN_EXE_ha2lux"))
            .args(&args)
            .env("DBUS_SESSION_BUS_ADDRESS", address)
            .output()
            .unwrap();
        Run {
            code: output.status.code().unwrap(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    })
    .await
    .unwrap()
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

#[tokio::test]
async fn the_real_binary_drives_the_real_daemon() {
    let bus = bus_or_skip!();
    let service = start_service(&bus, None);
    wait_for_name(&bus).await;
    let a = &bus.address;
    until("énumération", || service.control.raw() == 50).await;
    // The first call may precede the end of enumeration: we wait for the list.
    let mut listed = ha2lux(a, &["list"]).await;
    for _ in 0..100 {
        if listed.stdout.contains("Mock A") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        listed = ha2lux(a, &["list"]).await;
    }
    assert_eq!(listed.code, 0, "{}", listed.stderr);
    assert!(
        listed
            .stdout
            .contains(" 1  Mock A  [A]  externe  contrôlable")
            && listed.stdout.contains(" 2  Mock B"),
        "{}",
        listed.stdout
    );

    let set = ha2lux(a, &["set", "40"]).await;
    assert_eq!(
        (set.code, set.stdout.as_str(), set.stderr.as_str()),
        (0, "", "")
    );
    until("écran A à 40", || service.control.raw() == 40).await;
    assert!(ha2lux(a, &["get"]).await.stdout.contains("maître : 40 %"));
    assert_eq!(
        ha2lux(a, &["SET", "35"]).await.code,
        0,
        "sous-commande insensible à la casse"
    );
    until("écran A à 35", || service.control.raw() == 35).await;
    assert_eq!(ha2lux(a, &["up"]).await.code, 0);
    until("pas par défaut (5)", || service.control.raw() == 40).await;
    assert_eq!(ha2lux(a, &["down", "10"]).await.code, 0);
    until("pas de 10", || service.control.raw() == 30).await;
    assert_eq!(ha2lux(a, &["preset", "NIGHT"]).await.code, 0);
    until("preset Night", || service.control.raw() == 30).await;
    assert_eq!(
        ha2lux(a, &["set", "80", "--display", "mock b"]).await.code,
        0
    );
    assert!(
        ha2lux(a, &["get", "--display", "2"])
            .await
            .stdout
            .contains("Mock B (n°2)")
    );

    let json = ha2lux(a, &["--json", "list"]).await;
    let parsed: serde_json::Value = serde_json::from_str(&json.stdout).unwrap();
    assert_eq!(parsed.as_array().unwrap().len(), 2);
    let auto = ha2lux(a, &["auto", "status"]).await;
    assert!(
        auto.stdout.contains("désactivée") && auto.stdout.contains("non configurée"),
        "{}",
        auto.stdout
    );

    let idle = ha2lux(a, &["idle-dim", "on"]).await;
    assert!(
        idle.stdout.contains("niveau 5 %") && idle.stdout.contains("5 min"),
        "{}",
        idle.stdout
    );
    let schedule = ha2lux(a, &["schedule", "on"]).await;
    assert_eq!(schedule.code, 0);
    assert!(
        schedule.stderr.contains("aucun point"),
        "{}",
        schedule.stderr
    );
    assert_eq!(ha2lux(a, &["rescan"]).await.code, 0);
    assert_eq!(ha2lux(a, &["reload"]).await.code, 0);
    service.stop().await;
}

#[tokio::test]
async fn errors_map_to_exit_codes_through_the_real_binary() {
    let bus = bus_or_skip!();
    let service = start_service(&bus, None);
    wait_for_name(&bus).await;
    let a = &bus.address;
    until("énumération", || service.control.raw() == 50).await;
    for _ in 0..100 {
        if ha2lux(a, &["list"]).await.stdout.contains("Mock A") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // Usage (1): rejected by the CLI before any call.
    for bad in [
        &["set", "101"][..],
        &["set"],
        &["up", "0"],
        &["schedule", "maybe"],
        &["get", "--list"],
        &["preset", "-x"],
        &["list", "--display", "1"],
    ] {
        let run = ha2lux(a, bad).await;
        assert_eq!(run.code, 1, "{bad:?}\n{}", run.stderr);
        assert!(run.stdout.is_empty());
    }
    // Command failure (3): preset, unknown/ambiguous display, unconfigured feature.
    let preset = ha2lux(a, &["preset", "Zzz"]).await;
    assert_eq!(preset.code, 3);
    assert!(
        preset.stderr.contains("Night=30, Day=80, Presentation=100"),
        "{}",
        preset.stderr
    );
    let unknown = ha2lux(a, &["set", "5", "--display", "9"]).await;
    assert_eq!(unknown.code, 3);
    assert!(
        unknown.stderr.contains("Mock A"),
        "la liste est rappelée : {}",
        unknown.stderr
    );
    let ambiguous = ha2lux(a, &["get", "--display", "mock"]).await;
    assert_eq!(ambiguous.code, 3);
    assert!(
        ambiguous.stderr.contains("n°1, n°2"),
        "{}",
        ambiguous.stderr
    );
    assert_eq!(ha2lux(a, &["auto", "on"]).await.code, 3);
    let json = ha2lux(a, &["--json", "preset", "Zzz"]).await;
    assert_eq!(json.code, 3);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&json.stdout).unwrap()["ok"],
        false
    );
    assert_eq!(
        service.control.raw(),
        50,
        "aucune de ces erreurs n'a touché le matériel"
    );
    service.stop().await;
}

#[tokio::test]
async fn t_cli_15_absent_daemon_gives_exit_2() {
    let bus = bus_or_skip!();
    let run = ha2lux(&bus.address, &["set", "40"]).await;
    assert_eq!(run.code, 2, "{}", run.stderr);
    assert!(
        run.stderr.contains("absent") && run.stderr.contains("systemctl --user start ha2luxd"),
        "{}",
        run.stderr
    );
    assert_eq!(ha2lux(&bus.address, &["--json", "list"]).await.code, 2);
    // No session bus at all: same diagnosis.
    let run = ha2lux("unix:path=/nonexistent/ha2lux-bus", &["list"]).await;
    assert_eq!(run.code, 2, "{}", run.stderr);
}

#[tokio::test]
async fn a_daemon_error_of_class_usage_is_exit_1_when_the_cli_check_is_bypassed() {
    let bus = bus_or_skip!();
    let service = start_service(&bus, None);
    wait_for_name(&bus).await;
    let daemon = ZbusDaemon::connect_address(&bus.address).await.unwrap();
    // Another client (script, tray) can send 101: the daemon rejects it itself.
    let error = daemon.call(Call::SetMaster(101)).await.unwrap_err();
    assert!(matches!(error, CallError::Usage(_)), "{error:?}");
    assert_eq!(error.exit_code(), 1);
    let unknown = daemon
        .call(Call::SetDisplay {
            id: "nope".into(),
            level: 5,
        })
        .await
        .unwrap_err();
    assert_eq!(unknown.exit_code(), 3);
    service.stop().await;
}

/// Daemon that accepts calls but never answers.
struct Silent;

#[zbus::interface(name = "org.ha2lux.Daemon1")]
impl Silent {
    async fn set_master(&self, _level: u32) {
        std::future::pending::<()>().await;
    }
}

#[tokio::test]
async fn t_cli_15_a_daemon_without_reply_gives_exit_4() {
    let bus = bus_or_skip!();
    let _server = zbus::connection::Builder::address(bus.address.as_str())
        .unwrap()
        .serve_at(OBJECT_PATH, Silent)
        .unwrap()
        .name(BUS_NAME)
        .unwrap()
        .build()
        .await
        .unwrap();
    let daemon = ZbusDaemon::connect_address(&bus.address).await.unwrap();
    let Ok(Parsed::Run(cli)) = parse(["set".to_owned(), "40".to_owned()]) else {
        panic!()
    };
    let outcome = execute(&cli, &daemon, Duration::from_millis(300)).await;
    assert_eq!(outcome.code, 4, "{}", outcome.stderr);
    assert!(
        outcome.stderr.contains("ne répond pas"),
        "{}",
        outcome.stderr
    );
}
