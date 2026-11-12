//! Pilote d'inactivité : fausses sources et fausses sondes de blocage, vrai acteur,
//! temps virtuel (T-IDL-01..05, 13 dans leur forme « pilote »).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use ha2lux_backends::mock::MockControl;
use ha2lux_core::DisplayKind;
use ha2luxd::api::Request;
use ha2luxd::hardware::MockHardware;
use ha2luxd::idle::{
    BlockReport, Blockers, IdleEvent, IdleSource, IdleTime, drive_events, drive_polled, supervise,
};
use support::Rig;
use tokio::sync::mpsc;

struct FakeSource(mpsc::UnboundedReceiver<IdleEvent>);

impl IdleSource for FakeSource {
    async fn next_event(&mut self) -> Option<IdleEvent> {
        self.0.recv().await
    }
}

#[derive(Clone, Default)]
struct FakeBlockers {
    reasons: Arc<Mutex<Vec<String>>>,
    probes: Arc<AtomicU32>,
}

impl FakeBlockers {
    fn set(&self, reasons: &[&str]) {
        *self.reasons.lock().unwrap_or_else(PoisonError::into_inner) =
            reasons.iter().map(|s| (*s).to_owned()).collect();
    }

    fn probes(&self) -> u32 {
        self.probes.load(Ordering::SeqCst)
    }
}

impl Blockers for FakeBlockers {
    async fn probe(&mut self) -> BlockReport {
        self.probes.fetch_add(1, Ordering::SeqCst);
        BlockReport {
            reasons: self
                .reasons
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
        }
    }
}

const IDLE_CONFIG: &str = "[idle]\nenabled = true\nlevel = 5\nminutes = 5\n";
const THRESHOLD_MS: u64 = 300_000;

async fn rig() -> (Rig, MockControl) {
    let hardware = MockHardware::new();
    let control = hardware.add("A", DisplayKind::External, 100, 50);
    let rig = Rig::start_with(hardware, Some(IDLE_CONFIG), None)
        .await
        .unwrap();
    rig.ok(Request::SetMaster(60)).await;
    (rig, control)
}

fn start_driver(
    rig: &Rig,
    blockers: &FakeBlockers,
) -> (
    mpsc::UnboundedSender<IdleEvent>,
    tokio::task::JoinHandle<()>,
) {
    let (tx, rx) = mpsc::unbounded_channel();
    let handle = rig.handle.clone();
    let mut blockers = blockers.clone();
    let task = tokio::spawn(async move {
        let mut source = FakeSource(rx);
        drive_events(
            &handle,
            &mut source,
            &mut blockers,
            THRESHOLD_MS,
            Duration::from_secs(2),
        )
        .await;
    });
    (tx, task)
}

#[tokio::test(start_paused = true)]
async fn idle_then_activity_dims_and_restores() {
    let (rig, control) = rig().await;
    let blockers = FakeBlockers::default();
    let (events, _task) = start_driver(&rig, &blockers);
    rig.settle().await;
    assert_eq!(
        blockers.probes(),
        0,
        "aucune sonde tant que l'utilisateur travaille"
    );
    events.send(IdleEvent::Idled).unwrap();
    rig.settle().await;
    assert_eq!(control.raw(), 5, "atténué au niveau d'inactivité");
    assert!(rig.handle.snapshot().dimmed);
    assert_eq!(
        rig.handle.snapshot().master,
        Some(60),
        "le maître n'est pas modifié"
    );
    events.send(IdleEvent::Resumed).unwrap();
    rig.settle().await;
    assert_eq!(control.raw(), 60, "restauré à la reprise d'activité");
    assert!(!rig.handle.snapshot().dimmed);
}

