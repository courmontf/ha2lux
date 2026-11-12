//! Backend DDC/CI contre un faux moniteur : aucune E/S réelle, aucun `sleep`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;
use std::time::Duration;

use ha2lux_backends::ddc::{DdcBackend, DdcTimings, remaining_gap};
use ha2lux_backends::i2c::{BusLocks, classify_io_error};
use ha2lux_backends::task::{DisplayEvent, TaskConfig, spawn_display_task};
use ha2lux_core::ddc::{VCP_BRIGHTNESS, encode_get_vcp, encode_set_vcp};
use ha2lux_core::{BackendError, DisplayBackend, DisplayId, DisplayInfo, DisplayKind};
use support::monitor::{EACCES, ENODEV, ENXIO, FakeOpener, MonitorControl};

fn info(name: &str) -> DisplayInfo {
    DisplayInfo {
        id: DisplayId::new(name),
        name: name.to_owned(),
        kind: DisplayKind::External,
        max: 0,
    }
}

fn backend_for(opener: &Arc<FakeOpener>, locks: &BusLocks, bus: u32) -> DdcBackend {
    DdcBackend::new(
        info("A"),
        bus,
        Arc::clone(opener) as _,
        locks,
        DdcTimings::none(),
    )
}

fn setup(current: u16, max: u16) -> (DdcBackend, MonitorControl, Arc<FakeOpener>) {
    let monitor = MonitorControl::new(current, max);
    let opener = FakeOpener::with_monitor(5, monitor.clone());
    let backend = backend_for(&opener, &BusLocks::new(), 5);
    (backend, monitor, opener)
}

#[tokio::test]
async fn probe_reads_max_and_current_with_real_frames() {
    let (mut backend, monitor, _) = setup(40, 100);
    let probed = backend.probe().await.unwrap();
    assert_eq!(probed.max, 100);
    assert_eq!(backend.info().max, 100);
    assert_eq!(backend.read().await.unwrap(), 40);
    assert_eq!(monitor.frames()[0], encode_get_vcp(VCP_BRIGHTNESS).to_vec());
}

#[tokio::test]
async fn write_sends_a_set_vcp_frame() {
    let (mut backend, monitor, _) = setup(0, 255);
    backend.probe().await.unwrap();
    backend.write(200).await.unwrap();
    assert_eq!(monitor.current(), 200);
    assert_eq!(
        monitor.frames().last().unwrap(),
        &encode_set_vcp(VCP_BRIGHTNESS, 200).to_vec()
    );
    assert_eq!(monitor.set_values(), [200]);
}

#[tokio::test]
async fn write_is_refused_before_probe_and_above_max() {
    let (mut backend, monitor, _) = setup(0, 100);
    assert!(
        matches!(backend.write(10).await, Err(BackendError::Invalid(_))),
        "maximum inconnu"
    );
    backend.probe().await.unwrap();
    let frames_before = monitor.frames().len();
    assert!(matches!(
        backend.write(101).await,
        Err(BackendError::Invalid(_))
    ));
    assert_eq!(
        monitor.frames().len(),
        frames_before,
        "aucune trame envoyée hors bornes"
    );
}

#[tokio::test]
async fn null_message_after_wake_is_retried() {
    let (mut backend, monitor, _) = setup(30, 100);
    monitor.with(|s| s.null_messages = 2);
    assert_eq!(
        backend.probe().await.unwrap().max,
        100,
        "réponse nulle = échec transitoire, pas « non DDC »"
    );
    assert_eq!(monitor.get_requests(), 3);
}

#[tokio::test]
async fn persistent_null_messages_end_as_transient_failure() {
    let (mut backend, monitor, _) = setup(30, 100);
    monitor.with(|s| s.null_messages = 100);
    let error = backend.probe().await.unwrap_err();
    assert!(error.is_transient(), "{error}");
    assert_eq!(monitor.get_requests(), 3, "3 tentatives puis abandon");
}

