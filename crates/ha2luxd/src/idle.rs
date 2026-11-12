//! Atténuation à l'inactivité : pilote qui transforme une **source d'inactivité**
//! (Wayland `ext-idle-notify-v1`, ou repli D-Bus) et des **blocages** (inhibiteurs,
//! micro/caméra) en événements pour la machine d'états.
//!
//! La décision (atténuer, restaurer, tenir compte d'un blocage apparu après coup)
//! reste entièrement dans la machine d'états ; ce module ne fait que mesurer.
//! Il ne sonde les blocages que lorsque le seuil d'inactivité est dépassé : tant que
//! l'utilisateur travaille, aucune sonde coûteuse n'est lancée.

use std::future::Future;
use std::time::Duration;

use ha2lux_core::Event;
use tokio::time::Instant;

use crate::actor::DaemonHandle;

/// Période de réévaluation des blocages tant que le seuil est dépassé (≤ 1 sonde / 2 s).
pub const PROBE_PERIOD: Duration = Duration::from_secs(2);
/// Attente avant de relancer une source qui s'est arrêtée (compositeur redémarré…).
pub const SOURCE_RETRY: Duration = Duration::from_secs(30);

/// Changement d'état d'inactivité signalé par le compositeur.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdleEvent {
    /// Le délai d'inactivité demandé est écoulé.
    Idled,
    /// L'utilisateur est revenu (clavier, souris, tactile).
    Resumed,
}

/// Source d'inactivité pilotée par événements.
pub trait IdleSource: Send {
    /// Prochain événement ; `None` si la source s'est arrêtée. **Annulable sans perte** :
    /// le pilote peut abandonner l'attente pour sonder les blocages.
    fn next_event(&mut self) -> impl Future<Output = Option<IdleEvent>> + Send;
}

/// Source d'inactivité interrogée périodiquement (repli D-Bus).
pub trait IdleTime: Send {
    /// Durée d'inactivité de la session en millisecondes, si elle est disponible.
    fn idle_ms(&mut self) -> impl Future<Output = Option<u64>> + Send;
}

/// Résultat d'une sonde des blocages.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlockReport {
    /// Raisons lisibles (« inhibiteur : lecteur vidéo », « microphone en cours d'utilisation »…).
    pub reasons: Vec<String>,
}

impl BlockReport {
    /// Au moins un blocage est actif.
    pub fn blocked(&self) -> bool {
        !self.reasons.is_empty()
    }
}

/// Sonde des blocages : inhibiteurs d'inactivité, micro, caméra.
pub trait Blockers: Send {
    /// Sonde les blocages actuels.
    fn probe(&mut self) -> impl Future<Output = BlockReport> + Send;
}

fn elapsed_ms(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Pilote une source par événements. Rend la main quand la source s'arrête ou que le
/// démon est arrêté.
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
                        tracing::debug!(reasons = ?report.reasons, "atténuation bloquée");
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

/// Pilote une source interrogée périodiquement. Ne rend jamais la main, sauf arrêt du démon.
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
            continue; // l'utilisateur travaille : rien à signaler, aucune sonde
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

/// Relance `start(seuil)` chaque fois que les réglages d'atténuation changent
/// (activation, délai), et quand la source s'arrête (après [`SOURCE_RETRY`]). Désactivée,
/// aucune source n'est ouverte. Rend la main quand le démon s'arrête.
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
                    return false; // le démon est arrêté
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
                    // La source s'est arrêtée : nouvelle tentative plus tard, sauf changement de réglages.
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
