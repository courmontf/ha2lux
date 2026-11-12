//! State displayed by the tray icon and menu description, with no I/O.

use std::collections::HashMap;

use ha2lux_i18n::t;
use ha2lux_ipc::DisplayDto;
use zbus::zvariant::OwnedValue;

/// What the tray icon knows about the daemon.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrayState {
    /// The daemon responds.
    pub reachable: bool,
    /// Master level.
    pub master: Option<u8>,
    /// Source de base (`auto-lux`, `planning`, `static`).
    pub mode: String,
    /// Wheel step.
    pub step: u8,
    /// Schedule enabled.
    pub schedule_enabled: bool,
    /// Number of schedule points.
    pub schedule_points: u32,
    /// Resume time of a suspended schedule.
    pub schedule_resume: Option<String>,
    /// Idle dimming enabled.
    pub idle_enabled: bool,
    /// Dimming level.
    pub idle_level: u8,
    /// Idle delay in minutes.
    pub idle_minutes: u16,
    /// Dimming in progress.
    pub dimmed: bool,
    /// Auto-brightness enabled.
    pub auto_enabled: bool,
    /// Auto-brightness configured.
    pub auto_configured: bool,
    /// Health of the Home Assistant link.
    pub ha_health: String,
    /// Daemon version (empty if unknown).
    pub daemon_version: String,
    /// Displays.
    pub displays: Vec<DisplayDto>,
    /// Presets (nom, niveau).
    pub presets: Vec<(String, u32)>,
}

fn get<T: TryFrom<OwnedValue>>(map: &HashMap<String, OwnedValue>, key: &str) -> Option<T> {
    map.get(key)
        .and_then(|v| v.try_clone().ok())
        .and_then(|v| T::try_from(v).ok())
}

impl TrayState {
    /// Builds the state from `Status()`, `List()` and `Presets()`.
    pub fn from_daemon(
        status: &HashMap<String, OwnedValue>,
        displays: Vec<DisplayDto>,
        presets: Vec<(String, u32)>,
    ) -> Self {
        Self {
            reachable: true,
            master: get::<i32>(status, "master").and_then(|m| u8::try_from(m).ok()),
            mode: get(status, "mode").unwrap_or_default(),
            step: get::<u32>(status, "step")
                .and_then(|v| u8::try_from(v).ok())
                .unwrap_or(5)
                .max(1),
            schedule_enabled: get(status, "schedule_enabled").unwrap_or_default(),
            schedule_points: get(status, "schedule_points").unwrap_or_default(),
            schedule_resume: get(status, "schedule_resume"),
            idle_enabled: get(status, "idle_enabled").unwrap_or_default(),
            idle_level: get::<u32>(status, "idle_level")
                .and_then(|v| u8::try_from(v).ok())
                .unwrap_or(5),
            idle_minutes: get::<u32>(status, "idle_minutes")
                .and_then(|v| u16::try_from(v).ok())
                .unwrap_or(5),
            dimmed: get(status, "dimmed").unwrap_or_default(),
            auto_enabled: get(status, "auto_enabled").unwrap_or_default(),
            auto_configured: get(status, "auto_configured").unwrap_or_default(),
            ha_health: get(status, "ha_health").unwrap_or_default(),
            daemon_version: get(status, "version").unwrap_or_default(),
            displays,
            presets,
        }
    }

    /// Auto-brightness is currently driving the brightness.
    pub fn auto_drives(&self) -> bool {
        self.auto_enabled && self.auto_configured
    }
}

/// Action triggered by a menu item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Master level.
    SetMaster(u32),
    /// A display's own level.
    SetDisplay {
        /// Display identity.
        id: String,
        /// Niveau 0-100.
        level: u32,
    },
    /// Preset by name.
    Preset(String),
    /// Enables or disables a feature (`schedule`, `idle-dim`, `auto`).
    Toggle(&'static str, bool),
    /// Restarts enumeration.
    Rescan,
    /// Opens the configuration file.
    OpenSettings,
    /// Makes the daemon reload the configuration, then the tray icon reload the language.
    ReloadConfig,
    /// The menu opens (retries for pending displays are postponed).
    MenuOpened,
    /// Quits the tray icon (not the daemon).
    Quit,
}

/// Menu item, independent of `ksni`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    /// Plain item.
    Item {
        /// Label.
        label: String,
        /// Cliquable.
        enabled: bool,
        /// Action au clic.
        action: Option<Action>,
    },
    /// Checkbox.
    Check {
        /// Label.
        label: String,
        /// Checked.
        checked: bool,
        /// Cliquable.
        enabled: bool,
        /// Action au clic.
        action: Action,
    },
    /// Separator.
    Separator,
    /// Sous-menu.
    Sub {
        /// Label.
        label: String,
        /// Cliquable (ouvrable).
        enabled: bool,
        /// Contenu.
        entries: Vec<Entry>,
    },
}

fn item(label: impl Into<String>, action: Action) -> Entry {
    Entry::Item {
        label: label.into(),
        enabled: true,
        action: Some(action),
    }
}

fn note(label: impl Into<String>) -> Entry {
    Entry::Item {
        label: label.into(),
        enabled: false,
        action: None,
    }
}

