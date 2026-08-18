use csa::cli::{Cli, USAGE};
use csa::error::ManagerError;
use csa::manager::{OfflineArtifactProvider, doctor, exec, install, prepare, status, uninstall};
use csa::process::RealProcessRunner;
use csa::state::SystemClock;
use serde::Serialize;
use std::io::{self, Write};

fn main() {
    std::process::exit(run());
}

fn run() -> i32 {
    let runner = RealProcessRunner;
    if is_current_process_shim() {
        return match forward_current_shim(std::env::args_os().skip(1).collect(), &runner) {
            Ok(exit_code) => exit_code,
            Err(error) => print_error(&error),
        };
    }
    let cli = match Cli::parse(std::env::args_os().skip(1)) {
        Ok(cli) => cli,
        Err(error) => return print_error(&error),
    };
    let result = match cli {
        Cli::Doctor(options) => doctor(options, &runner).and_then(print_json),
        Cli::Install(options) => std::env::current_exe()
            .map_err(|error| ManagerError::io("resolve manager executable", error))
            .and_then(|source| {
                install(
                    options,
                    &runner,
                    &SystemClock,
                    &OfflineArtifactProvider,
                    &source,
                )
            })
            .and_then(print_json),
        Cli::Uninstall { manager_root } => uninstall(manager_root).and_then(print_json),
        Cli::Prepare(options) => {
            prepare(options, &runner, &SystemClock, &OfflineArtifactProvider).and_then(print_json)
        }
        Cli::Plug { manager_root } => std::env::current_exe()
            .map_err(|error| ManagerError::io("resolve manager executable", error))
            .and_then(|source| plug(manager_root, &runner, &SystemClock, &source))
            .and_then(print_json),
        Cli::Unplug { manager_root } => unplug(manager_root).and_then(print_json),
        Cli::Status { manager_root } => status(manager_root, &runner).and_then(print_json),
        Cli::Purge { manager_root } => purge(manager_root).and_then(print_json),
        Cli::Exec(options) => {
            return match exec(options, &runner) {
                Ok(outcome) => outcome.exit_code,
                Err(error) => print_error(&error),
            };
        }
        Cli::Help => {
            println!("{USAGE}");
            return 0;
        }
        Cli::Version => {
            println!("csa {}", env!("CARGO_PKG_VERSION"));
            return 0;
        }
    };
    match result {
        Ok(()) => 0,
        Err(error) => print_error(&error),
    }
}

fn print_json(value: impl Serialize) -> csa::error::Result<()> {
    let stdout = io::stdout();
    let mut lock = stdout.lock();
    serde_json::to_writer_pretty(&mut lock, &value).map_err(|error| {
        ManagerError::new("output_error", format!("serialize JSON output: {error}"))
    })?;
    writeln!(lock).map_err(|error| ManagerError::io("write JSON output", error))
}

fn print_error(error: &ManagerError) -> i32 {
    let value = serde_json::json!({
        "schema": 1,
        "error": {
            "code": error.code,
            "message": error.message,
        }
    });
    let stderr = io::stderr();
    let mut lock = stderr.lock();
    let _ = serde_json::to_writer_pretty(&mut lock, &value);
    let _ = writeln!(lock);
    2
}
use csa::activation::{forward_current_shim, is_current_process_shim, plug, purge, unplug};
