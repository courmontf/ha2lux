//! Full dimming wiring in the service (simulated Mutter fallback source
//! on a private bus): settings → source → blockers → state machine → displays.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, missing_docs)]

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use ha2lux_backends::task::TaskConfig;
use ha2lux_core::{DisplayKind, MinuteOfDay};
use ha2lux_ipc::{BUS_NAME, DaemonProxy};
use ha2luxd::clock::TokioClock;
use ha2luxd::hardware::MockHardware;
use ha2luxd::paths::Paths;
use ha2luxd::service::{BusKind, IdleOptions, ServiceOptions, SystemOptions, run};
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
        // The private bus is no longer needed; an error here has no consequence.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct FakeMutter(Arc<AtomicU64>);

#[zbus::interface(name = "org.gnome.Mutter.IdleMonitor")]
impl FakeMutter {
    fn get_idletime(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

struct FakePower(Arc<std::sync::atomic::AtomicBool>);

#[zbus::interface(name = "org.freedesktop.PowerManagement.Inhibit")]
impl FakePower {
    fn has_inhibit(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

async fn until(what: &str, mut condition: impl FnMut() -> bool) {
    for _ in 0..2_000 {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("condition non atteinte : {what}");
}

#[tokio::test]
async fn the_service_dims_on_idle_respects_blockers_and_restores_on_activity() {
    let Some(bus) = PrivateBus::start() else {
        eprintln!("SKIPPED : dbus-daemon introuvable");
        return;
    };
    let idle_ms = Arc::new(AtomicU64::new(1_000));
    let inhibited = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let _mutter = zbus::connection::Builder::address(bus.address.as_str())
        .unwrap()
        .serve_at(
            "/org/gnome/Mutter/IdleMonitor/Core",
            FakeMutter(Arc::clone(&idle_ms)),
        )
        .unwrap()
        .name("org.gnome.Mutter.IdleMonitor")
        .unwrap()
        .build()
        .await
        .unwrap();
    let _power = zbus::connection::Builder::address(bus.address.as_str())
        .unwrap()
        .serve_at(
            "/org/freedesktop/PowerManagement/Inhibit",
            FakePower(Arc::clone(&inhibited)),
        )
        .unwrap()
        .name("org.freedesktop.PowerManagement.Inhibit")
        .unwrap()
        .build()
        .await
        .unwrap();

    let dir = TempDir::new().unwrap();
    let paths = Paths::in_dir(dir.path());
    std::fs::write(
        &paths.config_file,
        "[idle]\nenabled = true\nlevel = 5\nminutes = 5\n",
    )
    .unwrap();
    let hardware = MockHardware::new();
    let control = hardware.add("A", DisplayKind::External, 100, 50);
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
        system: SystemOptions::none(),
        idle: IdleOptions {
            wayland: false,
            mutter: true,
            desktop_inhibitors: true,
            ..IdleOptions::none()
        },
    };
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let service = tokio::spawn(run(options, async move {
        // Shutdown requested by the test; a closed channel also means shutdown.
        let _ = stopped.await;
    }));

    let client = zbus::connection::Builder::address(bus.address.as_str())
        .unwrap()
        .build()
        .await
        .unwrap();
    let dbus = zbus::fdo::DBusProxy::new(&client).await.unwrap();
    let name = zbus::names::BusName::try_from(BUS_NAME).unwrap();
    for _ in 0..1_000 {
        if dbus.name_has_owner(name.clone()).await.unwrap() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let proxy = DaemonProxy::new(&client).await.unwrap();
    until("énumération", || hardware.discoveries() >= 1).await;
    proxy.set_master(60).await.unwrap();
    until("écran à 60", || control.raw() == 60).await;

    // A video is playing (desktop inhibitor): idle for 6 min, but no dimming.
    inhibited.store(true, Ordering::SeqCst);
    idle_ms.store(360_000, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(4_500)).await;
    assert_eq!(control.raw(), 60, "inhibiteur actif : pas d'atténuation");
    // The video ends: the display dims at the next probe (every 2 s).
    inhibited.store(false, Ordering::SeqCst);
    until("atténuation", || control.raw() == 5).await;
    assert_eq!(
        proxy.master().await.unwrap(),
        60,
        "le maître n'est pas modifié"
    );
    // L'utilisateur revient.
    idle_ms.store(200, Ordering::SeqCst);
    until("restauration", || control.raw() == 60).await;

    let _ = stop.send(());
    service.await.unwrap().unwrap();
}
