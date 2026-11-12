//! Modèle de configuration, lecture tolérante et sérialisation TOML (spec §10).
//!
//! Partie pure : aucune E/S. La lecture ne perd jamais la configuration : une
//! clé fautive reprend sa valeur par défaut (ou est bornée) et un
//! [`ConfigIssue`] est consigné ; seules une erreur de syntaxe TOML ou une
//! version de schéma trop récente sont fatales.
//!
//! Les valeurs des clés ne sont jamais recopiées dans les messages : un jeton
//! mal placé dans le fichier ne peut donc pas fuiter dans les journaux.

use serde::{Serialize, Serializer};
use thiserror::Error;
use toml::{Table, Value};

use crate::curve::LearnPolicy;
use crate::filter::FilterParams;
use crate::gate::GateParams;
use crate::range::Range;
use crate::schedule::{MAX_POINTS, MinuteOfDay, Point, Schedule, ScheduleError};
use crate::url::{UrlNote, check_url};

/// Version courante du schéma de configuration.
pub const CONFIG_VERSION: u32 = 1;
/// Nombre maximal de presets.
pub const MAX_PRESETS: usize = 10;
/// Nombre maximal d'écrans mémorisés.
pub const MAX_DISPLAYS: usize = 32;
/// Longueur maximale d'un nom de preset.
pub const MAX_PRESET_NAME: usize = 63;
/// Longueur maximale d'un identifiant d'entité.
pub const MAX_ENTITY_LEN: usize = 127;

/// Erreurs fatales de lecture ou d'écriture de configuration.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ConfigError {
    /// Syntaxe TOML invalide.
    #[error("unreadable configuration (line {line}): {message}")]
    Syntax {
        /// Ligne approximative de l'erreur (1 si inconnue).
        line: usize,
        /// Message de l'analyseur, sans extrait du fichier.
        message: String,
    },
    /// Schéma plus récent que ce binaire.
    #[error("version de configuration {found} non prise en charge (maximum {supported})")]
    UnsupportedVersion {
        /// Version trouvée dans le fichier.
        found: i64,
        /// Version maximale connue.
        supported: u32,
    },
    /// Échec de sérialisation.
    #[error("serialization failed: {0}")]
    Serialize(String),
}

/// Gravité d'une remarque de lecture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    /// Information (valeur corrigée sans perte).
    Info,
    /// Avertissement (valeur remplacée ou ignorée).
    Warning,
}

/// Remarque produite pendant la lecture (jamais de valeur sensible).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigIssue {
    /// Gravité.
    pub severity: Severity,
    /// Chemin de la clé (ex. `idle.minutes`).
    pub path: String,
    /// Description lisible.
    pub message: String,
}

/// Résultat d'une lecture : configuration et remarques.
#[derive(Clone, Debug, PartialEq)]
pub struct Loaded {
    /// Configuration validée.
    pub config: Config,
    /// Corrections appliquées.
    pub issues: Vec<ConfigIssue>,
}

/// Réglages généraux.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct General {
    /// Pas de la molette/des raccourcis (1-50).
    pub step: u8,
    /// Associer les écrans à leurs bus DDC en lisant l'EDID sur les adaptateurs
    /// GPU (pilote NVIDIA propriétaire, sans lien `ddc` dans sysfs). Désactivé
    /// par défaut : cela effectue de vraies lectures I²C (décision 48).
    pub edid_bus_matching: bool,
}

/// Preset nommé.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preset {
    /// Nom (1-63 caractères).
    pub name: String,
    /// Niveau maître 0-100.
    pub level: u8,
}

/// Configuration mémorisée d'un écran.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisplayEntry {
    /// Identifiant stable (clé EDID).
    pub id: String,
    /// Plage de l'écran.
    pub range: Range,
    /// Plancher backlight, en valeur brute.
    pub floor: u32,
}

/// Planning.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ScheduleConfig {
    /// Planning activé.
    pub enabled: bool,
    /// Points validés (triés, sans doublon, ≤ 24).
    #[serde(serialize_with = "ser_points")]
    pub points: Vec<Point>,
}

