//! Machine d'états de pilotage (sans E/S) : voir `specs/ARCHITECTURE.md` §2.
//!
//! Le [`Controller`] reçoit des [`Event`] datés (`Now`) et renvoie des
//! [`Command`] à exécuter par les adaptateurs. Il ne touche jamais le
//! matériel, ne lit jamais l'horloge et ne dort jamais : c'est l'appelant qui
//! fait vivre le temps (horloge virtuelle dans les tests).
//!
//! Trois couches : mode de base (auto-lux > planning > statique), maintien
//! manuel, surcouche d'inactivité. La surcouche ne modifie jamais le maître.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::backend::DisplayId;
use crate::clock::Now;
use crate::config::{Config, IdleConfig, Tuning};
use crate::curve::{Curve, LearnOutcome, log_scale};
use crate::filter::Filter;
use crate::gate::Gate;
use crate::range::{Range, clamp_percent, reconstruct_master};
use crate::schedule::{Schedule, ScheduleError, Suspension};

/// Délais de relance après rejet d'une liste d'écrans factice (ms).
pub const FAKE_LIST_RETRY_MS: [u64; 4] = [2_000, 5_000, 10_000, 20_000];
/// Délai de la seconde ré-application après un réveil (ms).
pub const SECOND_REAPPLY_MS: u64 = 3_000;
/// Fenêtre après un réveil pendant laquelle un écran dont le niveau lu a dérivé est réécrit
/// (certains moniteurs finissent de se réinitialiser longtemps après le retour de la veille).
pub const WAKE_DRIFT_WINDOW_MS: u64 = 60_000;
/// Délai de la vérification suivante quand une dérive vient d'être corrigée.
pub const WAKE_DRIFT_RECHECK_MS: u64 = 5_000;
/// Délai de rescan après un réveil (ms) : les écrans ne répondent pas tout de suite.
pub const WAKE_RESCAN_MS: u64 = 1_500;
/// Échecs d'écriture consécutifs avant de déclarer un écran indisponible.
pub const FAILURES_BEFORE_UNAVAILABLE: u8 = 3;
/// Écart (échelle log) de lumière qui met fin à la pause de l'auto après un preset.
const PAUSE_BREAK_LOG_DELTA: f64 = 0.3;

/// Réglages utiles à la machine d'états.
#[derive(Clone, Debug, PartialEq)]
pub struct ControllerConfig {
    /// Planning validé.
    pub schedule: Schedule,
    /// Planning activé.
    pub schedule_enabled: bool,
    /// Atténuation à l'inactivité.
    pub idle: IdleConfig,
    /// Auto-luminosité demandée.
    pub auto_enabled: bool,
    /// URL, jeton et entité sont renseignés.
    pub auto_configured: bool,
    /// Réglages fins.
    pub tuning: Tuning,
}

impl ControllerConfig {
    /// Extrait les réglages d'une configuration validée.
    pub fn from_config(config: &Config, auto_configured: bool) -> Result<Self, ScheduleError> {
        Ok(Self {
            schedule: config.build_schedule()?,
            schedule_enabled: config.schedule.enabled,
            idle: config.idle.clone(),
            auto_enabled: config.auto.enabled,
            auto_configured,
            tuning: config.auto.tuning.clone(),
        })
    }
}

impl Default for ControllerConfig {
    fn default() -> Self {
        let config = Config::default();
        Self {
            schedule: Schedule::default(),
            schedule_enabled: config.schedule.enabled,
            idle: config.idle,
            auto_enabled: false,
            auto_configured: false,
            tuning: config.auto.tuning,
        }
    }
}

/// État restauré au démarrage (fichier d'état).
#[derive(Clone, Debug, Default)]
pub struct InitialState {
    /// Dernier maître connu.
    pub master: Option<u8>,
    /// Suspension du planning en cours.
    pub suspension: Option<Suspension>,
    /// Courbe apprise.
    pub curve: Curve,
    /// Plages des écrans connus (même débranchés).
    pub ranges: BTreeMap<DisplayId, Range>,
}

/// Santé du lien Home Assistant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HaHealth {
    /// Lien sain.
    Ok,
    /// Hors ligne (échecs consécutifs).
    Offline,
    /// Dégradé (pas de mesure valide depuis trop longtemps).
    Degraded,
}

/// Fonctions activables.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Feature {
    /// Planning.
    Schedule,
    /// Atténuation à l'inactivité.
    IdleDim,
    /// Auto-luminosité.
    Auto,
}

/// État d'un écran vu par la machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayState {
    /// Répond et peut être piloté.
    Controllable,
    /// Déjà contrôlable ou connu de la config, ne répond pas : relancé.
    Unavailable,
    /// N'a jamais répondu : relancé seulement sur événement.
    NotControllable,
    /// Plus présent dans la dernière liste acceptée.
    Absent,
}

