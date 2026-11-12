//! Enveloppe de tous les backends d'affichage.
//!
//! `DisplayBackend` renvoie des `impl Future` : il n'est pas utilisable comme
//! objet dynamique. Cette énumération permet au démon de piloter des écrans
//! hétérogènes (DDC/CI, backlight, factice) avec une seule tâche d'écran.

use ha2lux_core::{BackendError, DisplayBackend, DisplayInfo};

use crate::backlight::{BacklightBackend, LogindSetter};
use crate::ddc::DdcBackend;
use crate::mock::MockBackend;

/// Un backend d'affichage quelconque.
pub enum AnyBackend {
    /// Écran externe en DDC/CI.
    Ddc(DdcBackend),
    /// Panneau interne via logind.
    Backlight(BacklightBackend<LogindSetter>),
    /// Écran factice (tests).
    Mock(MockBackend),
}

impl DisplayBackend for AnyBackend {
    fn info(&self) -> &DisplayInfo {
        match self {
            Self::Ddc(b) => b.info(),
            Self::Backlight(b) => b.info(),
            Self::Mock(b) => b.info(),
        }
    }

    async fn probe(&mut self) -> Result<DisplayInfo, BackendError> {
        match self {
            Self::Ddc(b) => b.probe().await,
            Self::Backlight(b) => b.probe().await,
            Self::Mock(b) => b.probe().await,
        }
    }

    async fn read(&mut self) -> Result<u32, BackendError> {
        match self {
            Self::Ddc(b) => b.read().await,
            Self::Backlight(b) => b.read().await,
            Self::Mock(b) => b.read().await,
        }
    }

    async fn write(&mut self, raw: u32) -> Result<(), BackendError> {
        match self {
            Self::Ddc(b) => b.write(raw).await,
            Self::Backlight(b) => b.write(raw).await,
            Self::Mock(b) => b.write(raw).await,
        }
    }
}