/// Atténuation à l'inactivité.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct IdleConfig {
    /// Fonction activée.
    pub enabled: bool,
    /// Niveau d'atténuation 0-100.
    pub level: u8,
    /// Délai en minutes (1-1440).
    pub minutes: u16,
    /// Niveau absolu (non remappé par la plage).
    pub level_is_absolute: bool,
}

/// Réglages fins de l'auto-luminosité (`auto.tuning.*`).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Tuning {
    /// Filtre : écart log d'un grand changement.
    pub big_log_delta: f64,
    /// Filtre : écart en lux d'un grand changement.
    pub big_lux_delta: f64,
    /// Filtre : coefficient de lissage.
    pub alpha: f64,
    /// Filtre : proximité de confirmation.
    pub confirm_close: f64,
    /// Porte : écart d'application immédiate.
    pub gate_immediate: u8,
    /// Porte : écart minimal.
    pub gate_minimum: u8,
    /// Porte : confirmations requises.
    pub gate_confirmations: u8,
    /// Rampe : durée d'un pas en ms (jamais plus rapide que le bus).
    pub ramp_step_ms: u32,
    /// Apprentissage : plancher d'écart (0 = désactivé).
    pub learn_min_delta: u8,
    /// Apprentissage : contradictions tolérées avant refus.
    pub learn_max_contradicted: u8,
    /// Apprentissage : ancienneté maximale d'un point en jours (0 = illimitée).
    pub learn_max_age_days: u32,
    /// Durée de pause de l'auto après un preset, en minutes.
    pub preset_pause_minutes: u32,
}

impl Default for Tuning {
    fn default() -> Self {
        let filter = FilterParams::default();
        let gate = GateParams::default();
        Self {
            big_log_delta: filter.big_log_delta,
            big_lux_delta: filter.big_lux_delta,
            alpha: filter.alpha,
            confirm_close: filter.confirm_close,
            gate_immediate: gate.immediate,
            gate_minimum: gate.minimum,
            gate_confirmations: gate.confirmations,
            ramp_step_ms: 100,
            learn_min_delta: 2,
            learn_max_contradicted: 2,
            learn_max_age_days: 0,
            preset_pause_minutes: 30,
        }
    }
}

impl Tuning {
    /// Réglages du filtre.
    pub fn filter_params(&self) -> FilterParams {
        FilterParams {
            big_log_delta: self.big_log_delta,
            big_lux_delta: self.big_lux_delta,
            alpha: self.alpha,
            confirm_close: self.confirm_close,
        }
    }

    /// Réglages de la porte.
    pub fn gate_params(&self) -> GateParams {
        GateParams {
            immediate: self.gate_immediate,
            minimum: self.gate_minimum,
            confirmations: self.gate_confirmations,
        }
    }

    /// Garde-fous d'apprentissage.
    pub fn learn_policy(&self) -> LearnPolicy {
        LearnPolicy {
            min_delta: self.learn_min_delta,
            max_contradicted: Some(usize::from(self.learn_max_contradicted)),
        }
    }
}

/// Auto-luminosité Home Assistant. Le jeton n'apparaît jamais ici.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AutoConfig {
    /// Fonction activée (exige URL et entité, plus le jeton côté démon).
    pub enabled: bool,
    /// URL normalisée de Home Assistant.
    pub url: String,
    /// Identifiant de l'entité (`sensor.xxx`).
    pub entity: String,
    /// Libellé d'affichage.
    pub label: String,
    /// Période du repli REST en secondes (5-300).
    pub poll_seconds: u32,
    /// Péremption d'une mesure en secondes.
    pub stale_seconds: u32,
    /// Délai sans mesure valide avant l'état « dégradé », en secondes.
    pub fallback_seconds: u32,
    /// Réglages fins.
    pub tuning: Tuning,
}

