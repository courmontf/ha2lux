//! Configuration model, tolerant reading and TOML serialization (spec §10).
//!
//! Pure part: no I/O. Reading never loses the configuration: a
//! faulty key falls back to its default value (or is clamped) and a
//! [`ConfigIssue`] is recorded; only a TOML syntax error or a
//! schema version that is too recent are fatal.
//!
//! Key values are never copied into messages: a token
//! misplaced in the file therefore cannot leak into the logs.

use serde::{Serialize, Serializer};
use thiserror::Error;
use toml::{Table, Value};

use crate::curve::LearnPolicy;
use crate::filter::FilterParams;
use crate::gate::GateParams;
use crate::range::Range;
use crate::schedule::{MAX_POINTS, MinuteOfDay, Point, Schedule, ScheduleError};
use crate::url::{UrlNote, check_url};

/// Current version of the configuration schema.
pub const CONFIG_VERSION: u32 = 1;
/// Maximum number of presets.
pub const MAX_PRESETS: usize = 10;
/// Maximum number of remembered displays.
pub const MAX_DISPLAYS: usize = 32;
/// Maximum length of a preset name.
pub const MAX_PRESET_NAME: usize = 63;
/// Maximum length of an entity identifier.
pub const MAX_ENTITY_LEN: usize = 127;

/// Fatal errors when reading or writing the configuration.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ConfigError {
    /// Invalid TOML syntax.
    #[error("unreadable configuration (line {line}): {message}")]
    Syntax {
        /// Approximate line of the error (1 if unknown).
        line: usize,
        /// Parser message, without an excerpt of the file.
        message: String,
    },
    /// Schema newer than this binary.
    #[error("version de configuration {found} non prise en charge (maximum {supported})")]
    UnsupportedVersion {
        /// Version found in the file.
        found: i64,
        /// Highest known version.
        supported: u32,
    },
    /// Serialization failure.
    #[error("serialization failed: {0}")]
    Serialize(String),
}

/// Severity of a reading remark.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    /// Information (value corrected without loss).
    Info,
    /// Warning (value replaced or ignored).
    Warning,
}

/// Remark produced while reading (never a sensitive value).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigIssue {
    /// Severity.
    pub severity: Severity,
    /// Key path (e.g. `idle.minutes`).
    pub path: String,
    /// Human-readable description.
    pub message: String,
}

/// Result of a read: configuration and remarks.
#[derive(Clone, Debug, PartialEq)]
pub struct Loaded {
    /// Validated configuration.
    pub config: Config,
    /// Corrections applied.
    pub issues: Vec<ConfigIssue>,
}

/// General settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct General {
    /// Step of the wheel/shortcuts (1-50).
    pub step: u8,
    /// Associate displays with their DDC buses by reading the EDID on the GPU
    /// adapters (proprietary NVIDIA driver, no `ddc` link in sysfs). Disabled
    /// by default: this performs real I²C reads (decision 48).
    pub edid_bus_matching: bool,
    /// Interface language: `auto` (follows the session) or a language code (`fr`, `en`…).
    /// The choice of the language itself (and the fallback to a missing language) is made by
    /// `ha2lux-i18n`, which alone knows the available languages.
    pub language: String,
}

/// Default value of [`General::language`].
pub const LANGUAGE_AUTO: &str = "auto";

/// True for `auto` or a simple language code (`fr`, `en`, `pt-br`).
fn valid_language(code: &str) -> bool {
    code == LANGUAGE_AUTO
        || (code.len() <= 8
            && code.split(['-', '_']).all(|part| {
                !part.is_empty()
                    && part
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            })
            && code.chars().next().is_some_and(|c| c.is_ascii_lowercase()))
}

/// Named preset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preset {
    /// Name (1-63 characters).
    pub name: String,
    /// Master level 0-100.
    pub level: u8,
}

/// Remembered configuration of a display.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisplayEntry {
    /// Stable identifier (EDID key).
    pub id: String,
    /// Range of the display.
    pub range: Range,
    /// Backlight floor, as a raw value.
    pub floor: u32,
}

/// Schedule.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ScheduleConfig {
    /// Schedule enabled.
    pub enabled: bool,
    /// Validated points (sorted, no duplicates, ≤ 24).
    #[serde(serialize_with = "ser_points")]
    pub points: Vec<Point>,
}

/// Idle dimming.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct IdleConfig {
    /// Feature enabled.
    pub enabled: bool,
    /// Dimming level 0-100.
    pub level: u8,
    /// Delay in minutes (1-1440).
    pub minutes: u16,
    /// Absolute level (not remapped by the range).
    pub level_is_absolute: bool,
}

