//! Banc de test du démon : matériel factice, horloge liée au temps de tokio.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use ha2lux_backends::task::TaskConfig;
use ha2lux_core::{DisplayKind, MinuteOfDay};
use ha2luxd::actor::{Daemon, DaemonHandle, Deps, Signal, StartError, start};
use ha2luxd::api::{ApiError, Reply, Request};
use ha2luxd::clock::TokioClock;
use ha2luxd::hardware::MockHardware;
use ha2luxd::paths::Paths;
use tempfile::TempDir;
use tokio::sync::mpsc::UnboundedReceiver;

pub struct Rig {
    pub daemon: Option<Daemon>,
    pub handle: DaemonHandle,
    pub hardware: Arc<MockHardware>,
    pub signals: UnboundedReceiver<Signal>,
    pub dir: TempDir,
    pub paths: Paths,
    pub ha: Option<ha2luxd::ha::HaDeps>,
}

fn deps(
    paths: &Paths,
    hardware: &Arc<MockHardware>,
    signals: tokio::sync::mpsc::UnboundedSender<Signal>,
    ha: Option<ha2luxd::ha::HaDeps>,
) -> Deps {
    Deps {
        paths: paths.clone(),
        hardware: Arc::clone(hardware) as _,
        clock: Arc::new(TokioClock::new(MinuteOfDay::parse("14:00").unwrap())),
        signals,
        // Pas de cadence minimale : les valeurs arrivent tout de suite sur l'écran factice.
        task_config: Some(TaskConfig {
            min_interval: Duration::ZERO,
            ..TaskConfig::ddc()
        }),
        planning_tick: Duration::from_secs(60),
        ha,
    }
}

pub fn write_file(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

impl Rig {
    /// Démarre un démon avec un écran factice « A » (max 100, niveau 50).
    pub async fn with_screen(config: Option<&str>, state: Option<&str>) -> Rig {
        let hardware = MockHardware::new();
        hardware.add("A", DisplayKind::External, 100, 50);
        Self::start_with(hardware, config, state).await.unwrap()
    }

    pub async fn start_with(
        hardware: Arc<MockHardware>,
        config: Option<&str>,
        state: Option<&str>,
    ) -> Result<Rig, StartError> {
        Self::start_with_ha(hardware, config, state, None).await
    }

    pub async fn start_with_ha(
        hardware: Arc<MockHardware>,
        config: Option<&str>,
        state: Option<&str>,
        ha: Option<ha2luxd::ha::HaDeps>,
    ) -> Result<Rig, StartError> {
        let dir = TempDir::new().unwrap();
        let paths = Paths::in_dir(dir.path());
        if let Some(text) = config {
            write_file(&paths.config_file, text);
        }
        if let Some(text) = state {
            write_file(&paths.state_file, text);
        }
        let (tx, signals) = tokio::sync::mpsc::unbounded_channel();
        let daemon = start(deps(&paths, &hardware, tx, ha.clone())).await?;
        let handle = daemon.handle();
        let rig = Rig {
            daemon: Some(daemon),
            handle,
            hardware,
            signals,
            dir,
            paths,
            ha,
        };
        rig.settle().await;
        Ok(rig)
    }

    /// Arrêt propre puis redémarrage sur les mêmes fichiers et le même matériel.
    pub async fn restart(&mut self) {
        self.daemon.take().unwrap().shutdown().await;
        let (tx, signals) = tokio::sync::mpsc::unbounded_channel();
        let daemon = start(deps(&self.paths, &self.hardware, tx, self.ha.clone()))
            .await
            .unwrap();
        self.handle = daemon.handle();
        self.daemon = Some(daemon);
        self.signals = signals;
        self.settle().await;
    }

    /// Laisse tourner tous les acteurs et tâches prêts, sans faire avancer le temps.
    pub async fn settle(&self) {
        for _ in 0..200 {
            tokio::task::yield_now().await;
        }
    }

    /// Fait avancer le temps virtuel **pas à pas** (les minuteries enchaînées, comme le
    /// backoff de 50 puis 100 ms, se déclenchent dans l'ordre, comme en production).
    pub async fn advance(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
        self.settle().await;
    }

    pub async fn ask(&self, request: Request) -> Result<Reply, ApiError> {
        let result = self.handle.request(request).await;
        self.settle().await;
        result
    }

    pub async fn ok(&self, request: Request) {
        self.ask(request).await.unwrap();
    }

    pub fn drain_signals(&mut self) -> Vec<Signal> {
        std::iter::from_fn(|| self.signals.try_recv().ok()).collect()
    }

    pub async fn shutdown(mut self) -> (Paths, TempDir, Arc<MockHardware>) {
        self.daemon.take().unwrap().shutdown().await;
        (self.paths, self.dir, self.hardware)
    }
}

/// Attend (sans faire avancer le temps) qu'un fichier existe et satisfasse `ready`.
pub async fn wait_file(path: &Path, ready: impl Fn(&str) -> bool) -> String {
    for _ in 0..2_000_000 {
        if let Ok(text) = std::fs::read_to_string(path)
            && ready(&text)
        {
            return text;
        }
        tokio::task::yield_now().await;
    }
    panic!(
        "{} n'a pas atteint l'état attendu : {:?}",
        path.display(),
        std::fs::read_to_string(path)
    );
}

pub const PLANNING_CONFIG: &str = "[schedule]\nenabled = true\npoints = [{ time = \"08:00\", level = 80 }, { time = \"20:00\", level = 20 }]\n";