/// Configuration complète.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Config {
    /// Version du schéma.
    pub version: u32,
    /// Réglages généraux.
    pub general: General,
    /// Presets (≤ 10), dans l'ordre du fichier.
    #[serde(serialize_with = "ser_presets")]
    pub presets: Vec<Preset>,
    /// Écrans mémorisés (≤ 32, même débranchés).
    #[serde(serialize_with = "ser_displays")]
    pub displays: Vec<DisplayEntry>,
    /// Planning.
    pub schedule: ScheduleConfig,
    /// Atténuation à l'inactivité.
    pub idle: IdleConfig,
    /// Auto-luminosité.
    pub auto: AutoConfig,
}

#[derive(Serialize)]
struct PointDto {
    time: String,
    level: u8,
}

fn ser_points<S: Serializer>(points: &[Point], s: S) -> Result<S::Ok, S::Error> {
    s.collect_seq(points.iter().map(|p| PointDto {
        time: p.time.to_string(),
        level: p.level,
    }))
}

fn ser_presets<S: Serializer>(presets: &[Preset], s: S) -> Result<S::Ok, S::Error> {
    s.collect_map(presets.iter().map(|p| (&p.name, p.level)))
}

#[derive(Serialize)]
struct DisplayDto {
    range: [u8; 2],
    floor: u32,
}

fn ser_displays<S: Serializer>(displays: &[DisplayEntry], s: S) -> Result<S::Ok, S::Error> {
    s.collect_map(displays.iter().map(|d| {
        (
            &d.id,
            DisplayDto {
                range: [d.range.low(), d.range.high()],
                floor: d.floor,
            },
        )
    }))
}

impl Default for Config {
    fn default() -> Self {
        let preset = |name: &str, level| Preset {
            name: name.to_owned(),
            level,
        };
        Self {
            version: CONFIG_VERSION,
            general: General {
                step: 5,
                edid_bus_matching: false,
            },
            presets: vec![
                preset("Night", 30),
                preset("Day", 80),
                preset("Presentation", 100),
            ],
            displays: Vec::new(),
            schedule: ScheduleConfig {
                enabled: false,
                points: Vec::new(),
            },
            idle: IdleConfig {
                enabled: false,
                level: 5,
                minutes: 5,
                level_is_absolute: false,
            },
            auto: AutoConfig {
                enabled: false,
                url: String::new(),
                entity: String::new(),
                label: String::new(),
                poll_seconds: 15,
                stale_seconds: 600,
                fallback_seconds: 900,
                tuning: Tuning::default(),
            },
        }
    }
}

/// Vrai pour un identifiant d'entité Home Assistant `domaine.objet`
/// (minuscules, chiffres, `_`), de 127 caractères au plus. Écarte `/`, `..`, `?`.
pub fn is_valid_entity_id(id: &str) -> bool {
    let ok = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    };
    id.len() <= MAX_ENTITY_LEN && id.split_once('.').is_some_and(|(d, o)| ok(d) && ok(o))
}

struct Reader {
    issues: Vec<ConfigIssue>,
}

impl Reader {
    fn note(&mut self, severity: Severity, path: &str, message: impl Into<String>) {
        self.issues.push(ConfigIssue {
            severity,
            path: path.to_owned(),
            message: message.into(),
        });
    }

    fn int(&mut self, path: &str, value: Option<&Value>, default: i64, min: i64, max: i64) -> i64 {
        match value {
            None => default,
            Some(Value::Integer(i)) if (min..=max).contains(i) => *i,
            Some(Value::Integer(i)) => {
                let clamped = (*i).clamp(min, max);
                self.note(
                    Severity::Warning,
                    path,
                    format!("valeur hors bornes, ramenée dans {min}..={max}"),
                );
                clamped
            }
            Some(_) => {
                self.note(
                    Severity::Warning,
                    path,
                    "integer expected, default value used",
                );
                default
            }
        }
    }

    fn float(
        &mut self,
        path: &str,
        value: Option<&Value>,
        default: f64,
        min: f64,
        max: f64,
    ) -> f64 {
        let number = match value {
            None => return default,
            Some(Value::Float(f)) if f.is_finite() => *f,
            Some(Value::Integer(i)) => *i as f64,
            Some(_) => {
                self.note(
                    Severity::Warning,
                    path,
                    "number expected, default value used",
                );
                return default;
            }
        };
        if (min..=max).contains(&number) {
            number
        } else {
            self.note(
                Severity::Warning,
                path,
                format!("valeur hors bornes, ramenée dans {min}..={max}"),
            );
            number.clamp(min, max)
        }
    }

