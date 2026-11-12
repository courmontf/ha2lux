//! Measurement source: persistent WebSocket (subscription) with **REST fallback** while it
//! is unavailable, reconnection with exponential delay and jitter, heartbeat, and
//! link health tracking (staleness, offline, degraded).
//!
//! The supervisor is written against two small traits ([`Link`], [`Feed`]): tests
//! run it in virtual time with a fake link, and the real link ([`HaLink`]) is tested
//! separately against real sockets.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use ha2lux_core::{HaHealth, HealthConfig, HealthTracker};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::backoff::{Backoff, Jitter};
use crate::config::HaConfig;
use crate::error::HaError;
use crate::reading::{Reading, ReadingValue};
use crate::rest::RestClient;
use crate::ws::{WsClient, WsEvent};

/// Event from a persistent stream.
#[derive(Clone, Debug, PartialEq)]
pub enum FeedEvent {
    /// New reading.
    Reading(Reading),
    /// Heartbeat response.
    Pong,
}

/// Persistent stream (WebSocket).
pub trait Feed: Send {
    /// Next event. **Cancel-safe.**
    fn next(&mut self) -> impl Future<Output = Result<FeedEvent, HaError>> + Send;
    /// Sends a heartbeat.
    fn ping(&mut self) -> impl Future<Output = Result<(), HaError>> + Send;
}

/// Link to Home Assistant: one-off read (REST) and opening of a stream.
pub trait Link: Send + Sync {
    /// Type of the persistent stream.
    type Feed: Feed;
    /// One-off read.
    fn read(&self) -> impl Future<Output = Result<Reading, HaError>> + Send;
    /// Opens the persistent stream (connection, authentication, subscription).
    fn connect(&self) -> impl Future<Output = Result<Self::Feed, HaError>> + Send;
}

/// What the source publishes.
#[derive(Clone, Debug, PartialEq)]
pub enum Output {
    /// Valid, fresh measurement (to hand to the filter).
    Measurement {
        /// Illuminance in lux.
        lux: f64,
    },
    /// The link health changed.
    Health(HaHealth),
    /// Raw state as displayed by Home Assistant (status panel).
    Raw(String),
    /// The persistent stream is (in)active.
    Websocket(bool),
    /// The token is rejected (`true`), or no longer is (`false`).
    AuthRejected(bool),
}

/// Source settings.
#[derive(Clone, Debug)]
pub struct SourceConfig {
    /// REST fallback period while the persistent stream is unavailable (15 s).
    pub poll_period: Duration,
    /// REST refresh period while the stream is active (60 s): measures the
    /// freshness of a sensor whose value does not change.
    pub refresh_period: Duration,
    /// Heartbeat period (30 s).
    pub heartbeat: Duration,
    /// Maximum wait for the heartbeat response (10 s).
    pub pong_timeout: Duration,
    /// Delay of the confirmation poll after a light jump (3 s).
    pub confirm_delay: Duration,
    /// Quiet period after an authentication refusal (5 min).
    pub auth_pause: Duration,
    /// Exponential reconnection (1 s → 60 s).
    pub backoff: Backoff,
    /// Link health.
    pub health: HealthConfig,
    /// Use the persistent stream (otherwise REST only).
    pub websocket: bool,
}

impl Default for SourceConfig {
    fn default() -> Self {
        Self {
            poll_period: Duration::from_secs(15),
            refresh_period: Duration::from_secs(60),
            heartbeat: Duration::from_secs(30),
            pong_timeout: Duration::from_secs(10),
            confirm_delay: Duration::from_secs(3),
            auth_pause: Duration::from_secs(300),
            backoff: Backoff::standard(),
            health: HealthConfig::default(),
            websocket: true,
        }
    }
}

/// Running source; stopped when dropped.
pub struct HaSource {
    /// Measurements and state changes.
    pub outputs: mpsc::UnboundedReceiver<Output>,
    poke: mpsc::UnboundedSender<()>,
    task: JoinHandle<()>,
}

impl HaSource {
    /// Requests a prompt read (≈ 3 s): confirmation of a large light jump.
    pub fn poll_soon(&self) {
        // The task may have stopped: irrelevant.
        let _ = self.poke.send(());
    }
}

impl Drop for HaSource {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Starts the source. `now_unix_ms` gives the Unix time (for the age of a measurement).
pub fn spawn<L>(
    link: L,
    config: SourceConfig,
    now_unix_ms: Arc<dyn Fn() -> i64 + Send + Sync>,
) -> HaSource
where
    L: Link + 'static,
    L::Feed: 'static,
{
    let (out, outputs) = mpsc::unbounded_channel();
    let (poke, poke_rx) = mpsc::unbounded_channel();
    let seed = u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |d| d.as_nanos())
            % u128::from(u64::MAX),
    )
    .unwrap_or(1);
    let runner = Runner::new(link, config, out, poke_rx, now_unix_ms, Jitter::new(seed));
    HaSource {
        outputs,
        poke,
        task: tokio::spawn(runner.run()),
    }
}