#[tokio::test(start_paused = true)]
async fn t_idl_04_05_blockers_prevent_dimming_and_it_dims_as_soon_as_they_end() {
    let (rig, control) = rig().await;
    let blockers = FakeBlockers::default();
    blockers.set(&["inhibiteur d'inactivité : lecteur vidéo"]);
    let (events, _task) = start_driver(&rig, &blockers);
    events.send(IdleEvent::Idled).unwrap();
    rig.advance(Duration::from_secs(7)).await;
    assert_eq!(control.raw(), 60, "vidéo en cours : pas d'atténuation");
    assert!(
        blockers.probes() >= 3,
        "réessayé à chaque sonde (toutes les 2 s) : {}",
        blockers.probes()
    );
    blockers.set(&["microphone en cours d'utilisation"]);
    rig.advance(Duration::from_secs(4)).await;
    assert_eq!(control.raw(), 60, "micro ouvert : pas d'atténuation");
    blockers.set(&[]);
    rig.advance(Duration::from_secs(3)).await;
    assert_eq!(control.raw(), 5, "atténue dès la levée des blocages");
}

#[tokio::test(start_paused = true)]
async fn t_idl_13_a_blocker_appearing_after_dimming_restores_and_it_dims_again_when_released() {
    let (rig, control) = rig().await;
    let blockers = FakeBlockers::default();
    let (events, _task) = start_driver(&rig, &blockers);
    events.send(IdleEvent::Idled).unwrap();
    rig.advance(Duration::from_secs(1)).await;
    assert_eq!(control.raw(), 5);
    blockers.set(&["microphone en cours d'utilisation"]); // un appel démarre
    rig.advance(Duration::from_secs(3)).await;
    assert_eq!(
        control.raw(),
        60,
        "restauré : l'utilisateur est en visioconférence"
    );
    blockers.set(&[]);
    rig.advance(Duration::from_secs(3)).await;
    assert_eq!(
        control.raw(),
        5,
        "toujours inactif : l'écran s'atténue de nouveau à la fin de l'appel"
    );
}

#[tokio::test(start_paused = true)]
async fn the_idle_duration_reported_grows_past_the_threshold() {
    // Contrôle indirect : à Idled + 10 s, la machine d'états reçoit idle ≥ seuil + 10 s sans erreur.
    let (rig, control) = rig().await;
    let blockers = FakeBlockers::default();
    let (events, _task) = start_driver(&rig, &blockers);
    events.send(IdleEvent::Idled).unwrap();
    rig.advance(Duration::from_secs(10)).await;
    assert_eq!(control.raw(), 5);
    assert!(rig.handle.snapshot().dimmed);
}

#[tokio::test(start_paused = true)]
async fn a_cli_or_dbus_command_while_dimmed_counts_as_activity() {
    let (rig, control) = rig().await;
    let blockers = FakeBlockers::default();
    let (events, _task) = start_driver(&rig, &blockers);
    events.send(IdleEvent::Idled).unwrap();
    rig.advance(Duration::from_secs(1)).await;
    assert_eq!(control.raw(), 5);
    rig.ok(Request::SetMaster(40)).await;
    assert!(!rig.handle.snapshot().dimmed);
    assert_eq!(control.raw(), 40);
}

#[tokio::test(start_paused = true)]
async fn the_driver_stops_when_the_source_ends() {
    let (rig, _) = rig().await;
    let blockers = FakeBlockers::default();
    let (events, task) = start_driver(&rig, &blockers);
    drop(events);
    rig.advance(Duration::from_millis(10)).await;
    assert!(task.is_finished());
}

struct FakeTime {
    now_ms: Arc<AtomicU64>,
}

impl IdleTime for FakeTime {
    async fn idle_ms(&mut self) -> Option<u64> {
        Some(self.now_ms.load(Ordering::SeqCst))
    }
}

#[tokio::test(start_paused = true)]
async fn polled_source_probes_blockers_only_when_idle_and_restores_on_activity() {
    let (rig, control) = rig().await;
    let blockers = FakeBlockers::default();
    let idle_ms = Arc::new(AtomicU64::new(1_000));
    let handle = rig.handle.clone();
    let (mut b, i) = (blockers.clone(), Arc::clone(&idle_ms));
    let _task = tokio::spawn(async move {
        let mut time = FakeTime { now_ms: i };
        drive_polled(
            &handle,
            &mut time,
            &mut b,
            THRESHOLD_MS,
            Duration::from_secs(2),
        )
        .await;
    });
    rig.advance(Duration::from_secs(20)).await;
    assert_eq!(
        blockers.probes(),
        0,
        "actif : aucune sonde, aucun événement"
    );
    assert_eq!(control.raw(), 60);
    idle_ms.store(THRESHOLD_MS + 5_000, Ordering::SeqCst);
    rig.advance(Duration::from_secs(3)).await;
    assert_eq!(control.raw(), 5);
    assert!(blockers.probes() >= 1);
    idle_ms.store(500, Ordering::SeqCst); // un mouvement de souris
    rig.advance(Duration::from_secs(3)).await;
    assert_eq!(control.raw(), 60);
}