    fn boolean(&mut self, path: &str, value: Option<&Value>, default: bool) -> bool {
        match value {
            None => default,
            Some(Value::Boolean(b)) => *b,
            Some(_) => {
                self.note(
                    Severity::Warning,
                    path,
                    "boolean expected, default value used",
                );
                default
            }
        }
    }

    fn string(&mut self, path: &str, value: Option<&Value>, max_chars: usize) -> String {
        match value {
            None => String::new(),
            Some(Value::String(s)) if s.chars().count() <= max_chars => s.clone(),
            Some(Value::String(_)) => {
                self.note(
                    Severity::Warning,
                    path,
                    format!("texte trop long (maximum {max_chars}), ignoré"),
                );
                String::new()
            }
            Some(_) => {
                self.note(Severity::Warning, path, "text expected, default value used");
                String::new()
            }
        }
    }

    /// Sous-table `key` de `parent`, ou table vide (avec remarque si le type est faux).
    fn table<'a>(
        &mut self,
        path: &str,
        parent: &'a Table,
        key: &str,
        empty: &'a Table,
    ) -> &'a Table {
        match parent.get(key) {
            None => empty,
            Some(Value::Table(t)) => t,
            Some(_) => {
                self.note(Severity::Warning, path, "table expected, section ignored");
                empty
            }
        }
    }

    fn unknown_keys(&mut self, path: &str, table: &Table, known: &[&str]) {
        for key in table.keys().filter(|k| !known.contains(&k.as_str())) {
            let full = if path.is_empty() {
                key.clone()
            } else {
                format!("{path}.{key}")
            };
            self.note(Severity::Info, &full, "unknown key, ignored");
        }
    }
}

fn read_presets(r: &mut Reader, value: &Value) -> Vec<Preset> {
    let Value::Table(table) = value else {
        r.note(
            Severity::Warning,
            "presets",
            "table expected, default presets used",
        );
        return Config::default().presets;
    };
    let mut presets: Vec<Preset> = Vec::new();
    for (name, level) in table {
        let path = format!("presets.{name}");
        if presets.len() >= MAX_PRESETS {
            r.note(
                Severity::Warning,
                &path,
                format!("plus de {MAX_PRESETS} presets, entrée ignorée"),
            );
            continue;
        }
        let length = name.chars().count();
        if length == 0 || length > MAX_PRESET_NAME || name.chars().any(char::is_control) {
            r.note(
                Severity::Warning,
                "presets",
                format!("invalid preset name (1 to {MAX_PRESET_NAME} characters), entry rejected"),
            );
            continue;
        }
        if presets.iter().any(|p| {
            p.name.eq_ignore_ascii_case(name) || p.name.to_lowercase() == name.to_lowercase()
        }) {
            r.note(
                Severity::Warning,
                &path,
                "name already used (case-insensitive), entry ignored",
            );
            continue;
        }
        let Value::Integer(_) = level else {
            r.note(
                Severity::Warning,
                &path,
                "integer level expected, entry rejected",
            );
            continue;
        };
        let level = r.int(&path, Some(level), 0, 0, 100);
        presets.push(Preset {
            name: name.clone(),
            level: level as u8,
        });
    }
    presets
}

fn read_range(r: &mut Reader, path: &str, value: Option<&Value>) -> Range {
    match value {
        None => Range::FULL,
        Some(Value::Array(a)) => match a.as_slice() {
            [Value::Integer(low), Value::Integer(high)] => {
                let range = Range::normalized(*low, *high);
                if i64::from(range.low()) != *low || i64::from(range.high()) != *high {
                    r.note(Severity::Info, path, "range normalized (minimum width 20)");
                }
                range
            }
            _ => {
                r.note(
                    Severity::Warning,
                    path,
                    "range [low, high] of integers expected, (0, 100) used",
                );
                Range::FULL
            }
        },
        Some(_) => {
            r.note(
                Severity::Warning,
                path,
                "range [low, high] expected, (0, 100) used",
            );
            Range::FULL
        }
    }
}