/// Écran issu d'une énumération.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    /// Identifiant stable.
    pub id: DisplayId,
    /// La sonde a réussi (lecture valide, maximum > 0).
    pub controllable: bool,
    /// Niveau écran lu (0-100), s'il l'a été.
    pub level: Option<u8>,
}

/// Événements d'entrée.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// Définir le maître (0-100, sinon rejeté).
    SetMaster(i64),
    /// Variation du maître (−100..=100).
    Step(i64),
    /// Niveau direct d'un écran (0-100 de son niveau, hors plage).
    SetDisplay {
        /// Écran visé.
        display: DisplayId,
        /// Niveau écran.
        level: i64,
    },
    /// Preset : niveau maître tenu temporairement, jamais appris.
    Preset(i64),
    /// Nouvelle plage pour un écran.
    SetRange {
        /// Écran visé.
        display: DisplayId,
        /// Borne basse brute.
        low: i64,
        /// Borne haute brute.
        high: i64,
    },
    /// Activation d'une fonction.
    Feature(Feature, bool),
    /// Oublier la courbe apprise.
    ResetCurve,
    /// Annuler le dernier apprentissage.
    UndoLearn,
    /// Configuration rechargée.
    Reload(Box<ControllerConfig>),
    /// Évaluation périodique du planning (60 s).
    PlanningTick,
    /// Pas de rampe demandé par [`Command::ArmRampTimer`].
    RampTick,
    /// Mesure de lux valide.
    Lux(f64),
    /// Changement de santé du lien Home Assistant.
    Ha(HaHealth),
    /// L'auto-luminosité devient configurée (URL, entité et jeton présents) ou ne l'est plus.
    AutoConfigured(bool),
    /// Sonde d'inactivité.
    IdleProbe {
        /// Durée d'inactivité de la session (ms).
        idle_ms: u64,
        /// Un blocage est actif (inhibiteur, plein écran, micro/caméra).
        blocked: bool,
    },
    /// Reprise d'activité de l'utilisateur.
    Activity,
    /// Réveil de veille, déverrouillage, hotplug, écran rallumé.
    Wake,
    /// Seconde ré-application différée après un réveil.
    Reapply,
    /// Nouvelle liste d'écrans issue d'un rescan.
    DisplaysListed(Vec<Listed>),
    /// Résultat d'une écriture demandée par [`Command::Write`].
    WriteResult {
        /// Écran concerné.
        display: DisplayId,
        /// Niveau écrit.
        level: u8,
        /// Succès de l'écriture.
        ok: bool,
    },
}

/// Données à persister (le démon débounce).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Persist {
    /// Maître courant.
    Master,
    /// Courbe apprise.
    Curve,
    /// Suspension du planning.
    Suspension,
    /// Plages des écrans.
    Ranges,
}

/// Informations destinées à l'utilisateur.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    /// Apprentissage refusé : trop de points contredits.
    LearnRefused {
        /// Nombre de points contredits.
        contradicted: usize,
    },
    /// Écran passé en indisponible après des échecs répétés.
    DisplayUnavailable(DisplayId),
    /// Liste d'écrans rejetée (factice) ; nouvelle tentative prévue.
    ListRejected {
        /// Délai avant le prochain rescan (ms).
        retry_in_ms: u64,
    },
}

/// Commandes de sortie, exécutées par les adaptateurs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// Écrire le niveau écran (0-100) ; l'exécuteur coalesce et convertit en brut.
    Write {
        /// Écran visé.
        display: DisplayId,
        /// Niveau écran 0-100.
        level: u8,
    },
    /// Lancer un rescan (ciblé si `target`), après `delay_ms`.
    Rescan {
        /// Écran ciblé, ou tous.
        target: Option<DisplayId>,
        /// Délai avant de scanner.
        delay_ms: u64,
    },
    /// Émettre [`Event::RampTick`] dans `in_ms`.
    ArmRampTimer {
        /// Délai en ms.
        in_ms: u64,
    },
    /// Émettre [`Event::Reapply`] dans `in_ms`.
    ArmReapply {
        /// Délai en ms.
        in_ms: u64,
    },
    /// Rapprocher le prochain sondage Home Assistant (confirmation d'un saut).
    PollLuxSoon {
        /// Délai en ms.
        in_ms: u64,
    },
    /// Persister une donnée.
    Persist(Persist),
    /// Information pour l'interface.
    Notify(Notice),
}

/// Requêtes utilisateur invalides, refusées sans effet.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ControlError {
    /// Valeur hors bornes.
    #[error("{what} hors bornes : {value}")]
    OutOfRange {
        /// Grandeur concernée.
        what: &'static str,
        /// Valeur reçue.
        value: i64,
    },
    /// Écran inconnu.
    #[error("écran inconnu : {0}")]
    UnknownDisplay(DisplayId),
    /// Écran connu mais pas contrôlable pour le moment.
    #[error("écran indisponible : {0}")]
    DisplayUnavailable(DisplayId),
    /// Aucun écran contrôlable.
    #[error("aucun écran contrôlable")]
    NoControllableDisplay,
}

