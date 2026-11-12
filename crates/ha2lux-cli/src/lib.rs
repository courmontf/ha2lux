//! CLI `ha2lux` : parle au démon par D-Bus ; une seule commande par appel.
//!
//! Codes de sortie : 0 succès · 1 usage · 2 démon absent · 3 échec de la commande ·
//! 4 démon sans réponse. Les mêmes validations existent dans le démon, qui reste
//! l'autorité : la CLI ne fait que fournir un message d'erreur plus tôt et plus clair.
#![forbid(unsafe_code)]

pub mod cli;
pub mod client;
pub mod output;
pub mod run;
pub mod select;