/// Levels offered in the submenus (the spec's slider popup does not exist in SNI).
/// Tray icon version (that of the whole project).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// "À propos" submenu: tray icon and daemon versions, with a warning
/// if they differ (daemon not restarted after an update).
fn about(state: &TrayState) -> Entry {
    let mut entries = vec![note(t!("tray-about-tray", version = VERSION))];
    if !state.reachable {
        entries.push(note(t!("tray-about-daemon-unreachable")));
    } else if state.daemon_version.is_empty() {
        entries.push(note(t!("tray-about-daemon-unknown")));
    } else {
        entries.push(note(t!(
            "tray-about-daemon",
            version = state.daemon_version.as_str()
        )));
        if state.daemon_version != VERSION {
            entries.push(note(t!("tray-about-mismatch")));
        }
    }
    Entry::Sub {
        label: t!("tray-about"),
        enabled: true,
        entries,
    }
}

const LEVELS: [u32; 11] = [0, 10, 20, 30, 40, 50, 60, 70, 80, 90, 100];

fn levels(make: impl Fn(u32) -> Action, current: Option<u8>) -> Vec<Entry> {
    LEVELS
        .iter()
        .map(|&level| {
            let near = current.is_some_and(|c| u32::from(c).abs_diff(level) < 5);
            Entry::Check {
                label: percent(level),
                checked: near,
                enabled: true,
                action: make(level),
            }
        })
        .collect()
}

fn percent(value: impl std::fmt::Display) -> String {
    t!("tray-percent", value = value.to_string())
}

fn health_label(health: &str) -> String {
    match health {
        "ok" => t!("tray-sensor-ok"),
        "stale" => t!("tray-sensor-stale"),
        "degraded" => t!("tray-sensor-degraded"),
        "offline" => t!("tray-sensor-offline"),
        _ => t!("tray-sensor-unknown"),
    }
}

/// Menu description for this state.
pub fn build_menu(state: &TrayState) -> Vec<Entry> {
    let mut menu = Vec::new();
    if !state.reachable {
        menu.push(note(t!("tray-daemon-unreachable")));
        menu.push(about(state));
        menu.push(Entry::Separator);
        menu.push(item(t!("tray-quit"), Action::Quit));
        return menu;
    }
    menu.push(note(match state.master {
        Some(level) => t!("tray-brightness", level = percent(level)),
        None => t!("tray-brightness-unknown"),
    }));
    menu.push(Entry::Sub {
        label: t!("tray-all-displays"),
        enabled: !state.displays.is_empty(),
        entries: levels(Action::SetMaster, state.master),
    });
    for display in &state.displays {
        if display.state == "controllable" {
            let level = u8::try_from(display.level).ok();
            let id = display.id.clone();
            menu.push(Entry::Sub {
                label: match level {
                    Some(level) => t!(
                        "tray-display-with-level",
                        name = display.name.as_str(),
                        level = percent(level)
                    ),
                    None => display.name.clone(),
                },
                enabled: true,
                entries: levels(
                    move |level| Action::SetDisplay {
                        id: id.clone(),
                        level,
                    },
                    level,
                ),
            });
        } else {
            menu.push(note(t!(
                "tray-display-unavailable",
                name = display.name.as_str()
            )));
        }
    }
    if !state.presets.is_empty() {
        menu.push(Entry::Separator);
        for (name, level) in &state.presets {
            menu.push(item(
                t!("tray-preset", name = name.as_str(), level = percent(level)),
                Action::Preset(name.clone()),
            ));
        }
    }
    menu.push(Entry::Separator);
    menu.push(Entry::Check {
        label: if state.auto_configured {
            t!("tray-auto-configured")
        } else {
            t!("tray-auto-unconfigured")
        },
        checked: state.auto_enabled,
        enabled: state.auto_configured,
        action: Action::Toggle("auto", !state.auto_enabled),
    });
    if state.auto_drives() {
        menu.push(note(health_label(&state.ha_health)));
    }
    let schedule_label = if state.auto_drives() {
        t!("tray-schedule-fallback")
    } else {
        t!("tray-schedule")
    };
    menu.push(Entry::Check {
        label: schedule_label,
        checked: state.schedule_enabled,
        // Greyed out while auto-brightness is driving; with no point, there is nothing to enable.
        enabled: !state.auto_drives() && state.schedule_points > 0,
        action: Action::Toggle("schedule", !state.schedule_enabled),
    });
    if let Some(resume) = &state.schedule_resume {
        menu.push(note(t!("tray-schedule-resume", resume = resume.as_str())));
    }
    menu.push(Entry::Check {
        label: t!(
            "tray-idle-dim",
            level = state.idle_level.to_string(),
            minutes = state.idle_minutes.to_string()
        ),
        checked: state.idle_enabled,
        enabled: true,
        action: Action::Toggle("idle-dim", !state.idle_enabled),
    });
    menu.push(Entry::Separator);
    menu.push(item(t!("tray-rescan"), Action::Rescan));
    menu.push(item(t!("tray-settings"), Action::OpenSettings));
    menu.push(item(t!("tray-reload"), Action::ReloadConfig));
    menu.push(about(state));
    menu.push(item(t!("tray-quit"), Action::Quit));
    menu
}

/// Texte de l'infobulle.
pub fn tooltip(state: &TrayState) -> String {
    if !state.reachable {
        return t!("tray-tooltip-unreachable");
    }
    let level = state.master.map_or_else(|| "?".to_owned(), percent);
    if state.dimmed {
        t!(
            "tray-tooltip-dimmed",
            level = level,
            mode = state.mode.as_str()
        )
    } else {
        t!("tray-tooltip", level = level, mode = state.mode.as_str())
    }
}

/// Icon name (freedesktop theme).
pub fn icon_name(state: &TrayState) -> &'static str {
    if state.reachable {
        "display-brightness-symbolic"
    } else {
        "dialog-warning-symbolic"
    }
}