/// Source qui décide du maître (couche 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BaseMode {
    /// Capteur de lux Home Assistant.
    AutoLux,
    /// Planning horaire.
    Planning,
    /// Le dernier maître reste.
    Static,
}

/// État de l'auto-luminosité pour le panneau d'état.
#[derive(Clone, Debug, PartialEq)]
pub struct AutoInfo {
    /// Lux lissé par le filtre (aucune mesure : `None`).
    pub smoothed_lux: Option<f64>,
    /// Lux le plus récent (mesure en attente de confirmation, sinon lux lissé).
    pub latest_lux: Option<f64>,
    /// Un grand écart attend sa confirmation.
    pub adjusting: bool,
    /// Points de la courbe effectivement utilisée (lux, niveau maître).
    pub curve: Vec<(f64, u8)>,
    /// La courbe est la courbe par défaut (rien d'appris).
    pub default_curve: bool,
    /// Niveau cible de la courbe pour le lux lissé.
    pub target: Option<u8>,
}

/// Instantané de l'état, pour l'interface et les tests.
#[derive(Clone, Debug, PartialEq)]
pub struct Status {
    /// Maître courant.
    pub master: Option<u8>,
    /// Dernier maître confirmé écrit sur tous les écrans visés.
    pub applied_master: Option<u8>,
    /// Source de base.
    pub mode: BaseMode,
    /// Suspension du planning.
    pub suspension: Option<Suspension>,
    /// Atténuation d'inactivité en cours.
    pub dimmed: bool,
    /// Cible de rampe en cours.
    pub ramp_target: Option<u8>,
    /// Santé Home Assistant.
    pub ha_health: HaHealth,
    /// Au moins une mesure valide reçue.
    pub has_measure: bool,
    /// Auto en pause après un preset.
    pub auto_paused: bool,
    /// Écrans : identifiant, état, dernier niveau commandé, plage.
    pub displays: Vec<(DisplayId, DisplayState, Option<u8>, Range)>,
}

#[derive(Clone, Debug)]
struct Disp {
    range: Range,
    state: DisplayState,
    level: Option<u8>,
    failures: u8,
}

#[derive(Clone, Debug)]
struct Pending {
    master: u8,
    waiting: BTreeSet<DisplayId>,
}

#[derive(Clone, Copy, Debug)]
struct Pause {
    until_ms: u64,
    lux: f64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ManualKind {
    Normal,
    Preset,
}

/// Machine d'états de pilotage.
pub struct Controller {
    cfg: ControllerConfig,
    displays: BTreeMap<DisplayId, Disp>,
    master: Option<u8>,
    applied_master: Option<u8>,
    pending: Option<Pending>,
    suspension: Option<Suspension>,
    dimmed: bool,
    ramp: Option<u8>,
    ramp_armed: bool,
    health: HaHealth,
    has_measure: bool,
    pause: Option<Pause>,
    filter: Filter,
    gate: Gate,
    curve: Curve,
    reapply_on_list: bool,
    /// Fin de la fenêtre de correction de dérive qui suit un réveil.
    drift_until_ms: Option<u64>,
    fake_attempts: usize,
}

impl Controller {
    /// Crée la machine à partir de la configuration et de l'état persisté.
    pub fn new(cfg: ControllerConfig, initial: InitialState) -> Self {
        let displays = initial
            .ranges
            .into_iter()
            .map(|(id, range)| {
                (
                    id,
                    Disp {
                        range,
                        state: DisplayState::Absent,
                        level: None,
                        failures: 0,
                    },
                )
            })
            .collect();
        Self {
            filter: Filter::new(cfg.tuning.filter_params()),
            gate: Gate::new(cfg.tuning.gate_params()),
            cfg,
            displays,
            master: initial.master.map(|m| m.min(100)),
            applied_master: None,
            pending: None,
            suspension: initial.suspension,
            dimmed: false,
            ramp: None,
            ramp_armed: false,
            health: HaHealth::Ok,
            has_measure: false,
            pause: None,
            curve: initial.curve,
            reapply_on_list: false,
            drift_until_ms: None,
            fake_attempts: 0,
        }
    }

    /// Source de base : auto-lux > planning > statique.
    pub fn base_mode(&self) -> BaseMode {
        if self.auto_owner() {
            BaseMode::AutoLux
        } else if self.planning_active() {
            BaseMode::Planning
        } else {
            BaseMode::Static
        }
    }

    /// Courbe apprise.
    pub fn curve(&self) -> &Curve {
        &self.curve
    }

    /// État de l'auto-luminosité (filtre, courbe, cible).
    pub fn auto_info(&self) -> AutoInfo {
        let has_value = self.filter.has_value();
        AutoInfo {
            smoothed_lux: has_value.then(|| self.filter.smoothed_lux()),
            latest_lux: has_value.then(|| self.filter.latest_lux()),
            adjusting: self.filter.is_pending() || self.ramp.is_some(),
            curve: self
                .curve
                .effective_points()
                .iter()
                .map(|p| (p.lux, p.level))
                .collect(),
            default_curve: self.curve.points().is_empty(),
            target: has_value.then(|| self.curve.level_at(self.filter.smoothed_lux())),
        }
    }