#[tokio::test]
async fn corrupted_checksum_is_retried() {
    let (mut backend, monitor, _) = setup(30, 100);
    monitor.with(|s| s.corrupt_next = 1);
    assert_eq!(backend.probe().await.unwrap().max, 100);
    assert_eq!(monitor.get_requests(), 2);
}

#[tokio::test]
async fn unsupported_vcp_fails_at_once() {
    let (mut backend, monitor, _) = setup(30, 100);
    monitor.with(|s| s.unsupported = true);
    assert!(matches!(
        backend.probe().await,
        Err(BackendError::Invalid(_))
    ));
    assert_eq!(monitor.get_requests(), 1, "inutile de réessayer");
}

#[tokio::test]
async fn zero_max_is_not_controllable() {
    let (mut backend, _, _) = setup(0, 0);
    let error = backend.probe().await.unwrap_err();
    assert!(
        matches!(error, BackendError::Invalid(ref m) if m.contains("nul")),
        "{error}"
    );
}

#[tokio::test]
async fn nack_is_transient_and_retried() {
    let (mut backend, monitor, _) = setup(30, 100);
    monitor.with(|s| s.io_errors.extend([ENXIO, ENXIO]));
    assert_eq!(
        backend.probe().await.unwrap().max,
        100,
        "l'écran dort puis répond"
    );
    assert_eq!(monitor.get_requests(), 3);
}

#[tokio::test]
async fn permission_error_is_final() {
    let (mut backend, monitor, _) = setup(30, 100);
    monitor.with(|s| s.io_errors.push_back(EACCES));
    assert!(matches!(
        backend.probe().await,
        Err(BackendError::PermissionDenied(_))
    ));
    assert_eq!(monitor.frames().len(), 1);
}

#[tokio::test]
async fn vanished_node_closes_the_device_and_reopens_next_time() {
    let (mut backend, monitor, opener) = setup(30, 100);
    backend.probe().await.unwrap();
    assert_eq!(opener.opens(), 1);
    monitor.with(|s| s.io_errors.push_back(ENODEV));
    assert!(matches!(
        backend.read().await,
        Err(BackendError::Disconnected(_))
    ));
    assert_eq!(backend.read().await.unwrap(), 30);
    assert_eq!(opener.opens(), 2, "fd refermé puis rouvert");
}

#[tokio::test]
async fn open_failures_are_classified() {
    let monitor = MonitorControl::new(1, 100);
    let opener = FakeOpener::with_monitor(5, monitor);
    opener.fail_open(5, EACCES);
    let mut backend = backend_for(&opener, &BusLocks::new(), 5);
    assert!(matches!(
        backend.probe().await,
        Err(BackendError::PermissionDenied(_))
    ));
    let mut missing = backend_for(&opener, &BusLocks::new(), 9);
    assert!(matches!(
        missing.probe().await,
        Err(BackendError::Disconnected(_))
    ));
}

#[test]
fn io_error_classification() {
    let class = |errno| classify_io_error(&std::io::Error::from_raw_os_error(errno));
    assert!(matches!(class(13), BackendError::PermissionDenied(_)));
    assert!(matches!(class(1), BackendError::PermissionDenied(_)));
    assert!(matches!(class(2), BackendError::Disconnected(_)));
    assert!(matches!(class(19), BackendError::Disconnected(_)));
    for transient in [5, 6, 16, 110, 121] {
        assert!(class(transient).is_transient(), "errno {transient}");
    }
}

#[test]
fn pacing_arithmetic() {
    let gap = Duration::from_millis(50);
    assert_eq!(
        remaining_gap(Duration::from_millis(20), gap),
        Duration::from_millis(30)
    );
    assert_eq!(
        remaining_gap(Duration::from_millis(50), gap),
        Duration::ZERO
    );
    assert_eq!(remaining_gap(Duration::from_secs(5), gap), Duration::ZERO);
    assert_eq!(
        remaining_gap(Duration::ZERO, Duration::ZERO),
        Duration::ZERO
    );
}

