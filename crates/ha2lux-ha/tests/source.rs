//! Source supervisor (T-HA-11..15): fake link, virtual time, no socket.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use ha2lux_core::HaHealth;
use ha2lux_ha::backoff::Backoff;
use ha2lux_ha::error::HaError;
use ha2lux_ha::reading::{Reading, ReadingValue};
use ha2lux_ha::source::{Feed, FeedEvent, HaSource, Link, Output, SourceConfig, spawn};
use tokio::sync::mpsc;
use tokio::time::Instant;

const BASE_UNIX_MS: i64 = 1_800_000_000_000;

fn lux(value: f64) -> Reading {
    Reading {
        value: ReadingValue::Lux(value),
        raw_state: value.to_string(),
        observed_ms: None,
    }
}

fn no_value() -> Reading {
    Reading {
        value: ReadingValue::NoValue,
        raw_state: "unavailable".to_owned(),
        observed_ms: None,
    }
}

type Script<T> = Mutex<VecDeque<Result<T, HaError>>>;

struct FakeFeed {
    events: mpsc::UnboundedReceiver<Result<FeedEvent, HaError>>,
    reply: mpsc::UnboundedSender<Result<FeedEvent, HaError>>,
    auto_pong: bool,
    state: Arc<State>,
}

impl Feed for FakeFeed {
    async fn next(&mut self) -> Result<FeedEvent, HaError> {
        match self.events.recv().await {
            Some(event) => event,
            None => Err(HaError::Connection("fermé".into())),
        }
    }

    async fn ping(&mut self) -> Result<(), HaError> {
        self.state.pings.fetch_add(1, Ordering::SeqCst);
        if self.auto_pong {
            // The receiver may have gone away: irrelevant.
            let _ = self.reply.send(Ok(FeedEvent::Pong));
        }
        Ok(())
    }
}

#[derive(Default)]
struct State {
    reads: Script<Reading>,
    default_read: Mutex<Option<Result<Reading, HaError>>>,
    connects: Script<()>,
    default_connect: Mutex<Option<Result<(), HaError>>>,
    read_times: Mutex<Vec<Instant>>,
    connect_times: Mutex<Vec<Instant>>,
    feed_tx: Mutex<Option<mpsc::UnboundedSender<Result<FeedEvent, HaError>>>>,
    pings: AtomicU32,
    auto_pong: Mutex<bool>,
}

impl State {
    fn push_read(&self, read: Result<Reading, HaError>) {
        self.reads
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push_back(read);
    }

    fn set_default_read(&self, read: Result<Reading, HaError>) {
        *self
            .default_read
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(read);
    }

    fn set_default_connect(&self, result: Result<(), HaError>) {
        *self
            .default_connect
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(result);
    }

    fn reads_count(&self) -> usize {
        self.read_times
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    fn connects_count(&self) -> usize {
        self.connect_times
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    fn send_feed(&self, event: Result<FeedEvent, HaError>) {
        let tx = self.feed_tx.lock().unwrap_or_else(PoisonError::into_inner);
        tx.as_ref().expect("aucun flux ouvert").send(event).unwrap();
    }

    fn gaps(times: &[Instant]) -> Vec<Duration> {
        times.windows(2).map(|w| w[1] - w[0]).collect()
    }
}

struct FakeLink(Arc<State>);

impl Link for FakeLink {
    type Feed = FakeFeed;

    async fn read(&self) -> Result<Reading, HaError> {
        self.0
            .read_times
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Instant::now());
        if let Some(next) = self
            .0
            .reads
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front()
        {
            return next;
        }
        self.0
            .default_read
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .unwrap_or_else(|| Ok(lux(10.0)))
    }

    async fn connect(&self) -> Result<FakeFeed, HaError> {
        self.0
            .connect_times
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Instant::now());
        let scripted = self
            .0
            .connects
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front();
        let result = scripted.unwrap_or_else(|| {
            self.0
                .default_connect
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
                .unwrap_or(Ok(()))
        });
        result?;
        let (tx, rx) = mpsc::unbounded_channel();
        *self
            .0
            .feed_tx
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(tx.clone());
        let auto_pong = *self
            .0
            .auto_pong
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        Ok(FakeFeed {
            events: rx,
            reply: tx,
            auto_pong,
            state: Arc::clone(&self.0),
        })
    }
}