    /// Instantané de l'état.
    pub fn status(&self) -> Status {
        Status {
            master: self.master,
            applied_master: self.applied_master,
            mode: self.base_mode(),
            suspension: self.suspension,
            dimmed: self.dimmed,
            ramp_target: self.ramp,
            ha_health: self.health,
            has_measure: self.has_measure,
            auto_paused: self.pause.is_some(),
            displays: self
                .displays
                .iter()
                .map(|(id, d)| (id.clone(), d.state, d.level, d.range))
                .collect(),
        }
    }

    /// Mémorise la plage d'un écran, vu ou non (plages du fichier de
    /// configuration, y compris pour des écrans débranchés). Aucune écriture :
    /// elle prend effet à la prochaine application (rechargement, réveil…).
    pub fn remember_range(&mut self, id: DisplayId, range: Range) {
        self.displays
            .entry(id)
            .and_modify(|d| d.range = range)
            .or_insert(Disp {
                range,
                state: DisplayState::Absent,
                level: None,
                failures: 0,
            });
    }

    /// Traite un événement et renvoie les commandes à exécuter.
    ///
    /// Une requête utilisateur invalide renvoie une erreur sans aucun effet.
    pub fn handle(&mut self, now: Now, event: Event) -> Result<Vec<Command>, ControlError> {
        let mut out = Vec::new();
        match event {
            Event::SetMaster(m) => {
                let m = percent("niveau maître", m)?;
                self.require_controllable()?;
                self.manual_policy(now, m, ManualKind::Normal, &mut out);
                self.apply_master(m, false, &mut out);
            }
            Event::Step(delta) => {
                if !(-100..=100).contains(&delta) {
                    return Err(ControlError::OutOfRange {
                        what: "pas",
                        value: delta,
                    });
                }
                self.require_controllable()?;
                self.ensure_master(&mut out);
                let m = clamp_percent(i64::from(self.master.unwrap_or(50)) + delta);
                self.manual_policy(now, m, ManualKind::Normal, &mut out);
                self.apply_master(m, false, &mut out);
            }
            Event::Preset(level) => {
                let m = percent("preset", level)?;
                self.require_controllable()?;
                self.manual_policy(now, m, ManualKind::Preset, &mut out);
                self.apply_master(m, false, &mut out);
            }
            Event::SetDisplay { display, level } => {
                self.set_display(now, display, level, &mut out)?
            }
            Event::SetRange { display, low, high } => {
                self.set_range(display, low, high, &mut out)?
            }
            Event::Feature(feature, on) => self.set_feature(now, feature, on, &mut out),
            Event::ResetCurve => {
                self.curve.reset();
                self.gate.reset();
                out.push(Command::Persist(Persist::Curve));
                if self.auto_owner() && !self.dimmed {
                    self.apply_auto_forced(&mut out);
                }
            }
            Event::UndoLearn => {
                if self.curve.undo() {
                    out.push(Command::Persist(Persist::Curve));
                }
            }
            Event::Reload(cfg) => self.reload(now, *cfg, &mut out),
            Event::PlanningTick => self.planning_tick(now, &mut out),
            Event::RampTick => self.ramp_tick(&mut out),
            Event::Lux(lux) => self.on_lux(now, lux, &mut out),
            Event::Ha(health) => self.on_health(now, health, &mut out),
            Event::AutoConfigured(configured) => self.on_auto_configured(now, configured, &mut out),
            Event::IdleProbe { idle_ms, blocked } => {
                self.on_idle_probe(now, idle_ms, blocked, &mut out)
            }
            Event::Activity => {
                if self.dimmed {
                    self.restore(now, &mut out);
                }
            }
            Event::Wake => {
                self.reapply_on_list = true;
                self.drift_until_ms = Some(now.ms + WAKE_DRIFT_WINDOW_MS);
                out.push(Command::Rescan {
                    target: None,
                    delay_ms: WAKE_RESCAN_MS,
                });
            }
            Event::Reapply => self.reapply(now, &mut out),
            Event::DisplaysListed(list) => self.on_list(now, list, &mut out),
            Event::WriteResult { display, level, ok } => {
                self.on_write_result(&display, level, ok, &mut out)
            }
        }
        Ok(out)
    }

    // ---- Prédicats ---------------------------------------------------------

    fn auto_owner(&self) -> bool {
        self.cfg.auto_enabled
            && self.cfg.auto_configured
            && self.health == HaHealth::Ok
            && self.has_measure
    }

    fn planning_active(&self) -> bool {
        self.cfg.schedule_enabled && !self.cfg.schedule.is_empty()
    }

    fn require_controllable(&self) -> Result<(), ControlError> {
        if self
            .displays
            .values()
            .any(|d| d.state == DisplayState::Controllable)
        {
            Ok(())
        } else {
            Err(ControlError::NoControllableDisplay)
        }
    }