fn read_displays(r: &mut Reader, value: &Value) -> Vec<DisplayEntry> {
    let Value::Table(table) = value else {
        r.note(
            Severity::Warning,
            "displays",
            "table expected, section ignored",
        );
        return Vec::new();
    };
    let mut displays = Vec::new();
    for (id, entry) in table {
        let path = format!("displays.{id}");
        if displays.len() >= MAX_DISPLAYS {
            r.note(
                Severity::Warning,
                &path,
                format!("plus de {MAX_DISPLAYS} écrans mémorisés, entrée ignorée"),
            );
            continue;
        }
        let Value::Table(entry) = entry else {
            r.note(Severity::Warning, &path, "table expected, entry ignored");
            continue;
        };
        if id.is_empty() {
            r.note(
                Severity::Warning,
                "displays",
                "empty display identifier, entry ignored",
            );
            continue;
        }
        r.unknown_keys(&path, entry, &["range", "floor"]);
        let range = read_range(r, &format!("{path}.range"), entry.get("range"));
        let floor = r.int(
            &format!("{path}.floor"),
            entry.get("floor"),
            1,
            0,
            i64::from(u32::MAX),
        ) as u32;
        displays.push(DisplayEntry {
            id: id.clone(),
            range,
            floor,
        });
    }
    displays
}

fn read_points(r: &mut Reader, value: Option<&Value>) -> Vec<Point> {
    let items = match value {
        None => return Vec::new(),
        Some(Value::Array(items)) => items,
        Some(_) => {
            r.note(
                Severity::Warning,
                "schedule.points",
                "list of points expected, schedule emptied",
            );
            return Vec::new();
        }
    };
    let mut points: Vec<Point> = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let path = format!("schedule.points[{i}]");
        if points.len() >= MAX_POINTS {
            r.note(
                Severity::Warning,
                &path,
                format!("plus de {MAX_POINTS} points, point ignoré"),
            );
            continue;
        }
        let Value::Table(item) = item else {
            r.note(
                Severity::Warning,
                &path,
                "point {time, level} expected, ignored",
            );
            continue;
        };
        let time = match item.get("time") {
            Some(Value::String(s)) => MinuteOfDay::parse(s),
            _ => Err(ScheduleError::InvalidTime(String::new())),
        };
        let Ok(time) = time else {
            r.note(
                Severity::Warning,
                &path,
                "invalid HH:MM time, point ignored",
            );
            continue;
        };
        if !matches!(item.get("level"), Some(Value::Integer(_))) {
            r.note(
                Severity::Warning,
                &path,
                "integer level expected, point ignored",
            );
            continue;
        }
        if points.iter().any(|p| p.time == time) {
            r.note(
                Severity::Warning,
                &path,
                "time already used by another point, point ignored",
            );
            continue;
        }
        let level = r.int(&format!("{path}.level"), item.get("level"), 0, 0, 100) as u8;
        points.push(Point { time, level });
    }
    points.sort_by_key(|p| p.time);
    points
}

