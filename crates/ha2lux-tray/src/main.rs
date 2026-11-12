//! Icône de zone de notification ha2lux.
#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::process::ExitCode;

use ha2lux_tray::app::{AppOptions, run};
use tracing_subscriber::EnvFilter;

const USAGE: &str = "usage : ha2lux-tray [--help | --version]

Icône de zone de notification (StatusNotifierItem) pour ha2lux : menu des presets,
molette pour ajuster la luminosité, bascules planning / auto / atténuation.
Le démon ha2luxd doit tourner ; l'icône l'interroge par D-Bus.

codes de sortie : 0 arrêt normal, 1 usage, 3 pas de zone de notification ou de bus";

fn config_file() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("/"));
    base.join("ha2lux").join("config.toml")
}

#[tokio::main]
async fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        None => {}
        Some("--help" | "-h") => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Some("--version" | "-V") => {
            println!("ha2lux-tray {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Some(other) => {
            eprintln!("argument inattendu : {other}\n{USAGE}");
            return ExitCode::from(1);
        }
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    match run(AppOptions {
        connection: None,
        config_file: config_file(),
    })
    .await
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("erreur : {error}");
            ExitCode::from(3)
        }
    }
}