    /// Reconstruit le maître s'il est inconnu (spec §4.5).
    fn ensure_master(&mut self, out: &mut Vec<Command>) {
        if self.master.is_none() {
            self.master = Some(self.reconstructed_master());
            out.push(Command::Persist(Persist::Master));
        }
    }

    fn reconstructed_master(&self) -> u8 {
        reconstruct_master(
            self.displays
                .values()
                .filter(|d| d.state == DisplayState::Controllable)
                .filter_map(|d| d.level.map(|l| (d.range, i64::from(l)))),
        )
    }

    // ---- Écritures ---------------------------------------------------------

    /// Applique un maître à tous les écrans contrôlables. Sans `force`, seuls
    /// les écrans dont le niveau change sont écrits.
    fn apply_master(&mut self, master: u8, force: bool, out: &mut Vec<Command>) {
        self.master = Some(master);
        out.push(Command::Persist(Persist::Master));
        let mut waiting = BTreeSet::new();
        let mut targeted = false;
        for (id, d) in &mut self.displays {
            if d.state != DisplayState::Controllable {
                continue;
            }
            targeted = true;
            let level = d.range.display_level(i64::from(master));
            if force || d.level != Some(level) {
                d.level = Some(level);
                waiting.insert(id.clone());
                out.push(Command::Write {
                    display: id.clone(),
                    level,
                });
            }
        }
        if waiting.is_empty() {
            // Rien à écrire : le maître n'est « appliqué » que s'il existe au
            // moins un écran déjà au bon niveau (jamais sans écran).
            self.applied_master = targeted.then_some(master);
            self.pending = None;
        } else {
            // Écritures en vol : rien n'est « appliqué » tant qu'elles ne sont pas confirmées.
            self.applied_master = None;
            self.pending = Some(Pending { master, waiting });
        }
    }

    fn idle_level_for(&self, range: Range) -> u8 {
        if self.cfg.idle.level_is_absolute {
            self.cfg.idle.level.min(100)
        } else {
            range.display_level(i64::from(self.cfg.idle.level))
        }
    }

    fn write_dim(&mut self, force: bool, out: &mut Vec<Command>) {
        let absolute = self.cfg.idle.level_is_absolute;
        let idle = i64::from(self.cfg.idle.level);
        for (id, d) in &mut self.displays {
            if d.state != DisplayState::Controllable {
                continue;
            }
            let level = if absolute {
                clamp_percent(idle)
            } else {
                d.range.display_level(idle)
            };
            if force || d.level != Some(level) {
                d.level = Some(level);
                out.push(Command::Write {
                    display: id.clone(),
                    level,
                });
            }
        }
    }

    fn on_write_result(
        &mut self,
        display: &DisplayId,
        level: u8,
        ok: bool,
        out: &mut Vec<Command>,
    ) {
        let Some(d) = self.displays.get_mut(display) else {
            return;
        };
        if ok {
            d.failures = 0;
            if let Some(pending) = &mut self.pending {
                pending.waiting.remove(display);
                if pending.waiting.is_empty() {
                    self.applied_master = Some(pending.master);
                    self.pending = None;
                }
            }
            return;
        }
        if d.level != Some(level) {
            return; // résultat périmé : une valeur plus récente a été commandée
        }
        // Un échec n'est jamais mémorisé comme appliqué.
        d.level = None;
        d.failures = d.failures.saturating_add(1);
        self.pending = None;
        self.applied_master = None;
        if d.failures >= FAILURES_BEFORE_UNAVAILABLE {
            d.state = DisplayState::Unavailable;
            d.failures = 0;
            out.push(Command::Notify(Notice::DisplayUnavailable(display.clone())));
            out.push(Command::Rescan {
                target: Some(display.clone()),
                delay_ms: 0,
            });
        }
    }

    // ---- Actions manuelles ----------------------------------------------------

    /// Effets d'une action manuelle selon le mode de base (hors écriture).
    fn manual_policy(&mut self, now: Now, master: u8, kind: ManualKind, out: &mut Vec<Command>) {
        self.ramp = None;
        self.dimmed = false;
        match self.base_mode() {
            BaseMode::AutoLux => {
                let lux = self.filter.latest_lux();
                match kind {
                    ManualKind::Normal => {
                        let policy = self.cfg.tuning.learn_policy();
                        match self.curve.learn(lux, i64::from(master), None, &policy) {
                            LearnOutcome::Learned => out.push(Command::Persist(Persist::Curve)),
                            LearnOutcome::Refused { contradicted } => {
                                out.push(Command::Notify(Notice::LearnRefused { contradicted }));
                            }
                            LearnOutcome::IgnoredTooClose => {}
                        }
                    }
                    ManualKind::Preset => {
                        let minutes = u64::from(self.cfg.tuning.preset_pause_minutes);
                        self.pause = Some(Pause {
                            until_ms: now.ms + minutes * 60_000,
                            lux,
                        });
                    }
                }
                self.gate.reset_to(master);
            }
            BaseMode::Planning => {
                self.suspension = Suspension::begin(&self.cfg.schedule, now.minute);
                out.push(Command::Persist(Persist::Suspension));
            }
            BaseMode::Static => {}
        }
    }

