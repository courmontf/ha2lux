//! Idle dimming: driver that turns an **idle source**
//! (Wayland `ext-idle-notify-v1`, or D-Bus fallback) and **blockers** (inhibitors,
//! mic/camera) into events for the state machine.
//!
//! The decision (dim, restore, account for a blocker that appeared later)
//! stays entirely in the state machine; this module only measures.
//! It only probes the blockers once the idle threshold is exceeded: as long as
//! the user is working, no costly probe is launched.

use std::future::Future;
use std::time::Duration;

use ha2lux_core::Event;
use tokio::time::Instant;

use crate::actor::DaemonHandle;

/// Re-evaluation period of the blockers while the threshold is exceeded (at most 1 probe / 2 s).
pub const PROBE_PERIOD: Duration = Duration::from_secs(2);
/// Wait before restarting a source that has stopped (compositor restarted...).
pub const SOURCE_RETRY: Duration = Duration::from_secs(30);

/// Idle state change reported by the compositor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdleEvent {
    /// The requested idle delay has elapsed.
    Idled,
    /// The user is back (keyboard, mouse, touch).
    Resumed,
}

/// Event-driven idle source.
pub trait IdleSource: Send {
    /// Next event; `None` if the source has stopped. **Cancel-safe**:
    /// the driver may abandon the wait to probe the blockers.
    fn next_event(&mut self) -> impl Future<Output = Option<IdleEvent>> + Send;
}

/// Periodically polled idle source (D-Bus fallback).
pub trait IdleTime: Send {
    /// Session idle time in milliseconds, if available.
    fn idle_ms(&mut self) -> impl Future<Output = Option<u64>> + Send;
}

/// Result of a blockers probe.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlockReport {
    /// Human-readable reasons (« inhibiteur : lecteur vidéo », « microphone en cours d'utilisation »...).
    pub reasons: Vec<String>,
}

impl BlockReport {
    /// At least one blocker is active.
    pub fn blocked(&self) -> bool {
        !self.reasons.is_empty()
    }
}

/// Blockers probe: idle inhibitors, mic, camera.
pub trait Blockers: Send {
    /// Probes the current blockers.
    fn probe(&mut self) -> impl Future<Output = BlockReport> + Send;
}

fn elapsed_ms(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Drives an event-driven source. Returns when the source stops or the
/// daemon is stopped.
pub async fn drive_events<S: IdleSource, B: Blockers>(
    handle: &DaemonHandle,
    source: &mut S,
    blockers: &mut B,
    threshold_ms: u64,
    period: Duration,
) {
    while let Some(event) = source.next_event().await {
        match event {
            IdleEvent::Resumed => {
                if handle.send_event(Event::Activity).await.is_err() {
                    return;
                }
            }
            IdleEvent::Idled => {
                let idle_since = Instant::now();
                loop {
                    let report = blockers.probe().await;
                    if !report.reasons.is_empty() {
                        tracing::debug!(reasons = ?report.reasons, "dimming blocked");
                    }
                    let probe = Event::IdleProbe {
                        idle_ms: threshold_ms.saturating_add(elapsed_ms(idle_since)),
                        blocked: report.blocked(),
                    };
                    if handle.send_event(probe).await.is_err() {
                        return;
                    }
                    tokio::select! {
                        next = source.next_event() => match next {
                            None => return,
                            Some(IdleEvent::Resumed) => {
                                if handle.send_event(Event::Activity).await.is_err() {
                                    return;
                                }
                                break;
                            }
                            Some(IdleEvent::Idled) => {}
                        },
                        () = tokio::time::sleep(period) => {}
                    }
                }
            }
        }
    }
}

/// Drives a periodically polled source. Never returns, except when the daemon stops.
pub async fn drive_polled<T: IdleTime, B: Blockers>(
    handle: &DaemonHandle,
    time: &mut T,
    blockers: &mut B,
    threshold_ms: u64,
    period: Duration,
) {
    let mut was_idle = false;
    loop {
        tokio::time::sleep(period).await;
        let Some(idle_ms) = time.idle_ms().await else {
            continue;
        };
        let idle = idle_ms >= threshold_ms;
        if !idle && !was_idle {
            continue; // the user is working: nothing to report, no probe
        }
        let blocked = idle && blockers.probe().await.blocked();
        was_idle = idle;
        if handle
            .send_event(Event::IdleProbe { idle_ms, blocked })
            .await
            .is_err()
        {
            return;
        }
    }
}

/// Restarts `start(threshold)` whenever the dimming settings change
/// (activation, delay), and when the source stops (after [`SOURCE_RETRY`]). When disabled,
/// no source is opened. Returns when the daemon stops.
pub async fn supervise<F, Fut>(handle: DaemonHandle, mut start: F)
where
    F: FnMut(u64) -> Fut,
    Fut: Future<Output = ()>,
{
    let mut snapshots = handle.subscribe();
    loop {
        let (enabled, minutes) = {
            let snapshot = snapshots.borrow_and_update();
            (snapshot.idle_enabled, snapshot.idle_minutes)
        };
        let settings_changed = async {
            loop {
                if snapshots.changed().await.is_err() {
                    return false; // the daemon is stopped
                }
                let snapshot = snapshots.borrow();
                if (snapshot.idle_enabled, snapshot.idle_minutes) != (enabled, minutes) {
                    return true;
                }
            }
        };
        tokio::pin!(settings_changed);
        if enabled {
            let threshold_ms = u64::from(minutes) * 60_000;
            tokio::select! {
                () = start(threshold_ms) => {
                    // The source has stopped: retry later, unless the settings change.
                    tokio::select! {
                        () = tokio::time::sleep(SOURCE_RETRY) => {}
                        alive = &mut settings_changed => if !alive { return },
                    }
                }
                alive = &mut settings_changed => if !alive { return },
            }
        } else if !(&mut settings_changed).await {
            return;
        }
    }
}