#[test]
fn bus_locks_are_per_bus() {
    let locks = BusLocks::new();
    assert!(Arc::ptr_eq(&locks.get(5), &locks.get(5)));
    assert!(!Arc::ptr_eq(&locks.get(5), &locks.get(6)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_displays_on_one_bus_never_overlap() {
    let monitor = MonitorControl::new(10, 100);
    let opener = FakeOpener::with_monitor(5, monitor.clone());
    let locks = BusLocks::new();
    let mut a = backend_for(&opener, &locks, 5);
    let mut b = backend_for(&opener, &locks, 5);
    a.probe().await.unwrap();
    b.probe().await.unwrap();
    let work = |mut backend: DdcBackend| async move {
        for value in 0..20 {
            backend.write(value).await.unwrap();
            backend.read().await.unwrap();
        }
    };
    tokio::join!(tokio::spawn(work(a)), tokio::spawn(work(b)))
        .0
        .unwrap();
    assert_eq!(
        monitor.max_concurrency(),
        1,
        "un seul accès à la fois sur un même bus"
    );
}

/// Un thread bloqué dans le noyau : l'appelant abandonne l'attente (comme lors d'un
/// timeout), mais le verrou du bus et le garde « opération en cours » ne sont
/// libérés qu'à la fin réelle du thread.
#[tokio::test]
async fn abandoned_operation_keeps_bus_locked_and_blocks_reuse_until_it_ends() {
    let monitor = MonitorControl::new(10, 100);
    let opener = FakeOpener::with_monitor(5, monitor.clone());
    let locks = BusLocks::new();
    let mut hung = backend_for(&opener, &locks, 5);
    let mut neighbour = backend_for(&opener, &locks, 5);
    hung.probe().await.unwrap();

    monitor.hang(true);
    {
        let read = hung.read();
        tokio::pin!(read);
        tokio::select! {
            _ = &mut read => panic!("la lecture ne peut pas aboutir : l'écran est coincé"),
            () = monitor.entered.notified() => {}
        }
        // `read` est abandonnée ici, thread toujours bloqué dans le « noyau ».
    }

    // Même écran : refus immédiat, aucun second thread empilé.
    let again = hung.read().await.unwrap_err();
    assert!(
        matches!(&again, BackendError::Transient(m) if m.contains("précédente")),
        "{again}"
    );
    // Autre écran sur le même bus : le verrou est toujours tenu.
    {
        let blocked = neighbour.read();
        tokio::pin!(blocked);
        tokio::select! {
            _ = &mut blocked => panic!("le verrou du bus aurait dû rester tenu"),
            () = tokio::task::yield_now() => {}
        }
    }

    monitor.hang(false);
    let mut recovered = false;
    for _ in 0..1_000_000 {
        match hung.read().await {
            Ok(value) => {
                assert_eq!(value, 10);
                recovered = true;
                break;
            }
            Err(BackendError::Transient(m)) if m.contains("précédente") => {
                tokio::task::yield_now().await
            }
            Err(other) => panic!("erreur inattendue : {other}"),
        }
    }
    assert!(
        recovered,
        "l'écran redevient utilisable après la fin du thread"
    );
    assert_eq!(
        neighbour.read().await.unwrap(),
        10,
        "le verrou du bus est libéré"
    );
}

/// Tâche d'écran + backend DDC : une rafale ne laisse partir que la dernière valeur.
#[tokio::test]
async fn display_task_over_ddc_writes_the_final_value() {
    let (mut backend, monitor, _) = setup(0, 100);
    backend.probe().await.unwrap();
    let config = TaskConfig {
        min_interval: Duration::ZERO,
        ..TaskConfig::ddc()
    };
    let (events, mut received) = tokio::sync::mpsc::unbounded_channel();
    let handle = spawn_display_task(backend, config, events);
    for value in 1..=50 {
        handle.set_target(value);
    }
    let event = tokio::time::timeout(Duration::from_secs(5), received.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(event, DisplayEvent::Applied { raw: 50, .. }),
        "{event:?}"
    );
    assert_eq!(
        monitor.set_values(),
        [50],
        "rafale coalescée en une écriture"
    );
    handle.shutdown().await;
}