struct Rig {
    state: Arc<State>,
    source: HaSource,
    start: Instant,
}

fn start(configure: impl FnOnce(&State, &mut SourceConfig)) -> Rig {
    let state = Arc::new(State::default());
    *state.auto_pong.lock().unwrap() = true;
    let mut cfg = SourceConfig::default();
    configure(&state, &mut cfg);
    let began = Instant::now();
    let clock: Arc<dyn Fn() -> i64 + Send + Sync> =
        Arc::new(move || BASE_UNIX_MS + i64::try_from(began.elapsed().as_millis()).unwrap());
    let source = spawn(FakeLink(Arc::clone(&state)), cfg, clock);
    Rig {
        state,
        source,
        start: began,
    }
}

impl Rig {
    async fn advance(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
        for _ in 0..50 {
            tokio::task::yield_now().await;
        }
    }

    fn drain(&mut self) -> Vec<Output> {
        std::iter::from_fn(|| self.source.outputs.try_recv().ok()).collect()
    }

    fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }
}

fn health(outputs: &[Output]) -> Vec<HaHealth> {
    outputs
        .iter()
        .filter_map(|o| {
            if let Output::Health(h) = o {
                Some(*h)
            } else {
                None
            }
        })
        .collect()
}

fn measurements(outputs: &[Output]) -> Vec<f64> {
    outputs
        .iter()
        .filter_map(|o| {
            if let Output::Measurement { lux } = o {
                Some(*lux)
            } else {
                None
            }
        })
        .collect()
}

#[tokio::test(start_paused = true)]
async fn startup_reads_at_once_then_connects_the_websocket_and_slows_down_to_a_refresh() {
    let mut rig = start(|s, _| s.push_read(Ok(lux(42.0))));
    rig.advance(Duration::from_millis(10)).await;
    let out = rig.drain();
    assert_eq!(
        measurements(&out),
        [42.0],
        "première mesure immédiate (REST)"
    );
    assert!(out.contains(&Output::Websocket(true)), "{out:?}");
    assert!(out.contains(&Output::Raw("42".into())));
    assert_eq!(rig.state.connects_count(), 1);
    // WebSocket active: no more 15 s fallback, only a REST refresh at 60 s.
    rig.advance(Duration::from_secs(50)).await;
    assert_eq!(
        rig.state.reads_count(),
        1,
        "pas de sondage à 15 s tant que le flux est actif"
    );
    rig.advance(Duration::from_secs(15)).await;
    assert_eq!(rig.state.reads_count(), 2, "rafraîchissement à 60 s");
}

#[tokio::test(start_paused = true)]
async fn websocket_events_become_measurements_immediately() {
    let mut rig = start(|_, _| {});
    rig.advance(Duration::from_millis(10)).await;
    rig.drain();
    rig.state.send_feed(Ok(FeedEvent::Reading(lux(300.0))));
    rig.advance(Duration::from_millis(1)).await;
    assert_eq!(measurements(&rig.drain()), [300.0]);
}

#[tokio::test(start_paused = true)]
async fn rest_fallback_every_15_seconds_while_the_websocket_is_unavailable() {
    let mut rig = start(|s, _| s.set_default_connect(Err(HaError::Connection("refusé".into()))));
    rig.advance(Duration::from_secs(100)).await;
    let reads = rig.state.read_times.lock().unwrap().clone();
    assert!(reads.len() >= 6, "{} lectures en 100 s", reads.len());
    for gap in State::gaps(&reads) {
        assert_eq!(gap, Duration::from_secs(15), "repli REST toutes les 15 s");
    }
    assert!(!rig.drain().contains(&Output::Websocket(true)));
}