struct Runner<L: Link> {
    link: L,
    cfg: SourceConfig,
    out: mpsc::UnboundedSender<Output>,
    poke_rx: mpsc::UnboundedReceiver<()>,
    poke_open: bool,
    now_unix_ms: Arc<dyn Fn() -> i64 + Send + Sync>,
    jitter: Jitter,
    start: Instant,
    tracker: HealthTracker,
    backoff: Backoff,
    feed: Option<L::Feed>,
    next_connect: Option<Instant>,
    next_read: Option<Instant>,
    extra_read: Option<Instant>,
    heartbeat_at: Option<Instant>,
    pong_deadline: Option<Instant>,
    auth_until: Option<Instant>,
    closed: bool,
}

enum Woke<E> {
    Poke(Option<()>),
    Feed(Result<E, HaError>),
    Timer,
}

impl<L: Link> Runner<L> {
    fn new(
        link: L,
        cfg: SourceConfig,
        out: mpsc::UnboundedSender<Output>,
        poke_rx: mpsc::UnboundedReceiver<()>,
        now_unix_ms: Arc<dyn Fn() -> i64 + Send + Sync>,
        jitter: Jitter,
    ) -> Self {
        let start = Instant::now();
        Self {
            tracker: HealthTracker::new(cfg.health, 0),
            backoff: cfg.backoff,
            next_connect: cfg.websocket.then_some(start),
            next_read: Some(start),
            link,
            cfg,
            out,
            poke_rx,
            poke_open: true,
            now_unix_ms,
            jitter,
            start,
            feed: None,
            extra_read: None,
            heartbeat_at: None,
            pong_deadline: None,
            auth_until: None,
            closed: false,
        }
    }

    fn ms(&self, at: Instant) -> u64 {
        u64::try_from(at.saturating_duration_since(self.start).as_millis()).unwrap_or(u64::MAX)
    }

    fn emit(&mut self, output: Output) {
        if self.out.send(output).is_err() {
            self.closed = true; // nobody is listening anymore: stop
        }
    }

    fn apply(&mut self, update: ha2lux_core::health::Update) {
        if update.changed {
            self.emit(Output::Health(update.health));
        }
        if let Some(lux) = update.measurement {
            self.emit(Output::Measurement { lux });
        }
    }

    fn auth_blocked(&self, now: Instant) -> bool {
        self.auth_until.is_some_and(|until| now < until)
    }

    fn handle_reading(&mut self, now: Instant, reading: Reading) {
        if self.auth_until.take().is_some() {
            self.emit(Output::AuthRejected(false));
        }
        let age_ms = reading.age_ms((self.now_unix_ms)());
        let update = match reading.value {
            ReadingValue::Lux(lux) => self.tracker.on_reading(self.ms(now), lux, age_ms),
            ReadingValue::NoValue => self.tracker.on_no_value(self.ms(now)),
        };
        self.emit(Output::Raw(reading.raw_state));
        self.apply(update);
    }

    fn on_auth_failure(&mut self, now: Instant) {
        tracing::warn!("Home Assistant token refused: no new attempt for several minutes");
        let until = now + self.cfg.auth_pause;
        if self.auth_until.replace(until).is_none() {
            self.emit(Output::AuthRejected(true));
        }
        self.drop_feed(now, false);
        self.next_connect = self.cfg.websocket.then_some(until);
        self.next_read = Some(until);
        self.extra_read = None;
        let update = self.tracker.on_auth_failure(self.ms(now));
        self.apply(update);
    }

    fn drop_feed(&mut self, now: Instant, schedule_reconnect: bool) {
        if self.feed.take().is_some() {
            self.emit(Output::Websocket(false));
        }
        self.heartbeat_at = None;
        self.pong_deadline = None;
        if schedule_reconnect && self.cfg.websocket {
            let delay = self.backoff.next_delay(self.jitter.sample());
            self.next_connect = Some(now + delay);
        }
        // REST fallback right away, then every 15 s.
        self.next_read = Some(self.next_read.map_or(now, |t| t.min(now)));
    }

    async fn try_connect(&mut self, now: Instant) {
        match self.link.connect().await {
            Ok(feed) => {
                tracing::info!("Home Assistant WebSocket connected");
                self.feed = Some(feed);
                self.backoff.reset();
                self.next_connect = None;
                self.heartbeat_at = Some(now + self.cfg.heartbeat);
                self.pong_deadline = None;
                let refresh = now + self.cfg.refresh_period;
                self.next_read = Some(self.next_read.map_or(refresh, |t| t.min(refresh)));
                self.emit(Output::Websocket(true));
            }
            Err(error) if error.is_auth() => self.on_auth_failure(now),
            Err(error) => {
                let delay = self.backoff.next_delay(self.jitter.sample());
                tracing::debug!(%error, ?delay, "WebSocket indisponible : nouvelle tentative plus tard");
                self.next_connect = Some(now + delay);
            }
        }
    }

