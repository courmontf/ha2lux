//! Binaire `ha2luxd` : démon utilisateur ha2lux.
//!
//! Il remplace proprement une instance déjà en cours (voir `service`), écoute
//! sur le bus de session et s'arrête sur SIGTERM/SIGINT ou sur `Quit`.
#![forbid(unsafe_code)]

use std::process::ExitCode;
use std::sync::Arc;

use ha2lux_i18n::t;
use ha2luxd::clock::SystemClock;
use ha2luxd::ha::{HaDeps, RealSources, SecretTokens};
use ha2luxd::hardware::SystemHardware;
use ha2luxd::paths::Paths;
use ha2luxd::service::{ServiceOptions, run};
use tracing_subscriber::EnvFilter;

fn init_logging() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr);
    // Sous systemd, journald ajoute lui-même l'horodatage et n'aime pas les couleurs.
    if std::env::var_os("JOURNAL_STREAM").is_some() {
        builder.without_time().with_ansi(false).init();
    } else {
        builder.init();
    }
}

async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let (Ok(mut term), Ok(mut int)) = (
        signal(SignalKind::terminate()),
        signal(SignalKind::interrupt()),
    ) else {
        // Sans gestionnaire de signaux, on attend indéfiniment : l'arrêt passera par `Quit`.
        std::future::pending::<()>().await;
        return;
    };
    tokio::select! {
        _ = term.recv() => {}
        _ = int.recv() => {}
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    // Langue du fichier de configuration, avant d'afficher quoi que ce soit.
    ha2lux_i18n::config::init_from_config();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => {}
        Some("--help" | "-h") => {
            println!("{}", t!("daemon-usage"));
            return ExitCode::SUCCESS;
        }
        Some("--version" | "-V") => {
            println!("ha2luxd {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Some(other) => {
            eprintln!(
                "{}\n{}",
                t!("daemon-unexpected-argument", arg = other),
                t!("daemon-usage")
            );
            return ExitCode::from(1);
        }
    }
    init_logging();
    let paths = match Paths::system() {
        Ok(paths) => paths,
        Err(error) => {
            eprintln!("{}", t!("daemon-error", error = error.to_string()));
            return ExitCode::from(3);
        }
    };
    tracing::info!(version = env!("CARGO_PKG_VERSION"), config = %paths.config_file.display(), "starting");
    let keyring = match ha2lux_secrets::ServiceSecretStore::connect_session().await {
        Ok(store) => Some(store),
        Err(error) => {
            tracing::info!(%error, "no keyring: falling back to the secrets file");
            None
        }
    };
    let store = ha2lux_secrets::ChainStore::new(
        keyring,
        ha2lux_secrets::FileSecretStore::new(paths.secrets_file.clone()),
    );
    let ha = HaDeps {
        tokens: Arc::new(SecretTokens(Arc::new(store))),
        sources: Arc::new(RealSources),
    };
    let mut options = ServiceOptions::production(
        paths,
        Arc::new(SystemHardware::new()),
        Arc::new(SystemClock::new()),
    );
    options.ha = Some(ha);
    match run(options, shutdown_signal()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(%error, "stopping on error");
            eprintln!("{}", t!("daemon-error", error = error.to_string()));
            ExitCode::from(3)
        }
    }
}
