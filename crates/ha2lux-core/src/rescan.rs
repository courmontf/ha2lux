//! Gestionnaire de rescan (sans E/S) : quand lancer une énumération des écrans.
//!
//! Mécanismes de robustesse :
//! - **débounce** de 600 ms (une rafale d'événements = un seul scan) ;
//! - **plancher de 30 s** entre deux scans déclenchés par un hotplug *bruyant*
//!   (un écran dont le lien se ré-entraîne en boucle) ; veille, déverrouillage et
//!   demande manuelle ne sont jamais bridés ;
//! - **un seul scan à la fois**, avec un numéro de **génération** : le résultat
//!   tardif d'un scan désavoué est jeté ;
//! - **chien de garde** de 10 s : un scan bloqué est désavoué ; après 3 désaveux
//!   consécutifs on attend un nouvel événement ;
//! - écrans **en attente** : relances à 2, 5, 10, 20 s puis toutes les 60 s sans
//!   fin, reportées tant que l'interface interactive est ouverte ; un écran qui n'a
//!   jamais répondu n'est pas relancé (le gestionnaire n'est prévenu que s'il y en a).
//!
//! Le temps est celui de l'appelant (millisecondes monotones) : le gestionnaire ne
//! dort jamais, il publie sa prochaine échéance ([`RescanManager::next_deadline`]).

/// Réglages (valeurs de départ de la spec, à recalibrer sur du matériel).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RescanConfig {
    /// Débounce d'une rafale d'événements.
    pub debounce_ms: u64,
    /// Plafond de report d'un scan par des événements qui se répètent.
    pub max_defer_ms: u64,
    /// Plancher entre deux scans de hotplug bruyant.
    pub noisy_floor_ms: u64,
    /// Durée au-delà de laquelle un scan est désavoué.
    pub watchdog_ms: u64,
    /// Désaveux consécutifs avant d'attendre un nouvel événement.
    pub max_disavowals: u32,
    /// Délai avant de relancer après un désaveu.
    pub disavow_retry_ms: u64,
    /// Relances des écrans en attente ; le dernier palier se répète indéfiniment.
    pub waiting_ladder_ms: Vec<u64>,
}

impl Default for RescanConfig {
    fn default() -> Self {
        Self {
            debounce_ms: 600,
            max_defer_ms: 3_000,
            noisy_floor_ms: 30_000,
            watchdog_ms: 10_000,
            max_disavowals: 3,
            disavow_retry_ms: 2_000,
            waiting_ladder_ms: vec![2_000, 5_000, 10_000, 20_000, 60_000],
        }
    }
}

/// Origine d'une demande de scan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    /// Démarrage du démon : immédiat.
    Startup,
    /// Demande de l'utilisateur : immédiate, jamais bridée.
    Manual,
    /// Réveil de veille, déverrouillage, écran rallumé, ou demande de la machine
    /// d'états : débouncé, jamais bridé par le plancher.
    Resume,
    /// Branchement/débranchement d'écran : débouncé, bridé s'il est bruyant.
    Hotplug,
}

/// Action demandée à l'appelant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Lancer une énumération portant ce numéro de génération.
    Start {
        /// Numéro de génération à rendre dans [`RescanManager::finished`].
        generation: u64,
    },
}

/// Résultat d'un scan terminé.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScanOutcome {
    /// La liste a été acceptée (pas rejetée comme factice).
    pub accepted: bool,
    /// Au moins un écran déjà connu est indisponible : il faut le relancer.
    pub waiting: bool,
}

#[derive(Clone, Copy, Debug)]
struct Pending {
    first_ms: u64,
    due_ms: u64,
    hotplug_only: bool,
}

#[derive(Clone, Copy, Debug)]
struct Running {
    generation: u64,
    started_ms: u64,
}

/// Gestionnaire de rescan.
#[derive(Clone, Debug)]
pub struct RescanManager {
    config: RescanConfig,
    pending: Option<Pending>,
    running: Option<Running>,
    generation: u64,
    last_hotplug_scan_ms: Option<u64>,
    ladder_step: usize,
    retry_due_ms: Option<u64>,
    disavowals: u32,
    halted: bool,
    ui_open: bool,
    suspended: bool,
}

impl RescanManager {
    /// Gestionnaire neuf.
    pub fn new(config: RescanConfig) -> Self {
        Self {
            config,
            pending: None,
            running: None,
            generation: 0,
            last_hotplug_scan_ms: None,
            ladder_step: 0,
            retry_due_ms: None,
            disavowals: 0,
            halted: false,
            ui_open: false,
            suspended: false,
        }
    }