    fn set_display(
        &mut self,
        now: Now,
        display: DisplayId,
        level: i64,
        out: &mut Vec<Command>,
    ) -> Result<(), ControlError> {
        let level = percent("niveau écran", level)?;
        match self.displays.get(&display).map(|d| d.state) {
            None => return Err(ControlError::UnknownDisplay(display)),
            Some(DisplayState::Controllable) => {}
            Some(_) => return Err(ControlError::DisplayUnavailable(display)),
        }
        if let Some(d) = self.displays.get_mut(&display) {
            d.level = Some(level);
        }
        let master = self.reconstructed_master();
        self.manual_policy(now, master, ManualKind::Normal, out);
        self.master = Some(master);
        self.applied_master = None;
        self.pending = None;
        out.push(Command::Persist(Persist::Master));
        out.push(Command::Write { display, level });
        Ok(())
    }

    fn set_range(
        &mut self,
        display: DisplayId,
        low: i64,
        high: i64,
        out: &mut Vec<Command>,
    ) -> Result<(), ControlError> {
        if !self.displays.contains_key(&display) {
            return Err(ControlError::UnknownDisplay(display));
        }
        // Le maître inconnu est reconstruit avec l'ancienne plage.
        self.ensure_master(out);
        if let Some(d) = self.displays.get_mut(&display) {
            d.range = Range::normalized(low, high);
        }
        out.push(Command::Persist(Persist::Ranges));
        if !self.dimmed
            && let Some(m) = self.master
        {
            self.apply_master(m, true, out);
        }
        Ok(())
    }

    // ---- Fonctions et configuration ----------------------------------------------

    fn set_feature(&mut self, now: Now, feature: Feature, on: bool, out: &mut Vec<Command>) {
        match feature {
            Feature::Schedule => {
                self.cfg.schedule_enabled = on;
                self.suspension = None;
                out.push(Command::Persist(Persist::Suspension));
                if on && !self.dimmed && self.base_mode() == BaseMode::Planning {
                    if let Some(v) = self.cfg.schedule.value_at(now.minute) {
                        self.apply_master(v, true, out);
                    }
                }
            }
            Feature::IdleDim => {
                self.cfg.idle.enabled = on;
                if !on && self.dimmed {
                    self.restore(now, out);
                }
            }
            Feature::Auto => {
                self.cfg.auto_enabled = on;
                self.ramp = None;
                self.gate.reset();
                if !self.dimmed {
                    if on && self.auto_owner() {
                        self.apply_auto_forced(out);
                    } else if !on {
                        self.reapply(now, out);
                    }
                }
            }
        }
    }

    fn reload(&mut self, now: Now, cfg: ControllerConfig, out: &mut Vec<Command>) {
        if cfg.tuning != self.cfg.tuning {
            self.filter = Filter::new(cfg.tuning.filter_params());
            self.gate = Gate::new(cfg.tuning.gate_params());
            self.has_measure = false;
        }
        self.cfg = cfg;
        self.suspension = None;
        out.push(Command::Persist(Persist::Suspension));
        if !self.dimmed {
            self.reapply(now, out);
        } else {
            self.write_dim(true, out);
        }
    }

    // ---- Planning --------------------------------------------------------------

    fn planning_tick(&mut self, now: Now, out: &mut Vec<Command>) {
        if self.dimmed || self.auto_owner() || !self.planning_active() {
            return;
        }
        if let Some(s) = self.suspension {
            if !s.should_resume(now.minute) {
                return;
            }
            self.suspension = None;
            out.push(Command::Persist(Persist::Suspension));
        }
        let Some(value) = self.cfg.schedule.value_at(now.minute) else {
            return;
        };
        let in_flight = self.pending.as_ref().is_some_and(|p| p.master == value);
        if self.applied_master != Some(value) && !in_flight {
            self.apply_master(value, false, out);
        }
    }

    // ---- Auto-luminosité --------------------------------------------------------

    fn apply_auto_forced(&mut self, out: &mut Vec<Command>) {
        let target = self.curve.level_at(self.filter.smoothed_lux());
        self.gate.reset_to(target);
        self.ramp = None;
        self.apply_master(target, true, out);
    }

    fn start_ramp(&mut self, target: u8, out: &mut Vec<Command>) {
        match self.master {
            Some(m) if m == target => self.ramp = None,
            None => self.apply_master(target, true, out),
            Some(_) => {
                self.ramp = Some(target);
                if !self.ramp_armed {
                    self.ramp_armed = true;
                    out.push(Command::ArmRampTimer {
                        in_ms: u64::from(self.cfg.tuning.ramp_step_ms),
                    });
                }
            }
        }
    }

