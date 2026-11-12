//! Backend backlight : faux sysfs et faux logind, aucune écriture réelle.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use ha2lux_backends::backlight::{
    BacklightBackend, BacklightType, BrightnessSetter, classify_dbus_error, preferred, scan,
};
use ha2lux_backends::task::{DisplayEvent, TaskConfig, spawn_display_task};
use ha2lux_core::{BackendError, DisplayBackend, DisplayKind};
use tempfile::TempDir;

fn make(
    root: &Path,
    name: &str,
    kind: &str,
    max: &str,
    brightness: &str,
    actual: Option<&str>,
) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("type"), format!("{kind}\n")).unwrap();
    std::fs::write(dir.join("max_brightness"), format!("{max}\n")).unwrap();
    std::fs::write(dir.join("brightness"), format!("{brightness}\n")).unwrap();
    if let Some(actual) = actual {
        std::fs::write(dir.join("actual_brightness"), format!("{actual}\n")).unwrap();
    }
    dir
}

/// Faux logind : enregistre les appels et simule l'effet sur sysfs.
#[derive(Clone, Default)]
struct FakeSetter {
    calls: Arc<Mutex<Vec<(String, u32)>>>,
    root: Option<PathBuf>,
    error: Option<BackendError>,
}

impl FakeSetter {
    fn calls(&self) -> Vec<(String, u32)> {
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl BrightnessSetter for FakeSetter {
    async fn set(&mut self, name: &str, value: u32) -> Result<(), BackendError> {
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((name.to_owned(), value));
        if let Some(root) = &self.root {
            std::fs::write(root.join(name).join("brightness"), format!("{value}\n")).unwrap();
        }
        Ok(())
    }
}

#[tokio::test]
async fn scan_missing_directory_is_empty() {
    assert!(
        scan(Path::new("/nonexistent/ha2lux/backlight"))
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn scan_reads_devices_and_skips_broken_ones() {
    let root = TempDir::new().unwrap();
    make(
        root.path(),
        "intel_backlight",
        "raw",
        "96000",
        "48000",
        None,
    );
    make(root.path(), "acpi_video0", "firmware", "100", "50", None);
    make(root.path(), "zero", "raw", "0", "0", None);
    make(root.path(), "garbage", "raw", "abc", "0", None);
    std::fs::create_dir_all(root.path().join("no_files")).unwrap();
    let found = scan(root.path()).await;
    let names: Vec<_> = found.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(names, ["acpi_video0", "intel_backlight"]);
    assert_eq!(found[0].kind, BacklightType::Firmware);
    assert_eq!(found[1].max, 96000);
}

#[tokio::test]
async fn firmware_is_preferred_over_platform_over_raw() {
    let root = TempDir::new().unwrap();
    make(root.path(), "a_raw", "raw", "10", "1", None);
    make(root.path(), "b_platform", "platform", "10", "1", None);
    make(root.path(), "c_firmware", "firmware", "10", "1", None);
    let found = scan(root.path()).await;
    assert_eq!(preferred(&found).unwrap().name, "c_firmware");
    let only_raw: Vec<_> = found
        .iter()
        .filter(|b| b.kind == BacklightType::Raw)
        .cloned()
        .collect();
    assert_eq!(preferred(&only_raw).unwrap().name, "a_raw");
    assert!(preferred(&[]).is_none());
}

#[tokio::test]
async fn type_unknown_is_other() {
    let root = TempDir::new().unwrap();
    make(root.path(), "x", "weird", "10", "1", None);
    assert_eq!(scan(root.path()).await[0].kind, BacklightType::Other);
}

#[tokio::test]
async fn read_prefers_actual_brightness() {
    let root = TempDir::new().unwrap();
    make(root.path(), "with_actual", "raw", "100", "50", Some("40"));
    make(root.path(), "without", "raw", "100", "50", None);
    let found = scan(root.path()).await;
    let by = |n: &str| found.iter().find(|b| b.name == n).unwrap().clone();
    let mut a = BacklightBackend::new(&by("with_actual"), FakeSetter::default());
    let mut b = BacklightBackend::new(&by("without"), FakeSetter::default());
    assert_eq!(a.read().await.unwrap(), 40);
    assert_eq!(b.read().await.unwrap(), 50);
}

#[tokio::test]
async fn info_and_probe() {
    let root = TempDir::new().unwrap();
    let dir = make(root.path(), "panel", "raw", "100", "50", None);
    let device = scan(root.path()).await.remove(0);
    let mut backend = BacklightBackend::new(&device, FakeSetter::default());
    assert_eq!(backend.info().id.as_str(), "backlight:panel");
    assert_eq!(backend.info().kind, DisplayKind::Internal);
    assert_eq!(backend.probe().await.unwrap().max, 100);
    std::fs::write(dir.join("max_brightness"), "0\n").unwrap();
    assert!(matches!(
        backend.probe().await,
        Err(BackendError::Invalid(_))
    ));
    std::fs::remove_file(dir.join("max_brightness")).unwrap();
    assert!(matches!(
        backend.probe().await,
        Err(BackendError::Disconnected(_))
    ));
}

#[tokio::test]
async fn write_goes_through_setter_and_never_above_max() {
    let root = TempDir::new().unwrap();
    make(root.path(), "panel", "raw", "100", "50", None);
    let device = scan(root.path()).await.remove(0);
    let setter = FakeSetter::default();
    let mut backend = BacklightBackend::new(&device, setter.clone());
    backend.write(30).await.unwrap();
    assert_eq!(setter.calls(), [("panel".to_owned(), 30)]);
    assert!(matches!(
        backend.write(101).await,
        Err(BackendError::Invalid(_))
    ));
    assert_eq!(
        setter.calls().len(),
        1,
        "valeur hors maximum jamais envoyée à logind"
    );
}

#[tokio::test]
async fn setter_errors_are_propagated() {
    let root = TempDir::new().unwrap();
    make(root.path(), "panel", "raw", "100", "50", None);
    let device = scan(root.path()).await.remove(0);
    let setter = FakeSetter {
        error: Some(BackendError::PermissionDenied("session inactive".into())),
        ..FakeSetter::default()
    };
    let mut backend = BacklightBackend::new(&device, setter);
    assert!(matches!(
        backend.write(10).await,
        Err(BackendError::PermissionDenied(_))
    ));
}

#[test]
fn dbus_errors_are_classified() {
    let kind = |name: &str| classify_dbus_error(name)("x".to_owned());
    assert!(matches!(
        kind("org.freedesktop.DBus.Error.AccessDenied"),
        BackendError::PermissionDenied(_)
    ));
    assert!(matches!(
        kind("org.freedesktop.PolicyKit1.Error.NotAuthorized"),
        BackendError::PermissionDenied(_)
    ));
    assert!(matches!(
        kind("org.freedesktop.DBus.Error.InvalidArgs"),
        BackendError::Invalid(_)
    ));
    assert!(matches!(
        kind("org.freedesktop.DBus.Error.NoReply"),
        BackendError::Transient(_)
    ));
    assert!(matches!(
        kind("quelque.chose.Inconnu"),
        BackendError::Transient(_)
    ));
}

/// Bout en bout : tâche d'écran + backend backlight + faux sysfs/logind.
#[tokio::test]
async fn display_task_drives_backlight_end_to_end() {
    let root = TempDir::new().unwrap();
    make(root.path(), "panel", "raw", "1000", "1000", None);
    let device = scan(root.path()).await.remove(0);
    let setter = FakeSetter {
        root: Some(root.path().to_owned()),
        ..FakeSetter::default()
    };
    let backend = BacklightBackend::new(&device, setter.clone());
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let handle = spawn_display_task(backend, TaskConfig::backlight(), tx);

    handle.set_target(250);
    let event = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(event, DisplayEvent::Applied { raw: 250, .. }));
    assert_eq!(handle.read().await.unwrap(), 250, "relecture du sysfs");
    assert_eq!(setter.calls(), [("panel".to_owned(), 250)]);
    handle.shutdown().await;
}