#[tokio::test(start_paused = true)]
async fn t_ha_15_reconnection_backs_off_from_1_to_60_seconds_with_jitter() {
    let rig = start(|s, c| {
        s.set_default_connect(Err(HaError::Connection("refusé".into())));
        c.backoff = Backoff::standard();
    });
    rig.advance(Duration::from_secs(600)).await;
    let attempts = rig.state.connect_times.lock().unwrap().clone();
    let gaps: Vec<f64> = State::gaps(&attempts)
        .iter()
        .map(Duration::as_secs_f64)
        .collect();
    assert!(gaps.len() >= 10, "{gaps:?}");
    for (i, gap) in gaps.iter().enumerate() {
        let base = (1u64 << i.min(6)).min(60) as f64;
        assert!(
            *gap >= base * 0.8 - 0.001 && *gap <= base * 1.2 + 0.001,
            "tentative {i} : {gap} s pour une base de {base} s"
        );
    }
    assert!(
        gaps.iter().all(|g| *g <= 72.1),
        "plafonné à 60 s (+20 % de gigue) : {gaps:?}"
    );
    assert!(
        gaps[0] < 1.3 && gaps.last().copied().unwrap() > 40.0,
        "croissance exponentielle : {gaps:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_cut_websocket_falls_back_to_rest_at_once_and_reconnects_later() {
    let mut rig = start(|_, _| {});
    rig.advance(Duration::from_millis(10)).await;
    rig.drain();
    let reads_before = rig.state.reads_count();
    rig.state
        .send_feed(Err(HaError::Connection("coupé".into())));
    rig.advance(Duration::from_millis(10)).await;
    let out = rig.drain();
    assert!(out.contains(&Output::Websocket(false)), "{out:?}");
    assert!(
        rig.state.reads_count() > reads_before,
        "repli REST immédiat"
    );
    rig.advance(Duration::from_secs(3)).await;
    assert_eq!(rig.state.connects_count(), 2, "reconnexion après ≈ 1 s");
    assert!(rig.drain().contains(&Output::Websocket(true)));
}

#[tokio::test(start_paused = true)]
async fn t_ha_12_six_failed_reads_mean_offline_and_one_success_restores() {
    let mut rig = start(|s, _| {
        s.set_default_connect(Err(HaError::Connection("refusé".into())));
        s.set_default_read(Err(HaError::Connection("coupé".into())));
    });
    rig.advance(Duration::from_secs(74)).await; // reads at 0, 15, 30, 45, 60: 5 failures
    assert!(
        health(&rig.drain()).is_empty(),
        "5 échecs : pas encore hors ligne"
    );
    rig.advance(Duration::from_secs(2)).await; // 6th read at 75 s
    assert_eq!(
        health(&rig.drain()),
        [HaHealth::Offline],
        "6 échecs consécutifs (≈ 90 s) : hors ligne"
    );
    rig.state.set_default_read(Ok(lux(25.0)));
    rig.advance(Duration::from_secs(15)).await;
    let out = rig.drain();
    assert_eq!(health(&out), [HaHealth::Ok]);
    assert_eq!(
        measurements(&out),
        [25.0],
        "retour à l'état sain avec la mesure"
    );
}

#[tokio::test(start_paused = true)]
async fn offline_when_the_websocket_is_cut_and_the_fallback_fails_too() {
    let mut rig = start(|s, _| {
        s.set_default_connect(Err(HaError::Connection("refusé".into())));
        s.set_default_read(Ok(lux(5.0)));
    });
    rig.advance(Duration::from_millis(10)).await;
    rig.drain();
    rig.state.set_default_read(Err(HaError::Timeout));
    rig.advance(Duration::from_secs(100)).await;
    assert_eq!(health(&rig.drain()), [HaHealth::Offline]);
}

#[tokio::test(start_paused = true)]
async fn t_ha_13_unavailable_beyond_the_fallback_delay_is_degraded_then_recovers() {
    let mut rig = start(|s, _| {
        s.set_default_connect(Err(HaError::Connection("refusé".into())));
        s.push_read(Ok(lux(20.0)));
        s.set_default_read(Ok(no_value()));
    });
    rig.advance(Duration::from_secs(14 * 60)).await;
    let out = rig.drain();
    assert_eq!(measurements(&out), [20.0]);
    assert!(
        health(&out).is_empty(),
        "HA joignable, pas encore 15 min sans mesure"
    );
    rig.advance(Duration::from_secs(2 * 60)).await;
    assert_eq!(
        health(&rig.drain()),
        [HaHealth::Degraded],
        "dégradé après 15 min sans mesure valide"
    );
    rig.state.set_default_read(Ok(lux(33.0)));
    rig.advance(Duration::from_secs(20)).await;
    let out = rig.drain();
    assert_eq!(health(&out), [HaHealth::Ok]);
    assert_eq!(measurements(&out), [33.0]);
}

#[tokio::test(start_paused = true)]
async fn degraded_is_detected_even_without_any_new_response() {
    // WebSocket active and silent, REST refresh « unavailable »: the degradation
    // must come from the passage of time, not from an event.
    let mut rig = start(|s, _| {
        s.push_read(Ok(lux(20.0)));
        s.set_default_read(Ok(no_value()));
    });
    rig.advance(Duration::from_secs(20 * 60)).await;
    assert_eq!(health(&rig.drain()), [HaHealth::Degraded]);
}

#[tokio::test(start_paused = true)]
async fn t_ha_11_stale_measurements_are_ignored_even_though_the_link_is_fine() {
    let mut rig = start(|s, _| {
        s.set_default_connect(Err(HaError::Connection("refusé".into())));
        // Measurement 11 min old according to Home Assistant's timestamp.
        let stale = Reading {
            observed_ms: Some(BASE_UNIX_MS - 11 * 60_000),
            ..lux(80.0)
        };
        s.set_default_read(Ok(stale));
    });
    rig.advance(Duration::from_secs(60)).await;
    let out = rig.drain();
    assert!(
        measurements(&out).is_empty(),
        "mesure périmée ignorée : {out:?}"
    );
    assert!(health(&out).is_empty(), "le lien reste sain");
    assert!(
        out.contains(&Output::Raw("80".into())),
        "l'état brut reste affiché"
    );
    // A fresh measurement (recent last_reported) is used.
    let fresh = Reading {
        observed_ms: Some(BASE_UNIX_MS + 59_000 - 30_000),
        ..lux(81.0)
    };
    rig.state.set_default_read(Ok(fresh));
    rig.advance(Duration::from_secs(20)).await;
    assert_eq!(measurements(&rig.drain()), [81.0]);
}

#[tokio::test(start_paused = true)]
async fn t_ha_14_a_refused_token_stops_all_attempts_for_five_minutes() {
    let mut rig = start(|s, _| {
        s.set_default_connect(Err(HaError::Auth));
        s.set_default_read(Err(HaError::Auth));
    });
    rig.advance(Duration::from_secs(4 * 60 + 50)).await;
    let out = rig.drain();
    assert!(out.contains(&Output::AuthRejected(true)));
    assert_eq!(
        health(&out),
        [HaHealth::Offline],
        "hors ligne tout de suite, sans attendre 6 échecs"
    );
    let attempts = rig.state.connects_count() + rig.state.reads_count();
    assert!(
        attempts <= 2,
        "au plus la première tentative (WebSocket et lecture) : {attempts}"
    );
    // After 5 minutes, a single new attempt, then the silence starts again.
    rig.advance(Duration::from_secs(20)).await;
    let after_pause = rig.state.connects_count() + rig.state.reads_count();
    assert!(
        after_pause > attempts && after_pause <= attempts + 2,
        "{attempts} → {after_pause}"
    );
    rig.advance(Duration::from_secs(4 * 60)).await;
    assert_eq!(
        rig.state.connects_count() + rig.state.reads_count(),
        after_pause,
        "toujours en silence"
    );
    assert!(rig.elapsed() > Duration::from_secs(5 * 60));
}

#[tokio::test(start_paused = true)]
async fn a_new_token_is_accepted_after_the_pause_and_clears_the_rejection() {
    let mut rig = start(|s, _| {
        s.set_default_connect(Err(HaError::Auth));
        s.set_default_read(Err(HaError::Auth));
    });
    rig.advance(Duration::from_secs(60)).await;
    rig.drain();
    rig.state.set_default_connect(Ok(()));
    rig.state.set_default_read(Ok(lux(12.0)));
    rig.advance(Duration::from_secs(5 * 60)).await;
    let out = rig.drain();
    assert!(out.contains(&Output::AuthRejected(false)), "{out:?}");
    assert_eq!(health(&out).last(), Some(&HaHealth::Ok));
    assert!(measurements(&out).contains(&12.0));
}

#[tokio::test(start_paused = true)]
async fn the_heartbeat_pings_every_30_seconds_and_a_missing_pong_drops_the_websocket() {
    let mut rig = start(|_, _| {});
    rig.advance(Duration::from_secs(95)).await;
    assert_eq!(
        rig.state.pings.load(Ordering::SeqCst),
        3,
        "un battement toutes les 30 s"
    );
    assert_eq!(
        rig.state.connects_count(),
        1,
        "les pongs maintiennent le flux"
    );
    // No more response: give up after 10 s, then reconnect.
    *rig.state.auto_pong.lock().unwrap() = false;
    rig.state.send_feed(Err(HaError::Connection("x".into()))); // forces a reconnection with a stream that has no pong
    rig.advance(Duration::from_secs(3)).await;
    rig.drain();
    rig.advance(Duration::from_secs(45)).await;
    let out = rig.drain();
    assert!(
        out.contains(&Output::Websocket(false)),
        "pas de pong : WebSocket abandonné : {out:?}"
    );
    assert!(rig.state.connects_count() >= 3);
}

#[tokio::test(start_paused = true)]
async fn poll_soon_triggers_a_confirmation_read_after_about_three_seconds() {
    let mut rig = start(|_, _| {});
    rig.advance(Duration::from_millis(10)).await;
    rig.drain();
    let before = rig.state.reads_count();
    rig.advance(Duration::from_secs(5)).await;
    rig.source.poll_soon();
    rig.advance(Duration::from_millis(2_900)).await;
    assert_eq!(rig.state.reads_count(), before, "pas avant ≈ 3 s");
    rig.advance(Duration::from_millis(200)).await;
    assert_eq!(
        rig.state.reads_count(),
        before + 1,
        "lecture de confirmation rapprochée"
    );
}

#[tokio::test(start_paused = true)]
async fn rest_only_mode_never_opens_a_websocket() {
    let rig = start(|_, c| c.websocket = false);
    rig.advance(Duration::from_secs(60)).await;
    assert_eq!(rig.state.connects_count(), 0);
    assert!(rig.state.reads_count() >= 4);
}

#[tokio::test(start_paused = true)]
async fn dropping_the_source_stops_everything() {
    let rig = start(|_, _| {});
    rig.advance(Duration::from_secs(10)).await;
    let state = Arc::clone(&rig.state);
    drop(rig);
    let (reads, connects) = (state.reads_count(), state.connects_count());
    tokio::time::sleep(Duration::from_secs(600)).await;
    assert_eq!(
        (state.reads_count(), state.connects_count()),
        (reads, connects)
    );
}

#[tokio::test(start_paused = true)]
async fn the_source_stops_by_itself_when_nobody_listens() {
    let mut rig = start(|_, _| {});
    rig.advance(Duration::from_secs(5)).await;
    let state = Arc::clone(&rig.state);
    rig.source.outputs.close();
    rig.advance(Duration::from_secs(300)).await;
    let reads = state.reads_count();
    rig.advance(Duration::from_secs(300)).await;
    assert_eq!(
        state.reads_count(),
        reads,
        "plus de lecture une fois les sorties fermées"
    );
}
