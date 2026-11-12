//! Boucle d'exécution de l'icône : client D-Bus du démon, icône `ksni`, OSD de la molette.

use std::path::PathBuf;
use std::time::Duration;

use futures_util::StreamExt;
use ha2lux_i18n::t;
use ha2lux_ipc::DaemonProxy;
use ksni::menu::{CheckmarkItem, StandardItem, SubMenu};
use ksni::{MenuItem, Orientation, ToolTip, TrayMethods};
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::model::{Action, Entry, TrayState, build_menu, tooltip};
use crate::osd::Osd;
use crate::wheel::{FLUSH_AFTER, Wheel};

/// Relecture périodique de l'état quand le démon répond.
const REFRESH: Duration = Duration::from_secs(30);
/// Relecture quand le démon est injoignable (il peut démarrer à tout moment).
const REFRESH_UNREACHABLE: Duration = Duration::from_secs(3);
/// Durée pendant laquelle un menu ouvert reporte les relances des écrans en attente.
const UI_OPEN_FOR: Duration = Duration::from_secs(15);

/// Échec de l'icône.
#[derive(Debug)]
pub enum AppError {
    /// Pas de bus de session.
    Bus(String),
    /// Aucun hôte d'icônes (pas de StatusNotifierWatcher).
    NoWatcher(String),
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bus(e) => f.write_str(&t!("tray-err-bus", error = e.as_str())),
            Self::NoWatcher(e) => f.write_str(&t!("tray-err-no-watcher", error = e.as_str())),
        }
    }
}

impl std::error::Error for AppError {}

/// Réglages de l'icône.
pub struct AppOptions {
    /// Connexion explicite (tests) ; `None` : bus de session.
    pub connection: Option<zbus::Connection>,
    /// Fichier de configuration ouvert par « Réglages… ».
    pub config_file: PathBuf,
}

struct Tray {
    state: TrayState,
    actions: mpsc::UnboundedSender<Action>,
    notches: mpsc::UnboundedSender<i32>,
}

fn convert(entries: &[Entry], tx: &mpsc::UnboundedSender<Action>) -> Vec<MenuItem<Tray>> {
    entries
        .iter()
        .map(|entry| match entry {
            Entry::Separator => MenuItem::Separator,
            Entry::Item {
                label,
                enabled,
                action,
            } => {
                let (tx, action) = (tx.clone(), action.clone());
                StandardItem {
                    label: label.clone(),
                    enabled: *enabled,
                    activate: Box::new(move |_: &mut Tray| {
                        if let Some(action) = &action {
                            // La boucle a pu s'arrêter : sans importance.
                            let _ = tx.send(action.clone());
                        }
                    }),
                    ..Default::default()
                }
                .into()
            }
            Entry::Check {
                label,
                checked,
                enabled,
                action,
            } => {
                let (tx, action) = (tx.clone(), action.clone());
                CheckmarkItem {
                    label: label.clone(),
                    checked: *checked,
                    enabled: *enabled,
                    activate: Box::new(move |_: &mut Tray| {
                        let _ = tx.send(action.clone());
                    }),
                    ..Default::default()
                }
                .into()
            }
            Entry::Sub {
                label,
                enabled,
                entries,
            } => SubMenu {
                label: label.clone(),
                enabled: *enabled,
                submenu: convert(entries, tx),
                ..Default::default()
            }
            .into(),
        })
        .collect()
}

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        "ha2lux".to_owned()
    }

    fn title(&self) -> String {
        "ha2lux".to_owned()
    }

    fn icon_name(&self) -> String {
        // Vide : l'image fournie par `icon_pixmap` est utilisée (aucune dépendance au thème).
        String::new()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        [22_u32, 32, 48, 64]
            .iter()
            .map(|&size| ksni::Icon {
                width: i32::try_from(size).unwrap_or(22),
                height: i32::try_from(size).unwrap_or(22),
                data: crate::icon::sun_argb(size, self.state.reachable),
            })
            .collect()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: "ha2lux".to_owned(),
            description: tooltip(&self.state),
            ..Default::default()
        }
    }

    fn scroll(&mut self, delta: i32, orientation: Orientation) {
        if orientation == Orientation::Vertical && delta != 0 {
            let _ = self.notches.send(delta.signum());
        }
    }

    fn menu_about_to_show(&mut self) {
        let _ = self.actions.send(Action::MenuOpened);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        convert(&build_menu(&self.state), &self.actions)
    }
}

/// Lit l'état complet du démon.
pub async fn read_state(proxy: &DaemonProxy<'_>) -> Option<TrayState> {
    let status = proxy.status().await.ok()?;
    let displays = proxy.list().await.ok()?;
    let presets = proxy.presets().await.unwrap_or_default();
    Some(TrayState::from_daemon(&status, displays, presets))
}

