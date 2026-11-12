//! Tâche d'écran : coalescence, cadence, retries, cooldown, timeouts (T-DDC-10).
//! Temps virtuel de tokio (`start_paused`) : aucun `sleep` réel.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use ha2lux_backends::mock::{MockBackend, MockControl};
use ha2lux_backends::task::{DisplayEvent, DisplayHandle, TaskConfig, spawn_display_task};
use ha2lux_core::{BackendError, DisplayId, DisplayKind};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio::time::{Instant, sleep};

const MS: fn(u64) -> Duration = Duration::from_millis;

fn display(
    name: &str,
    max: u32,
    cfg: TaskConfig,
) -> (DisplayHandle, MockControl, UnboundedReceiver<DisplayEvent>) {
    let (backend, control) = MockBackend::new(name, DisplayKind::External, max, 0);
    let (tx, rx) = unbounded_channel();
    (spawn_display_task(backend, cfg, tx), control, rx)
}

fn drain(rx: &mut UnboundedReceiver<DisplayEvent>) -> Vec<DisplayEvent> {
    std::iter::from_fn(|| rx.try_recv().ok()).collect()
}

fn transient() -> BackendError {
    BackendError::Transient("occupé".to_owned())
}

#[tokio::test(start_paused = true)]
async fn t_ddc_10_burst_is_coalesced_and_last_value_written() {
    let (handle, control, _rx) = display("A", 100, TaskConfig::ddc());
    let start = Instant::now();
    for value in 1..=100u32 {
        handle.set_target(value);
        sleep(MS(2)).await; // 100 valeurs en 200 ms
    }
    sleep(MS(500)).await;
    let writes = control.write_times();
    assert_eq!(
        writes.last().map(|w| w.1),
        Some(100),
        "la dernière valeur est toujours écrite"
    );
    assert!(
        writes.len() <= 6,
        "rafale coalescée : {} écritures",
        writes.len()
    );
    assert!(writes.len() >= 3);
    assert!(
        writes[0].0 - start <= MS(2),
        "la première écriture part tout de suite"
    );
    for pair in writes.windows(2) {
        assert!(
            pair[1].0 - pair[0].0 >= MS(50),
            "cadence minimale non respectée"
        );
    }
    assert!(
        writes.windows(2).all(|p| p[0].1 < p[1].1),
        "valeurs croissantes, aucune obsolète"
    );
    handle.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn identical_target_is_not_rewritten() {
    let (handle, control, _rx) = display("A", 100, TaskConfig::ddc());
    handle.set_target(5);
    sleep(MS(100)).await;
    handle.set_target(5);
    sleep(MS(100)).await;
    assert_eq!(control.writes(), [5]);
    handle.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn target_is_clamped_to_display_max() {
    let (handle, control, _rx) = display("A", 100, TaskConfig::ddc());
    handle.set_target(10_000);
    sleep(MS(10)).await;
    assert_eq!(control.writes(), [100]);
    handle.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn retries_with_backoff_then_succeeds() {
    let (handle, control, mut rx) = display("A", 100, TaskConfig::ddc());
    control.fail_next_writes(2, transient());
    let start = Instant::now();
    handle.set_target(42);
    sleep(Duration::from_secs(1)).await;
    assert_eq!(control.attempts(), 3);
    let writes = control.write_times();
    assert_eq!(writes.len(), 1);
    assert_eq!(
        writes[0].0 - start,
        MS(50 + 100),
        "backoff 50 ms puis 100 ms"
    );
    let stats = handle.stats();
    assert_eq!(
        (stats.writes_ok, stats.retries, stats.writes_failed),
        (1, 2, 0)
    );
    assert_eq!(
        drain(&mut rx),
        [DisplayEvent::Applied {
            display: DisplayId::new("A"),
            raw: 42
        }]
    );
    handle.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn retry_uses_the_latest_target() {
    let (handle, control, _rx) = display("A", 100, TaskConfig::ddc());
    control.fail_next_writes(1, transient());
    handle.set_target(10);
    sleep(MS(10)).await; // 1ʳᵉ tentative échouée, backoff en cours
    handle.set_target(20);
    sleep(Duration::from_secs(1)).await;
    assert_eq!(
        control.writes(),
        [20],
        "la valeur périmée n'est jamais réécrite"
    );
    handle.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn failures_freeze_writes_then_single_final_write_at_thaw() {
    let (handle, control, mut rx) = display("A", 100, TaskConfig::ddc());
    control.fail_always(Some(transient()));
    handle.set_target(10);
    sleep(Duration::from_secs(1)).await;
    assert_eq!(control.attempts(), 3, "3 tentatives puis gel");
    let events = drain(&mut rx);
    assert_eq!(events.len(), 2, "{events:?}");
    assert!(matches!(
        &events[0],
        DisplayEvent::WriteFailed { fatal: false, .. }
    ));
    assert!(
        matches!(&events[1], DisplayEvent::CoolingDown { duration, .. } if *duration == Duration::from_secs(5))
    );

    // Pendant le gel, la cible évolue mais rien n'est tenté.
    for value in [20, 30] {
        handle.set_target(value);
        sleep(Duration::from_secs(1)).await;
    }
    assert_eq!(control.attempts(), 3, "écritures gelées");

    control.fail_always(None);
    sleep(Duration::from_secs(3)).await;
    assert_eq!(control.attempts(), 4, "une seule écriture au dégel");
    assert_eq!(control.writes(), [30], "avec la dernière cible");
    assert_eq!(
        drain(&mut rx),
        [DisplayEvent::Applied {
            display: DisplayId::new("A"),
            raw: 30
        }]
    );
    handle.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn still_failing_at_thaw_tries_once_per_cooldown() {
    let (handle, control, mut rx) = display("A", 100, TaskConfig::ddc());
    control.fail_always(Some(transient()));
    handle.set_target(10);
    sleep(Duration::from_secs(6)).await;
    assert_eq!(control.attempts(), 4, "3 puis une seule au dégel");
    sleep(Duration::from_secs(5)).await;
    assert_eq!(control.attempts(), 5);
    let cooling = drain(&mut rx)
        .iter()
        .filter(|e| matches!(e, DisplayEvent::CoolingDown { .. }))
        .count();
    assert!(cooling >= 3);
    handle.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn non_transient_error_is_fatal_without_retry() {
    let (handle, control, mut rx) = display("A", 100, TaskConfig::ddc());
    control.fail_always(Some(BackendError::PermissionDenied("udev".to_owned())));
    handle.set_target(10);
    sleep(Duration::from_secs(1)).await;
    assert_eq!(control.attempts(), 1);
    let events = drain(&mut rx);
    assert!(matches!(
        &events[0],
        DisplayEvent::WriteFailed {
            fatal: true,
            error: BackendError::PermissionDenied(_),
            ..
        }
    ));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, DisplayEvent::CoolingDown { .. }))
    );
    handle.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn hung_backend_times_out_counts_abandoned_ops_then_fails_for_good() {
    let (handle, control, mut rx) = display("A", 100, TaskConfig::ddc());
    control.set_hang(true);
    handle.set_target(10);
    sleep(Duration::from_secs(10)).await;
    assert_eq!(control.attempts(), 3);
    let stats = handle.stats();
    assert_eq!(stats.abandoned_ops, 3, "opérations abandonnées comptées");
    assert_eq!(stats.writes_failed, 1);
    let events = drain(&mut rx);
    assert!(
        matches!(
            &events[0],
            DisplayEvent::WriteFailed {
                fatal: true,
                error: BackendError::Timeout,
                ..
            }
        ),
        "{events:?}"
    );
    handle.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn slow_but_within_timeout_succeeds() {
    let (handle, control, _rx) = display("A", 100, TaskConfig::ddc());
    control.set_latency(Duration::from_millis(1_500));
    handle.set_target(7);
    sleep(Duration::from_secs(3)).await;
    assert_eq!(control.writes(), [7]);
    assert_eq!(handle.stats().abandoned_ops, 0);
    handle.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn a_blocked_display_does_not_block_the_others() {
    let (slow, slow_control, _slow_rx) = display("A", 100, TaskConfig::ddc());
    let (fast, fast_control, _fast_rx) = display("B", 100, TaskConfig::ddc());
    slow_control.set_hang(true);
    slow.set_target(10);
    fast.set_target(60);
    sleep(MS(20)).await;
    assert_eq!(
        fast_control.writes(),
        [60],
        "B écrit pendant que A est bloqué"
    );
    assert!(slow_control.writes().is_empty());
    slow.shutdown().await;
    fast.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn read_and_probe_are_serialized_with_writes() {
    let (handle, _control, _rx) = display("A", 255, TaskConfig::ddc());
    handle.set_target(30);
    sleep(MS(10)).await;
    assert_eq!(handle.read().await.unwrap(), 30);
    assert_eq!(handle.probe().await.unwrap().max, 255);
    assert_eq!(handle.info().id, DisplayId::new("A"));
    handle.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn hung_read_times_out_and_is_counted() {
    let (handle, control, _rx) = display("A", 100, TaskConfig::ddc());
    control.set_hang(true);
    let start = Instant::now();
    assert_eq!(handle.read().await, Err(BackendError::Timeout));
    assert_eq!(
        start.elapsed(),
        Duration::from_secs(2),
        "timeout dur de 2 s"
    );
    assert_eq!(handle.stats().abandoned_ops, 1);
    // La tâche reste vivante et utilisable après l'abandon.
    control.set_hang(false);
    assert_eq!(handle.read().await.unwrap(), 0);
    handle.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn shutdown_flushes_the_final_value_respecting_cadence() {
    let (handle, control, _rx) = display("A", 100, TaskConfig::ddc());
    handle.set_target(10);
    sleep(MS(1)).await;
    handle.set_target(77); // en attente de la cadence de 50 ms
    handle.shutdown().await;
    assert_eq!(
        control.writes(),
        [10, 77],
        "l'écriture finale n'est pas perdue"
    );
}

#[tokio::test(start_paused = true)]
async fn backlight_profile_has_no_pacing() {
    let (handle, control, _rx) = display("A", 100, TaskConfig::backlight());
    for value in [10, 20, 30] {
        handle.set_target(value);
        sleep(MS(1)).await;
    }
    assert_eq!(control.writes(), [10, 20, 30]);
    handle.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn forced_target_rewrites_an_identical_value() {
    let (handle, control, mut rx) = display("A", 100, TaskConfig::ddc());
    handle.set_target(40);
    sleep(MS(100)).await;
    control.set_raw(100); // l'écran s'est remis à 100 % tout seul
    handle.set_target(40);
    sleep(MS(100)).await;
    assert_eq!(
        control.writes(),
        [40],
        "sans force, une valeur identique n'est pas réécrite"
    );
    handle.force_target(40);
    sleep(MS(100)).await;
    assert_eq!(control.writes(), [40, 40], "avec force, elle est réécrite");
    assert_eq!(control.raw(), 40);
    assert_eq!(
        drain(&mut rx).len(),
        2,
        "un événement Applied par écriture réelle"
    );
    // Une écriture forcée n'est faite qu'une fois ; la cible suivante redevient normale.
    handle.set_target(40);
    sleep(MS(100)).await;
    assert_eq!(control.writes().len(), 2);
    handle.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn forced_write_respects_the_pacing_and_coalescing() {
    let (handle, control, _rx) = display("A", 100, TaskConfig::ddc());
    handle.set_target(10);
    sleep(MS(1)).await;
    handle.force_target(10);
    handle.force_target(20);
    sleep(MS(500)).await;
    let writes = control.write_times();
    assert_eq!(writes.last().map(|w| w.1), Some(20));
    assert!(
        writes.windows(2).all(|p| p[1].0 - p[0].0 >= MS(50)),
        "cadence respectée"
    );
    handle.shutdown().await;
}