/// Fine settings for auto-brightness (`auto.tuning.*`).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Tuning {
    /// Filter: log gap of a large change.
    pub big_log_delta: f64,
    /// Filter: gap in lux of a large change.
    pub big_lux_delta: f64,
    /// Filter: smoothing coefficient.
    pub alpha: f64,
    /// Filter: confirmation closeness.
    pub confirm_close: f64,
    /// Gate: gap for immediate application.
    pub gate_immediate: u8,
    /// Gate: minimum gap.
    pub gate_minimum: u8,
    /// Gate: confirmations required.
    pub gate_confirmations: u8,
    /// Ramp: step duration in ms (never faster than the bus).
    pub ramp_step_ms: u32,
    /// Learning: gap floor (0 = disabled).
    pub learn_min_delta: u8,
    /// Learning: contradictions tolerated before rejection.
    pub learn_max_contradicted: u8,
    /// Learning: maximum age of a point in days (0 = unlimited).
    pub learn_max_age_days: u32,
    /// Auto pause duration after a preset, in minutes.
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
    /// Filter settings.
    pub fn filter_params(&self) -> FilterParams {
        FilterParams {
            big_log_delta: self.big_log_delta,
            big_lux_delta: self.big_lux_delta,
            alpha: self.alpha,
            confirm_close: self.confirm_close,
        }
    }

    /// Gate settings.
    pub fn gate_params(&self) -> GateParams {
        GateParams {
            immediate: self.gate_immediate,
            minimum: self.gate_minimum,
            confirmations: self.gate_confirmations,
        }
    }

    /// Learning safeguards.
    pub fn learn_policy(&self) -> LearnPolicy {
        LearnPolicy {
            min_delta: self.learn_min_delta,
            max_contradicted: Some(usize::from(self.learn_max_contradicted)),
        }
    }
}

/// Home Assistant auto-brightness. The token never appears here.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AutoConfig {
    /// Feature enabled (requires URL and entity, plus the token on the daemon side).
    pub enabled: bool,
    /// Normalized Home Assistant URL.
    pub url: String,
    /// Entity identifier (`sensor.xxx`).
    pub entity: String,
    /// Display label.
    pub label: String,
    /// REST fallback period in seconds (5-300).
    pub poll_seconds: u32,
    /// Staleness of a measurement in seconds.
    pub stale_seconds: u32,
    /// Delay with no valid measurement before the "degraded" state, in seconds.
    pub fallback_seconds: u32,
    /// Fine settings.
    pub tuning: Tuning,
}

/// Complete configuration.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Config {
    /// Schema version.
    pub version: u32,
    /// General settings.
    pub general: General,
    /// Presets (≤ 10), in file order.
    #[serde(serialize_with = "ser_presets")]
    pub presets: Vec<Preset>,
    /// Remembered displays (≤ 32, even unplugged).
    #[serde(serialize_with = "ser_displays")]
    pub displays: Vec<DisplayEntry>,
    /// Schedule.
    pub schedule: ScheduleConfig,
    /// Idle dimming.
    pub idle: IdleConfig,
    /// Auto-brightness.
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
                language: LANGUAGE_AUTO.to_owned(),
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

/// True for a Home Assistant entity identifier `domain.object`
/// (lowercase, digits, `_`), at most 127 characters. Rejects `/`, `..`, `?`.
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
                    format!("text too long (maximum {max_chars}), ignored"),
                );
                String::new()
            }
            Some(_) => {
                self.note(Severity::Warning, path, "text expected, default value used");
                String::new()
            }
        }
    }

    /// `key` sub-table of `parent`, or empty table (with a remark if the type is wrong).
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
    /// Reads and validates a TOML document. See the module documentation.
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
        r.unknown_keys(
            "general",
            general,
            &["step", "edid_bus_matching", "language"],
        );
        let step = r.int("general.step", general.get("step"), 5, 1, 50) as u8;
        let edid_bus_matching = r.boolean(
            "general.edid_bus_matching",
            general.get("edid_bus_matching"),
            false,
        );

        let language = {
            let raw = r.string("general.language", general.get("language"), 16);
            let code = raw.trim().to_lowercase();
            if code.is_empty() {
                LANGUAGE_AUTO.to_owned()
            } else if valid_language(&code) {
                code
            } else {
                r.note(
                    Severity::Warning,
                    "general.language",
                    "invalid language (expected \"auto\" or a code such as \"fr\"), \"auto\" used",
                );
                LANGUAGE_AUTO.to_owned()
            }
        };

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
                    language,
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

    /// Serializes to TOML (atomic write is the daemon's responsibility).
    pub fn to_toml(&self) -> Result<String, ConfigError> {
        toml::to_string_pretty(self).map_err(|e| ConfigError::Serialize(e.to_string()))
    }

    /// Builds the validated schedule from the configuration points.
    pub fn build_schedule(&self) -> Result<Schedule, ScheduleError> {
        Schedule::new(self.schedule.points.clone())
    }

    /// Looks up a preset by name, ignoring case.
    pub fn preset(&self, name: &str) -> Option<&Preset> {
        self.presets
            .iter()
            .find(|p| p.name.to_lowercase() == name.to_lowercase())
    }
}