    fn ramp_tick(&mut self, out: &mut Vec<Command>) {
        self.ramp_armed = false;
        let Some(target) = self.ramp else {
            return;
        };
        if self.dimmed || !self.auto_owner() {
            self.ramp = None;
            return;
        }
        let current = self.master.unwrap_or(target);
        let next = match current.cmp(&target) {
            std::cmp::Ordering::Less => current + 1,
            std::cmp::Ordering::Greater => current - 1,
            std::cmp::Ordering::Equal => current,
        };
        self.apply_master(next, false, out);
        if next == target {
            self.ramp = None;
        } else {
            self.ramp_armed = true;
            out.push(Command::ArmRampTimer {
                in_ms: u64::from(self.cfg.tuning.ramp_step_ms),
            });
        }
    }

    fn on_lux(&mut self, now: Now, lux: f64, out: &mut Vec<Command>) {
        self.filter.push(lux);
        let first = !self.has_measure;
        self.has_measure = true;
        if !(self.cfg.auto_enabled && self.cfg.auto_configured)
            || self.health != HaHealth::Ok
            || self.dimmed
        {
            return;
        }
        if let Some(pause) = self.pause {
            let moved = (log_scale(self.filter.smoothed_lux()) - log_scale(pause.lux)).abs()
                >= PAUSE_BREAK_LOG_DELTA;
            if now.ms < pause.until_ms && !moved {
                return;
            }
            self.pause = None;
        }
        if first {
            self.apply_auto_forced(out);
        } else {
            let target = self.curve.level_at(self.filter.smoothed_lux());
            if self.gate.decide(target) {
                self.start_ramp(target, out);
            }
        }
        if self.filter.is_pending() {
            out.push(Command::PollLuxSoon { in_ms: 3_000 });
        }
    }

    fn on_health(&mut self, now: Now, health: HaHealth, out: &mut Vec<Command>) {
        let previous = std::mem::replace(&mut self.health, health);
        if !(self.cfg.auto_enabled && self.cfg.auto_configured) || previous == health {
            return;
        }
        match health {
            HaHealth::Ok => {
                self.gate.reset();
                if self.auto_owner() && !self.dimmed {
                    let target = self.curve.level_at(self.filter.smoothed_lux());
                    if self.gate.decide(target) {
                        self.start_ramp(target, out);
                    }
                }
            }
            HaHealth::Offline | HaHealth::Degraded => {
                self.ramp = None;
                if !self.dimmed && previous == HaHealth::Ok {
                    // Le planning (ou le dernier maître) reprend, valeur forcée.
                    self.reapply(now, out);
                }
            }
        }
    }

    fn on_auto_configured(&mut self, now: Now, configured: bool, out: &mut Vec<Command>) {
        if self.cfg.auto_configured == configured {
            return;
        }
        let was_owner = self.auto_owner();
        self.cfg.auto_configured = configured;
        if !configured {
            // Plus de lien possible : on repart de zéro, le planning ou le dernier maître reprend.
            self.has_measure = false;
            self.filter.reset();
            self.gate.reset();
            self.pause = None;
            if was_owner && !self.dimmed {
                self.reapply(now, out);
            }
        }
    }

    // ---- Inactivité -------------------------------------------------------------

    fn on_idle_probe(&mut self, now: Now, idle_ms: u64, blocked: bool, out: &mut Vec<Command>) {
        if !self.cfg.idle.enabled {
            return;
        }
        let threshold = u64::from(self.cfg.idle.minutes) * 60_000;
        if self.dimmed {
            if idle_ms < threshold || blocked {
                self.restore(now, out);
            }
        } else if idle_ms >= threshold && !blocked {
            self.dim(out);
        }
    }

    fn dim(&mut self, out: &mut Vec<Command>) {
        self.ramp = None;
        self.ensure_master(out);
        self.dimmed = true;
        self.applied_master = None;
        self.pending = None;
        self.write_dim(true, out);
    }

    fn restore(&mut self, now: Now, out: &mut Vec<Command>) {
        self.dimmed = false;
        self.reapply(now, out);
    }

    // ---- Ré-application par priorité ---------------------------------------------

    /// Ré-application par priorité : atténué > auto (forcé) > planning
    /// (forcé, sauf suspension non échue) > dernier maître.
    fn reapply(&mut self, now: Now, out: &mut Vec<Command>) {
        self.ramp = None;
        if self.dimmed {
            self.write_dim(true, out);
            return;
        }
        if let Some(s) = self.suspension
            && s.should_resume(now.minute)
        {
            self.suspension = None;
            out.push(Command::Persist(Persist::Suspension));
        }
        if self.auto_owner() {
            self.apply_auto_forced(out);
        } else if self.planning_active() && self.suspension.is_none() {
            match self.cfg.schedule.value_at(now.minute) {
                Some(v) => self.apply_master(v, true, out),
                None => self.write_master_if_known(out),
            }
        } else {
            self.write_master_if_known(out);
        }
    }

