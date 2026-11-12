//! ha2lux notification area icon.
#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::process::ExitCode;

use ha2lux_i18n::t;
use ha2lux_tray::app::{AppOptions, run};
use tracing_subscriber::EnvFilter;

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
    // Language from the configuration file, before displaying anything.
    ha2lux_i18n::config::init_from_config();
    match std::env::args().nth(1).as_deref() {
        None => {}
        Some("--help" | "-h") => {
            println!("{}", t!("tray-usage"));
            return ExitCode::SUCCESS;
        }
        Some("--version" | "-V") => {
            println!("ha2lux-tray {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Some(other) => {
            eprintln!(
                "{}\n{}",
                t!("tray-unexpected-argument", arg = other),
                t!("tray-usage")
            );
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
            eprintln!("{}", t!("tray-error", error = error.to_string()));
            ExitCode::from(3)
        }
    }
}