fn read_tuning(r: &mut Reader, table: &Table) -> Tuning {
    let d = Tuning::default();
    r.unknown_keys(
        "auto.tuning",
        table,
        &[
            "big_log_delta",
            "big_lux_delta",
            "alpha",
            "confirm_close",
            "gate_immediate",
            "gate_minimum",
            "gate_confirmations",
            "ramp_step_ms",
            "learn_min_delta",
            "learn_max_contradicted",
            "learn_max_age_days",
            "preset_pause_minutes",
        ],
    );
    let p = |k: &str| format!("auto.tuning.{k}");
    Tuning {
        big_log_delta: r.float(
            &p("big_log_delta"),
            table.get("big_log_delta"),
            d.big_log_delta,
            0.01,
            2.0,
        ),
        big_lux_delta: r.float(
            &p("big_lux_delta"),
            table.get("big_lux_delta"),
            d.big_lux_delta,
            0.0,
            1000.0,
        ),
        alpha: r.float(&p("alpha"), table.get("alpha"), d.alpha, 0.01, 1.0),
        confirm_close: r.float(
            &p("confirm_close"),
            table.get("confirm_close"),
            d.confirm_close,
            0.01,
            1.0,
        ),
        gate_immediate: r.int(
            &p("gate_immediate"),
            table.get("gate_immediate"),
            15,
            1,
            100,
        ) as u8,
        gate_minimum: r.int(&p("gate_minimum"), table.get("gate_minimum"), 3, 1, 50) as u8,
        gate_confirmations: r.int(
            &p("gate_confirmations"),
            table.get("gate_confirmations"),
            3,
            1,
            10,
        ) as u8,
        ramp_step_ms: r.int(&p("ramp_step_ms"), table.get("ramp_step_ms"), 100, 20, 5000) as u32,
        learn_min_delta: r.int(
            &p("learn_min_delta"),
            table.get("learn_min_delta"),
            2,
            0,
            20,
        ) as u8,
        learn_max_contradicted: r.int(
            &p("learn_max_contradicted"),
            table.get("learn_max_contradicted"),
            2,
            0,
            8,
        ) as u8,
        learn_max_age_days: r.int(
            &p("learn_max_age_days"),
            table.get("learn_max_age_days"),
            0,
            0,
            3650,
        ) as u32,
        preset_pause_minutes: r.int(
            &p("preset_pause_minutes"),
            table.get("preset_pause_minutes"),
            30,
            1,
            1440,
        ) as u32,
    }
}

fn read_auto(r: &mut Reader, table: &Table) -> AutoConfig {
    let empty = Table::new();
    r.unknown_keys(
        "auto",
        table,
        &[
            "enabled",
            "url",
            "entity",
            "label",
            "poll_seconds",
            "stale_seconds",
            "fallback_seconds",
            "tuning",
            "token",
        ],
    );
    let tuning_table = r.table("auto.tuning", table, "tuning", &empty);
    let tuning = read_tuning(r, tuning_table);
    if table.contains_key("token") {
        r.note(
            Severity::Warning,
            "auto.token",
            "token ignored: it is not stored in this file but in the keyring",
        );
    }
    let mut auto = AutoConfig {
        enabled: r.boolean("auto.enabled", table.get("enabled"), false),
        url: String::new(),
        entity: String::new(),
        label: r.string("auto.label", table.get("label"), 127),
        poll_seconds: r.int("auto.poll_seconds", table.get("poll_seconds"), 15, 5, 300) as u32,
        stale_seconds: r.int(
            "auto.stale_seconds",
            table.get("stale_seconds"),
            600,
            30,
            86_400,
        ) as u32,
        fallback_seconds: r.int(
            "auto.fallback_seconds",
            table.get("fallback_seconds"),
            900,
            60,
            86_400,
        ) as u32,
        tuning,
    };
    let url = r.string("auto.url", table.get("url"), 255);
    if !url.trim().is_empty() {
        match check_url(&url) {
            Ok(checked) => {
                if checked.note == UrlNote::HttpPublic {
                    r.note(
                        Severity::Warning,
                        "auto.url",
                        "http outside the local network: the token would travel in clear text",
                    );
                }
                auto.url = checked.url;
            }
            Err(e) => r.note(Severity::Warning, "auto.url", format!("URL ignorée : {e}")),
        }
    }
    let entity = r.string("auto.entity", table.get("entity"), MAX_ENTITY_LEN);
    if !entity.is_empty() {
        if is_valid_entity_id(&entity) {
            auto.entity = entity;
        } else {
            r.note(
                Severity::Warning,
                "auto.entity",
                "invalid entity identifier (expected: domain.object), ignored",
            );
        }
    }
    if auto.enabled && (auto.url.is_empty() || auto.entity.is_empty()) {
        r.note(
            Severity::Warning,
            "auto.enabled",
            "auto-brightness requires a valid URL and entity, disabled",
        );
        auto.enabled = false;
    }
    auto
}

