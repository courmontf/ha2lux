//! Binaire `ha2lux`.
#![forbid(unsafe_code)]

use std::process::ExitCode;
use std::time::Duration;

use ha2lux_cli::cli::{Command, Parsed, parse};
use ha2lux_cli::client::{CallError, ZbusDaemon};
use ha2lux_cli::run::{Outcome, execute};

/// Délai maximal d'un appel au démon.
const TIMEOUT: Duration = Duration::from_secs(10);

fn emit(outcome: &Outcome) -> ExitCode {
    // Un tube fermé (ex. `| head`) ne doit pas faire échouer la commande.
    use std::io::Write;
    let _ = std::io::stdout().write_all(outcome.stdout.as_bytes());
    let _ = std::io::stderr().write_all(outcome.stderr.as_bytes());
    ExitCode::from(outcome.code)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let cli = match parse(std::env::args().skip(1)) {
        Ok(Parsed::Output(text)) => {
            print!("{text}");
            return ExitCode::SUCCESS;
        }
        Ok(Parsed::Run(cli)) => cli,
        Err(error) => {
            eprint!("{}", error.0);
            return ExitCode::from(1);
        }
    };
    // `doctor` est autonome : il fonctionne sans démon.
    if matches!(cli.command, Command::Doctor { .. }) {
        // Le client n'est pas utilisé ; on évite de se connecter au bus.
        let outcome = execute(&cli, &NoDaemon, TIMEOUT).await;
        return emit(&outcome);
    }
    let daemon = match ZbusDaemon::connect_session().await {
        Ok(daemon) => daemon,
        Err(error) => {
            return emit(&Outcome {
                stderr: format!("erreur : {}\n", error.message()),
                code: error.exit_code(),
                ..Outcome::default()
            });
        }
    };
    emit(&execute(&cli, &daemon, TIMEOUT).await)
}

/// Démon factice pour `doctor` (jamais appelé).
struct NoDaemon;

impl ha2lux_cli::client::Daemon for NoDaemon {
    async fn call(
        &self,
        _call: ha2lux_cli::client::Call,
    ) -> Result<ha2lux_cli::client::Answer, CallError> {
        Err(CallError::Other(
            "aucun démon nécessaire pour cette commande".to_owned(),
        ))
    }
}