    /// Enregistre une demande de scan. Appeler ensuite [`RescanManager::poll`].
    pub fn trigger(&mut self, now_ms: u64, trigger: Trigger, extra_delay_ms: u64) {
        self.halted = false;
        self.disavowals = 0;
        let (due, hotplug) = match trigger {
            Trigger::Startup => (now_ms, false),
            Trigger::Manual => {
                self.ladder_step = 0;
                (now_ms, false)
            }
            Trigger::Resume => (now_ms + self.config.debounce_ms.max(extra_delay_ms), false),
            Trigger::Hotplug => (now_ms + self.config.debounce_ms, true),
        };
        let immediate = matches!(trigger, Trigger::Startup | Trigger::Manual);
        self.pending = Some(match self.pending {
            None => Pending {
                first_ms: now_ms,
                due_ms: due,
                hotplug_only: hotplug,
            },
            Some(p) => {
                let cap = p.first_ms + self.config.max_defer_ms;
                // Débounce « de queue » (chaque événement repousse l'échéance), plafonné pour
                // qu'une rafale continue ne retarde pas le scan indéfiniment.
                let merged = if immediate {
                    due.min(p.due_ms)
                } else {
                    p.due_ms.max(due.min(cap))
                };
                Pending {
                    first_ms: p.first_ms,
                    due_ms: merged,
                    hotplug_only: p.hotplug_only && hotplug,
                }
            }
        });
    }

    /// Suspend (veille, session inactive) ou reprend le lancement des scans. Les
    /// demandes reçues pendant la suspension sont conservées.
    pub fn set_suspended(&mut self, suspended: bool) {
        self.suspended = suspended;
    }

    /// L'interface interactive (popup) est ouverte : les relances périodiques sont
    /// reportées (une re-création la fermerait).
    pub fn set_ui_open(&mut self, open: bool) {
        self.ui_open = open;
    }

    fn effective_due(&self, pending: &Pending) -> u64 {
        let floor = match (pending.hotplug_only, self.last_hotplug_scan_ms) {
            (true, Some(last)) => last + self.config.noisy_floor_ms,
            _ => 0,
        };
        pending.due_ms.max(floor)
    }

    /// Évalue les échéances à l'instant `now_ms` et renvoie les scans à lancer.
    pub fn poll(&mut self, now_ms: u64) -> Vec<Action> {
        if let Some(running) = self.running
            && now_ms >= running.started_ms + self.config.watchdog_ms
        {
            // Scan bloqué : désavoué, son résultat tardif sera jeté.
            self.running = None;
            self.disavowals += 1;
            if self.disavowals >= self.config.max_disavowals {
                self.halted = true;
            } else {
                self.pending = Some(Pending {
                    first_ms: now_ms,
                    due_ms: now_ms + self.config.disavow_retry_ms,
                    hotplug_only: false,
                });
            }
        }
        if self.running.is_none()
            && !self.halted
            && !self.suspended
            && !self.ui_open
            && self.retry_due_ms.is_some_and(|due| now_ms >= due)
        {
            self.retry_due_ms = None;
            self.pending = self.pending.or(Some(Pending {
                first_ms: now_ms,
                due_ms: now_ms,
                hotplug_only: false,
            }));
        }
        if self.running.is_some() || self.halted || self.suspended {
            return Vec::new();
        }
        let Some(pending) = self.pending else {
            return Vec::new();
        };
        if now_ms < self.effective_due(&pending) {
            return Vec::new();
        }
        self.pending = None;
        self.generation += 1;
        self.running = Some(Running {
            generation: self.generation,
            started_ms: now_ms,
        });
        if pending.hotplug_only {
            self.last_hotplug_scan_ms = Some(now_ms);
        }
        vec![Action::Start {
            generation: self.generation,
        }]
    }

    /// Signale la fin d'un scan. Renvoie `false` si son résultat doit être **jeté**
    /// (scan désavoué ou dépassé) ; sinon met à jour les relances.
    pub fn finished(&mut self, now_ms: u64, generation: u64, outcome: ScanOutcome) -> bool {
        match self.running {
            Some(running) if running.generation == generation => self.running = None,
            _ => return false,
        }
        self.disavowals = 0;
        if outcome.accepted {
            if outcome.waiting {
                let ladder = &self.config.waiting_ladder_ms;
                let step = self.ladder_step.min(ladder.len().saturating_sub(1));
                let delay = ladder.get(step).copied().unwrap_or(60_000);
                self.ladder_step += 1;
                self.retry_due_ms = Some(now_ms + delay);
            } else {
                self.ladder_step = 0;
                self.retry_due_ms = None;
            }
        }
        true
    }

    /// Prochaine date à laquelle appeler [`RescanManager::poll`], s'il y en a une.
    pub fn next_deadline(&self) -> Option<u64> {
        let mut deadlines = Vec::new();
        if let Some(running) = self.running {
            deadlines.push(running.started_ms + self.config.watchdog_ms);
        } else if !self.halted && !self.suspended {
            if let Some(pending) = self.pending {
                deadlines.push(self.effective_due(&pending));
            }
            if let (Some(due), false) = (self.retry_due_ms, self.ui_open) {
                deadlines.push(due);
            }
        }
        deadlines.into_iter().min()
    }

    /// Ce numéro de génération est celui du scan en cours (sinon son résultat est périmé).
    pub fn is_current(&self, generation: u64) -> bool {
        self.running.is_some_and(|r| r.generation == generation)
    }

    /// Un scan est en cours.
    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }

    /// Les scans automatiques sont arrêtés (3 désaveux) jusqu'au prochain événement.
    pub fn is_halted(&self) -> bool {
        self.halted
    }

    /// Une demande est en attente de son échéance.
    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }
}