fn syntax_error(text: &str, e: &toml::de::Error) -> ConfigError {
    let line = e.span().map_or(1, |s| {
        text.get(..s.start)
            .map_or(1, |p| p.matches('\n').count() + 1)
    });
    ConfigError::Syntax {
        line,
        message: e.message().to_owned(),
    }
}

impl Config {
    /// Lit et valide un document TOML. Voir la documentation du module.
    pub fn parse(text: &str) -> Result<Loaded, ConfigError> {
        let root: Table = text.parse().map_err(|e| syntax_error(text, &e))?;
        let mut r = Reader { issues: Vec::new() };
        let empty = Table::new();
        let default = Config::default();

        let found = match root.get("version") {
            None => i64::from(CONFIG_VERSION),
            Some(Value::Integer(v)) => *v,
            Some(_) => {
                r.note(
                    Severity::Warning,
                    "version",
                    "integer expected, current version assumed",
                );
                i64::from(CONFIG_VERSION)
            }
        };
        if found > i64::from(CONFIG_VERSION) {
            return Err(ConfigError::UnsupportedVersion {
                found,
                supported: CONFIG_VERSION,
            });
        }
        r.unknown_keys(
            "",
            &root,
            &[
                "version", "general", "presets", "displays", "schedule", "idle", "auto",
            ],
        );

        let general = r.table("general", &root, "general", &empty);
        r.unknown_keys("general", general, &["step", "edid_bus_matching"]);
        let step = r.int("general.step", general.get("step"), 5, 1, 50) as u8;
        let edid_bus_matching = r.boolean(
            "general.edid_bus_matching",
            general.get("edid_bus_matching"),
            false,
        );

        let presets = root
            .get("presets")
            .map_or(default.presets, |v| read_presets(&mut r, v));
        let displays = root
            .get("displays")
            .map(|v| read_displays(&mut r, v))
            .unwrap_or_default();

        let schedule = r.table("schedule", &root, "schedule", &empty);
        r.unknown_keys("schedule", schedule, &["enabled", "points"]);
        let schedule = ScheduleConfig {
            enabled: r.boolean("schedule.enabled", schedule.get("enabled"), false),
            points: read_points(&mut r, schedule.get("points")),
        };

        let idle = r.table("idle", &root, "idle", &empty);
        r.unknown_keys(
            "idle",
            idle,
            &["enabled", "level", "minutes", "level_is_absolute"],
        );
        let idle = IdleConfig {
            enabled: r.boolean("idle.enabled", idle.get("enabled"), false),
            level: r.int("idle.level", idle.get("level"), 5, 0, 100) as u8,
            minutes: r.int("idle.minutes", idle.get("minutes"), 5, 1, 1440) as u16,
            level_is_absolute: r.boolean(
                "idle.level_is_absolute",
                idle.get("level_is_absolute"),
                false,
            ),
        };

        let auto_table = r.table("auto", &root, "auto", &empty);
        let auto = read_auto(&mut r, auto_table);

        Ok(Loaded {
            config: Config {
                version: CONFIG_VERSION,
                general: General {
                    step,
                    edid_bus_matching,
                },
                presets,
                displays,
                schedule,
                idle,
                auto,
            },
            issues: r.issues,
        })
    }

    /// Sérialise en TOML (écriture atomique à la charge du démon).
    pub fn to_toml(&self) -> Result<String, ConfigError> {
        toml::to_string_pretty(self).map_err(|e| ConfigError::Serialize(e.to_string()))
    }

    /// Construit le planning validé à partir des points de la configuration.
    pub fn build_schedule(&self) -> Result<Schedule, ScheduleError> {
        Schedule::new(self.schedule.points.clone())
    }

    /// Cherche un preset par nom, sans tenir compte de la casse.
    pub fn preset(&self, name: &str) -> Option<&Preset> {
        self.presets
            .iter()
            .find(|p| p.name.to_lowercase() == name.to_lowercase())
    }
}