    async fn do_read(&mut self, now: Instant) {
        self.extra_read = None;
        match self.link.read().await {
            Ok(reading) => self.handle_reading(now, reading),
            Err(error) if error.is_auth() => {
                self.on_auth_failure(now);
                return;
            }
            Err(error) => {
                tracing::debug!(%error, "Home Assistant read failed");
                let update = self.tracker.on_failure(self.ms(now));
                self.apply(update);
            }
        }
        let period = if self.feed.is_some() {
            self.cfg.refresh_period
        } else {
            self.cfg.poll_period
        };
        self.next_read = Some(now + period);
    }

    async fn heartbeat(&mut self, now: Instant) {
        let Some(feed) = self.feed.as_mut() else {
            return;
        };
        match feed.ping().await {
            Ok(()) => {
                self.pong_deadline = Some(now + self.cfg.pong_timeout);
                self.heartbeat_at = Some(now + self.cfg.heartbeat);
            }
            Err(_) => self.drop_feed(now, true),
        }
    }

    fn earliest_deadline(&self, now: Instant) -> Instant {
        let blocked = self.auth_blocked(now);
        let health = self
            .tracker
            .next_deadline()
            .map(|ms| self.start + Duration::from_millis(ms));
        [
            if self.feed.is_none() {
                self.next_connect
            } else {
                None
            },
            self.next_read,
            if blocked { None } else { self.extra_read },
            if self.feed.is_some() {
                self.heartbeat_at
            } else {
                None
            },
            self.pong_deadline,
            health,
        ]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(now + Duration::from_secs(3_600))
    }

    async fn run(mut self) {
        while !self.closed {
            let now = Instant::now();
            let blocked = self.auth_blocked(now);
            if let Some(ms) = self.tracker.next_deadline()
                && self.ms(now) >= ms
            {
                let update = self.tracker.tick(self.ms(now));
                self.apply(update);
            }
            if self.feed.is_none() && !blocked && self.next_connect.is_some_and(|t| now >= t) {
                self.try_connect(now).await;
                continue;
            }
            let read_due = self.next_read.is_some_and(|t| now >= t)
                || (!blocked && self.extra_read.is_some_and(|t| now >= t));
            if read_due && !blocked {
                self.do_read(now).await;
                continue;
            }
            if self.pong_deadline.is_some_and(|t| now >= t) {
                tracing::debug!("no reply to heartbeat: WebSocket abandoned");
                self.drop_feed(now, true);
                continue;
            }
            if self.feed.is_some() && self.heartbeat_at.is_some_and(|t| now >= t) {
                self.heartbeat(now).await;
                continue;
            }
            let deadline = self
                .earliest_deadline(now)
                .max(now + Duration::from_millis(1));
            let woke = {
                let poke_open = self.poke_open;
                let feed = &mut self.feed;
                let poke_rx = &mut self.poke_rx;
                tokio::select! {
                    poke = async { if poke_open { poke_rx.recv().await } else { std::future::pending().await } } => Woke::Poke(poke),
                    event = async { match feed.as_mut() { Some(f) => f.next().await, None => std::future::pending().await } } => Woke::Feed(event),
                    () = tokio::time::sleep_until(deadline) => Woke::Timer,
                }
            };
            let now = Instant::now();
            match woke {
                Woke::Poke(Some(())) => {
                    let due = now + self.cfg.confirm_delay;
                    self.extra_read = Some(self.extra_read.map_or(due, |t| t.min(due)));
                }
                Woke::Poke(None) => self.poke_open = false,
                Woke::Feed(Ok(FeedEvent::Reading(reading))) => self.handle_reading(now, reading),
                Woke::Feed(Ok(FeedEvent::Pong)) => self.pong_deadline = None,
                Woke::Feed(Err(error)) => {
                    tracing::debug!(%error, "WebSocket dropped: falling back to REST, reconnection scheduled");
                    self.drop_feed(now, true);
                }
                Woke::Timer => {}
            }
        }
    }
}

/// Real link: REST + WebSocket.
pub struct HaLink {
    config: HaConfig,
    rest: RestClient,
}

impl HaLink {
    /// Creates the link.
    pub fn new(config: HaConfig) -> Result<Self, HaError> {
        let rest = RestClient::new(&config)?;
        Ok(Self { config, rest })
    }
}

/// Real WebSocket stream.
pub struct WsFeed(WsClient);

impl Feed for WsFeed {
    async fn next(&mut self) -> Result<FeedEvent, HaError> {
        loop {
            match self.0.next_event().await? {
                WsEvent::State(reading) => return Ok(FeedEvent::Reading(reading)),
                WsEvent::Pong => return Ok(FeedEvent::Pong),
                WsEvent::Other => {}
            }
        }
    }

    async fn ping(&mut self) -> Result<(), HaError> {
        self.0.ping().await
    }
}

impl Link for HaLink {
    type Feed = WsFeed;

    async fn read(&self) -> Result<Reading, HaError> {
        self.rest.read().await
    }

    async fn connect(&self) -> Result<WsFeed, HaError> {
        let mut ws = WsClient::connect(&self.config).await?;
        ws.subscribe_entity(&self.config.entity).await?;
        Ok(WsFeed(ws))
    }
}

/// Current Unix time in milliseconds.
pub fn system_unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}