    fn write_master_if_known(&mut self, out: &mut Vec<Command>) {
        if let Some(m) = self.master {
            self.apply_master(m, true, out);
        }
    }

    // ---- Liste d'écrans -------------------------------------------------------------

    fn on_list(&mut self, now: Now, list: Vec<Listed>, out: &mut Vec<Command>) {
        let had_controllable = self
            .displays
            .values()
            .any(|d| d.state == DisplayState::Controllable);
        let has_controllable = list.iter().any(|l| l.controllable);
        if had_controllable && !has_controllable {
            if let Some(&delay) = FAKE_LIST_RETRY_MS.get(self.fake_attempts) {
                self.fake_attempts += 1;
                out.push(Command::Notify(Notice::ListRejected { retry_in_ms: delay }));
                out.push(Command::Rescan {
                    target: None,
                    delay_ms: delay,
                });
                return; // liste factice : on garde l'ancienne
            }
        }
        self.fake_attempts = 0;

        let listed: BTreeSet<&DisplayId> = list.iter().map(|l| &l.id).collect();
        for (id, d) in &mut self.displays {
            if !listed.contains(id) {
                d.state = DisplayState::Absent;
                d.level = None;
            }
        }
        let mut returning = Vec::new();
        for l in &list {
            let known = self.displays.contains_key(&l.id);
            let d = self.displays.entry(l.id.clone()).or_insert(Disp {
                range: Range::FULL,
                state: DisplayState::Absent,
                level: None,
                failures: 0,
            });
            let was = d.state;
            d.failures = 0;
            d.level = if l.controllable { l.level } else { None };
            d.state = if l.controllable {
                DisplayState::Controllable
            } else if matches!(was, DisplayState::Controllable | DisplayState::Unavailable) || known
            {
                DisplayState::Unavailable
            } else {
                DisplayState::NotControllable
            };
            if l.controllable && was != DisplayState::Controllable {
                returning.push(l.id.clone());
            }
        }

        if self.master.is_none() && has_controllable {
            // Démarrage sans état : on reconstruit le maître sans rien écrire.
            self.master = Some(self.reconstructed_master());
            out.push(Command::Persist(Persist::Master));
            self.reapply_on_list = false;
            return;
        }
        if self.reapply_on_list {
            self.reapply_on_list = false;
            self.reapply(now, out);
            out.push(Command::ArmReapply {
                in_ms: SECOND_REAPPLY_MS,
            });
        } else if !returning.is_empty() {
            self.write_to(&returning, out);
        } else if self.drift_until_ms.is_some_and(|until| now.ms < until) {
            self.correct_drift(&list, out);
        }
        // Tant que la fenêtre qui suit un réveil est ouverte, on revérifie : la réinitialisation
        // tardive d'un écran n'est pas toujours accompagnée d'un événement de branchement.
        if self
            .drift_until_ms
            .is_some_and(|until| now.ms + WAKE_DRIFT_RECHECK_MS < until)
        {
            out.push(Command::Rescan {
                target: None,
                delay_ms: WAKE_DRIFT_RECHECK_MS,
            });
        }
    }

    /// Réécrit les écrans dont le niveau lu s'écarte de celui attendu (réinitialisation tardive
    /// après un réveil), puis programme une vérification. Ne sert qu'après un réveil : hors de
    /// cette fenêtre, un réglage fait avec les boutons du moniteur n'est jamais écrasé.
    fn correct_drift(&mut self, list: &[Listed], out: &mut Vec<Command>) {
        let mut drifted = Vec::new();
        for l in list.iter().filter(|l| l.controllable) {
            let Some(range) = self.displays.get(&l.id).map(|d| d.range) else {
                continue;
            };
            let expected = if self.dimmed {
                Some(self.idle_level_for(range))
            } else {
                self.master.map(|m| range.display_level(i64::from(m)))
            };
            if expected.is_some() && l.level.is_some() && l.level != expected {
                drifted.push(l.id.clone());
            }
        }
        if !drifted.is_empty() {
            self.write_to(&drifted, out);
        }
    }

    /// Envoie le niveau courant (inactivité ou maître) aux seuls écrans donnés.
    fn write_to(&mut self, ids: &[DisplayId], out: &mut Vec<Command>) {
        for id in ids {
            let Some(range) = self.displays.get(id).map(|d| d.range) else {
                continue;
            };
            let level = if self.dimmed {
                Some(self.idle_level_for(range))
            } else {
                self.master.map(|m| range.display_level(i64::from(m)))
            };
            if let (Some(level), Some(d)) = (level, self.displays.get_mut(id)) {
                d.level = Some(level);
                out.push(Command::Write {
                    display: id.clone(),
                    level,
                });
            }
        }
    }
}

fn percent(what: &'static str, value: i64) -> Result<u8, ControlError> {
    if (0..=100).contains(&value) {
        Ok(clamp_percent(value))
    } else {
        Err(ControlError::OutOfRange { what, value })
    }
}
