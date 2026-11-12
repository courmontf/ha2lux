//! `ha2lux` binary.
#![forbid(unsafe_code)]

use std::process::ExitCode;
use std::time::Duration;

use ha2lux_cli::cli::{Command, Parsed, parse};
use ha2lux_cli::client::{CallError, ZbusDaemon};
use ha2lux_cli::run::{Outcome, execute};

/// Maximum delay of a call to the daemon.
const TIMEOUT: Duration = Duration::from_secs(10);

fn emit(outcome: &Outcome) -> ExitCode {
    // A closed pipe (e.g. `| head`) must not make the command fail.
    use std::io::Write;
    let _ = std::io::stdout().write_all(outcome.stdout.as_bytes());
    let _ = std::io::stderr().write_all(outcome.stderr.as_bytes());
    ExitCode::from(outcome.code)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    // Language from the configuration file, before displaying anything (help
    // and `doctor` work without the daemon).
    ha2lux_i18n::config::init_from_config();
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
    // `doctor` is standalone: it works without a daemon.
    if matches!(cli.command, Command::Doctor { .. }) {
        // The client is not used; we avoid connecting to the bus.
        let outcome = execute(&cli, &NoDaemon, TIMEOUT).await;
        return emit(&outcome);
    }
    let daemon = match ZbusDaemon::connect_session().await {
        Ok(daemon) => daemon,
        Err(error) => {
            return emit(&Outcome {
                stderr: ha2lux_i18n::t!("cli-error", message = error.message()) + "\n",
                code: error.exit_code(),
                ..Outcome::default()
            });
        }
    };
    emit(&execute(&cli, &daemon, TIMEOUT).await)
}

/// Dummy daemon for `doctor` (never called).
struct NoDaemon;

impl ha2lux_cli::client::Daemon for NoDaemon {
    async fn call(
        &self,
        _call: ha2lux_cli::client::Call,
    ) -> Result<ha2lux_cli::client::Answer, CallError> {
        Err(CallError::Other(
            "no daemon needed for this command".to_owned(),
        ))
    }
}
