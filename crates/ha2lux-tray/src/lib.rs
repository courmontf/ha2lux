//! Icône de zone de notification de ha2lux (StatusNotifierItem via `ksni`).
//!
//! - [`model`] : état affiché et description **pure** du menu (testable sans bus) ;
//! - [`wheel`] : accumulation des crans de molette (appliqués d'un seul coup) ;
//! - [`icon`] : image de l'icône, dessinée par le programme ;
//! - [`osd`] : affichage du niveau par une notification remplaçable ;
//! - [`app`] : boucle d'exécution (client D-Bus du démon, icône, OSD).
#![forbid(unsafe_code)]

pub mod app;
pub mod icon;
pub mod model;
pub mod osd;
pub mod wheel;
