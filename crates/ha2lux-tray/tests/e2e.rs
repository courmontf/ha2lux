//! De bout en bout : le vrai binaire `ha2lux-tray` contre le vrai démon (écrans factices),
//! avec un faux `StatusNotifierWatcher`, sur un **bus privé**.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, missing_docs)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use ha2lux_backends::task::TaskConfig;
use ha2lux_core::{DisplayKind, MinuteOfDay};
use ha2lux_ipc::{BUS_NAME, DaemonProxy};
use ha2luxd::clock::TokioClock;
use ha2luxd::hardware::MockHardware;
use ha2luxd::paths::Paths;
use ha2luxd::service::{BusKind, ServiceOptions, run};
use tempfile::TempDir;
use zbus::zvariant::{OwnedValue, Value};

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

    async fn connect(&self) -> zbus::Connection {
        zbus::connection::Builder::address(self.address.as_str())
            .unwrap()
            .build()
            .await
            .unwrap()
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Watcher {
    registered: Arc<Mutex<Vec<String>>>,
}

#[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
impl Watcher {
    fn register_status_notifier_item(&self, service: &str) {
        self.registered
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(service.to_owned());
    }
    fn register_status_notifier_host(&self, _service: &str) {}
    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        self.registered
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn protocol_version(&self) -> i32 {
        0
    }
}

async fn eventually<T, F, Fut>(what: &str, mut probe: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    for _ in 0..600 {
        if let Some(value) = probe().await {
            return value;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("délai dépassé : {what}");
}

/// Éléments du menu `dbusmenu` : (identifiant, libellé, activé).
fn layout_items(node: &Value<'_>, out: &mut Vec<(i32, String, bool)>) {
    let Value::Structure(structure) = node else {
        return;
    };
    let fields = structure.fields();
    let (Some(Value::I32(id)), Some(Value::Dict(props)), Some(Value::Array(children))) =
        (fields.first(), fields.get(1), fields.get(2))
    else {
        return;
    };
    let get = |key: &str| -> Option<Value<'_>> {
        props
            .iter()
            .find(|(k, _)| matches!(k, Value::Str(s) if s.as_str() == key))
            .map(|(_, v)| match v {
                Value::Value(inner) => (**inner).try_clone().unwrap(),
                other => other.try_clone().unwrap(),
            })
    };
    let label = match get("label") {
        Some(Value::Str(s)) => s.to_string(),
        _ => String::new(),
    };
    let enabled = !matches!(get("enabled"), Some(Value::Bool(false)));
    out.push((*id, label, enabled));
    for child in children.iter() {
        let child = match child {
            Value::Value(inner) => &**inner,
            other => other,
        };
        layout_items(child, out);
    }
}

async fn menu(connection: &zbus::Connection, service: &str) -> Vec<(i32, String, bool)> {
    let reply = connection
        .call_method(
            Some(service),
            "/MenuBar",
            Some("com.canonical.dbusmenu"),
            "GetLayout",
            &(0i32, -1i32, Vec::<String>::new()),
        )
        .await
        .unwrap();
    let body = reply.body();
    type Node = (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>);
    let (_revision, (id, props, children)): (u32, Node) = body.deserialize().unwrap();
    let mut items = vec![(
        id,
        props
            .get("label")
            .and_then(|v| String::try_from(v.try_clone().unwrap()).ok())
            .unwrap_or_default(),
        true,
    )];
    for child in &children {
        layout_items(child, &mut items);
    }
    items
}

async fn click(connection: &zbus::Connection, service: &str, id: i32) {
    connection
        .call_method(
            Some(service),
            "/MenuBar",
            Some("com.canonical.dbusmenu"),
            "Event",
            &(id, "clicked", Value::from(0i32), 0u32),
        )
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn tray_registers_shows_the_menu_and_drives_the_daemon() {
    let Some(bus) = PrivateBus::start() else {
        eprintln!("SKIPPED : dbus-daemon introuvable, test D-Bus non exécuté");
        return;
    };
    // Faux hôte d'icônes.
    let registered = Arc::new(Mutex::new(Vec::new()));
    let watcher_connection = bus.connect().await;
    watcher_connection
        .object_server()
        .at(
            "/StatusNotifierWatcher",
            Watcher {
                registered: Arc::clone(&registered),
            },
        )
        .await
        .unwrap();
    watcher_connection
        .request_name("org.kde.StatusNotifierWatcher")
        .await
        .unwrap();

    // Vrai démon, écrans factices.
    let dir = TempDir::new().unwrap();
    let paths = Paths::in_dir(dir.path());
    std::fs::write(
        &paths.config_file,
        "[presets]\nNuit = 20\nJour = 90\n[schedule]\nenabled = false\npoints = [{ time = \"08:00\", level = 80 }]\n",
    )
    .unwrap();
    let hardware = MockHardware::new();
    hardware.add("A", DisplayKind::External, 100, 50);
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
    let service = tokio::spawn(run(options, async move {
        let _ = stopped.await;
    }));
    let client = bus.connect().await;
    let dbus = zbus::fdo::DBusProxy::new(&client).await.unwrap();
    eventually("démon sur le bus", || async {
        dbus.name_has_owner(zbus::names::BusName::try_from(BUS_NAME).unwrap())
            .await
            .ok()
            .filter(|owned| *owned)
    })
    .await;
    let daemon = DaemonProxy::new(&client).await.unwrap();

    // Vrai binaire d'icône.
    let mut tray = Command::new(env!("CARGO_BIN_EXE_ha2lux-tray"))
        .env("DBUS_SESSION_BUS_ADDRESS", &bus.address)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let service_name = eventually("enregistrement de l'icône", || async {
        registered
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .first()
            .cloned()
    })
    .await;
    // ksni annonce `nom-de-bus` ou `nom-de-bus/chemin` selon la version.
    let destination = service_name.split('/').next().unwrap().to_owned();

    // Le menu reflète le démon : presets avec valeurs, écran, bascules.
    let items = eventually("menu rempli", || async {
        let items = menu(&client, &destination).await;
        items
            .iter()
            .any(|(_, label, _)| label == "Nuit — 20 %")
            .then_some(items)
    })
    .await;
    let labels: Vec<&str> = items.iter().map(|(_, l, _)| l.as_str()).collect();
    for expected in [
        "Jour — 90 %",
        "Re-scanner les écrans",
        "Réglages…",
        "Quitter",
    ] {
        assert!(
            labels.contains(&expected),
            "{expected} absent de {labels:?}"
        );
    }
    assert!(
        labels
            .iter()
            .any(|l| l.starts_with("Auto-luminosité (non configurée)"))
    );

    // Clic sur un preset : le démon applique le niveau.
    let night = items.iter().find(|(_, l, _)| l == "Nuit — 20 %").unwrap().0;
    click(&client, &destination, night).await;
    eventually("preset appliqué", || async {
        (daemon.master().await.ok()? == 20).then_some(())
    })
    .await;

    // Molette : trois crans vers le haut = un seul pas de 3 × 5 %.
    for _ in 0..3 {
        client
            .call_method(
                Some(destination.as_str()),
                "/StatusNotifierItem",
                Some("org.kde.StatusNotifierItem"),
                "Scroll",
                &(120i32, "vertical"),
            )
            .await
            .unwrap();
    }
    eventually("molette appliquée", || async {
        (daemon.master().await.ok()? == 35).then_some(())
    })
    .await;

    // Le menu suit le changement fait côté démon (CLI, raccourci…).
    daemon.set_master(70).await.unwrap();
    eventually("menu rafraîchi", || async {
        let items = menu(&client, &destination).await;
        items
            .iter()
            .any(|(_, l, _)| l == "Luminosité : 70 %")
            .then_some(())
    })
    .await;

    // « Quitter » arrête l'icône (pas le démon).
    let quit = items.iter().find(|(_, l, _)| l == "Quitter").unwrap().0;
    // L'icône peut se fermer avant de répondre à l'appel : l'erreur est attendue.
    let _ = client
        .call_method(
            Some(destination.as_str()),
            "/MenuBar",
            Some("com.canonical.dbusmenu"),
            "Event",
            &(quit, "clicked", Value::from(0i32), 0u32),
        )
        .await;
    let mut status = None;
    for _ in 0..600 {
        if let Some(done) = tray.try_wait().unwrap() {
            status = Some(done);
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let status = status.expect("l'icône ne s'est pas arrêtée");
    assert!(status.success(), "{status:?}");
    assert!(daemon.master().await.is_ok(), "le démon doit rester actif");

    let _ = stop.send(());
    service.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn tray_without_a_watcher_exits_with_code_3() {
    let Some(bus) = PrivateBus::start() else {
        eprintln!("SKIPPED : dbus-daemon introuvable, test D-Bus non exécuté");
        return;
    };
    let address = bus.address.clone();
    let output = tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_ha2lux-tray"))
            .env("DBUS_SESSION_BUS_ADDRESS", address)
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    assert_eq!(output.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("zone de notification"), "{stderr}");
}