async fn at(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

/// Exécute l'icône jusqu'à « Quitter ».
pub async fn run(options: AppOptions) -> Result<(), AppError> {
    let connection = match options.connection {
        Some(connection) => connection,
        None => zbus::Connection::session()
            .await
            .map_err(|e| AppError::Bus(e.to_string()))?,
    };
    let proxy = DaemonProxy::new(&connection)
        .await
        .map_err(|e| AppError::Bus(e.to_string()))?;
    let (actions, mut action_rx) = mpsc::unbounded_channel();
    let (notches, mut notch_rx) = mpsc::unbounded_channel();
    let state = read_state(&proxy).await.unwrap_or_default();
    let handle = match (Tray {
        state: state.clone(),
        actions: actions.clone(),
        notches,
    })
    .spawn()
    .await
    {
        Ok(handle) => handle,
        Err(ksni::Error::WontShow) => {
            return Err(AppError::NoWatcher(t!("tray-detail-no-host")));
        }
        Err(error) => return Err(AppError::NoWatcher(error.to_string())),
    };
    let mut osd = Osd::new(&connection).await.ok();

    // Les flux de signaux : un changement côté démon (raccourci, CLI, planning…) rafraîchit l'icône.
    let mut level = proxy
        .receive_level_changed()
        .await
        .map_err(|e| AppError::Bus(e.to_string()))?;
    let mut displays = proxy
        .receive_displays_changed()
        .await
        .map_err(|e| AppError::Bus(e.to_string()))?;
    let mut mode = proxy
        .receive_mode_switched()
        .await
        .map_err(|e| AppError::Bus(e.to_string()))?;

    let mut wheel = Wheel::default();
    let mut flush_at: Option<Instant> = None;
    let mut ui_until: Option<Instant> = None;
    let mut step = state.step.max(1);
    let mut refresh_at = Instant::now() + REFRESH;

    loop {
        let mut refresh = false;
        tokio::select! {
            Some(action) = action_rx.recv() => {
                match action {
                    Action::Quit => break,
                    Action::MenuOpened => {
                        if ui_until.is_none() {
                            let _ = proxy.set_ui_open(true).await;
                        }
                        ui_until = Some(Instant::now() + UI_OPEN_FOR);
                    }
                    other => {
                        run_action(&proxy, &options.config_file, other).await;
                        refresh = true;
                    }
                }
            }
            Some(count) = notch_rx.recv() => {
                wheel.push(count);
                flush_at.get_or_insert_with(|| Instant::now() + FLUSH_AFTER);
            }
            () = at(flush_at) => {
                flush_at = None;
                if let Some(delta) = wheel.take(step) {
                    if let Err(error) = proxy.step_master(delta).await {
                        tracing::debug!(%error, "wheel step refused by the daemon");
                    }
                    if let (Some(osd), Ok(master)) = (osd.as_mut(), proxy.master().await)
                        && let Ok(master) = u8::try_from(master)
                    {
                        osd.show(master).await;
                    }
                    refresh = true;
                }
            }
            () = at(ui_until) => {
                ui_until = None;
                let _ = proxy.set_ui_open(false).await;
            }
            Some(_) = level.next() => refresh = true,
            Some(_) = displays.next() => refresh = true,
            Some(_) = mode.next() => refresh = true,
            () = tokio::time::sleep_until(refresh_at) => refresh = true,
        }
        if refresh {
            let fresh = read_state(&proxy).await.unwrap_or_default();
            let reachable = fresh.reachable;
            step = fresh.step.max(1);
            if handle.update(|tray| tray.state = fresh).await.is_none() {
                break;
            }
            refresh_at = Instant::now()
                + if reachable {
                    REFRESH
                } else {
                    REFRESH_UNREACHABLE
                };
        }
    }
    if ui_until.is_some() {
        let _ = proxy.set_ui_open(false).await;
    }
    handle.shutdown().await;
    Ok(())
}

async fn run_action(proxy: &DaemonProxy<'_>, config_file: &std::path::Path, action: Action) {
    let result = match action {
        Action::SetMaster(level) => proxy.set_master(level).await,
        Action::SetDisplay { id, level } => proxy.set_display(&id, level).await,
        Action::Preset(name) => proxy.apply_preset(&name).await,
        Action::Toggle(feature, on) => proxy.set_mode(feature, on).await,
        Action::Rescan => proxy.rescan().await,
        Action::OpenSettings => {
            // Ouverture par le gestionnaire de fichiers du bureau ; un échec n'est pas grave.
            if let Err(error) = tokio::process::Command::new("xdg-open")
                .arg(config_file)
                .spawn()
            {
                tracing::warn!(%error, "xdg-open failed");
            }
            Ok(())
        }
        Action::MenuOpened | Action::Quit => Ok(()),
    };
    if let Err(error) = result {
        tracing::warn!(%error, "command refused by the daemon");
    }
}