struct DropFlag(Arc<AtomicU32>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test(start_paused = true)]
async fn supervise_runs_the_source_only_while_enabled_and_restarts_when_the_delay_changes() {
    let hardware = MockHardware::new();
    hardware.add("A", DisplayKind::External, 100, 50);
    let rig = Rig::start_with(
        hardware,
        Some("[idle]\nenabled = false\nminutes = 5\n"),
        None,
    )
    .await
    .unwrap();
    let starts: Arc<Mutex<Vec<u64>>> = Arc::default();
    let stopped = Arc::new(AtomicU32::new(0));
    let (log, drops) = (Arc::clone(&starts), Arc::clone(&stopped));
    let handle = rig.handle.clone();
    let _supervisor = tokio::spawn(supervise(handle, move |threshold_ms| {
        log.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(threshold_ms);
        let guard = DropFlag(Arc::clone(&drops));
        async move {
            let _guard = guard;
            std::future::pending::<()>().await;
        }
    }));
    rig.advance(Duration::from_secs(5)).await;
    assert!(
        starts.lock().unwrap().is_empty(),
        "désactivée : aucune source ouverte"
    );

    rig.ok(Request::SetMode {
        feature: "idle-dim".into(),
        on: true,
    })
    .await;
    rig.advance(Duration::from_secs(1)).await;
    assert_eq!(
        *starts.lock().unwrap(),
        [300_000],
        "activée : source ouverte avec le délai de 5 min"
    );

    std::fs::write(
        &rig.paths.config_file,
        "[idle]\nenabled = true\nminutes = 10\n",
    )
    .unwrap();
    rig.ok(Request::Reload).await;
    rig.advance(Duration::from_secs(1)).await;
    assert_eq!(
        *starts.lock().unwrap(),
        [300_000, 600_000],
        "délai changé : source relancée"
    );
    assert_eq!(
        stopped.load(Ordering::SeqCst),
        1,
        "l'ancienne source a été fermée"
    );

    rig.ok(Request::SetMode {
        feature: "idle-dim".into(),
        on: false,
    })
    .await;
    rig.advance(Duration::from_secs(1)).await;
    assert_eq!(
        stopped.load(Ordering::SeqCst),
        2,
        "désactivée : source fermée"
    );
    assert_eq!(starts.lock().unwrap().len(), 2);
}

#[tokio::test(start_paused = true)]
async fn supervise_retries_a_source_that_ended_after_30_seconds() {
    let hardware = MockHardware::new();
    hardware.add("A", DisplayKind::External, 100, 50);
    let rig = Rig::start_with(hardware, Some(IDLE_CONFIG), None)
        .await
        .unwrap();
    let count = Arc::new(AtomicU32::new(0));
    let counter = Arc::clone(&count);
    let _supervisor = tokio::spawn(supervise(rig.handle.clone(), move |_| {
        counter.fetch_add(1, Ordering::SeqCst);
        async {} // la source se termine aussitôt (compositeur parti)
    }));
    rig.advance(Duration::from_secs(1)).await;
    assert_eq!(count.load(Ordering::SeqCst), 1);
    rig.advance(Duration::from_secs(28)).await; // t = 29 s
    assert_eq!(count.load(Ordering::SeqCst), 1, "pas de boucle serrée");
    rig.advance(Duration::from_secs(2)).await;
    assert_eq!(
        count.load(Ordering::SeqCst),
        2,
        "nouvelle tentative après 30 s"
    );
}
