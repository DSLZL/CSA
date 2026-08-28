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
            .and_then(|mut report| {
                activate_user_path(&mut report.activation, &runner)?;
                print_json(&report)?;
                Ok(())
            }),
        Cli::Uninstall { manager_root } => uninstall(manager_root).and_then(|report| {
            remove_user_path(&report.manager_root, &runner)?;
            print_json(report)
        }),
        Cli::Prepare(options) => {
            prepare(options, &runner, &SystemClock, &OfflineArtifactProvider).and_then(print_json)
        }
        Cli::Plug { manager_root } => std::env::current_exe()
            .map_err(|error| ManagerError::io("resolve manager executable", error))
            .and_then(|source| plug(manager_root, &runner, &SystemClock, &source))
            .and_then(|mut report| {
                activate_user_path(&mut report, &runner)?;
                print_json(&report)?;
                Ok(())
            }),
        Cli::Unplug { manager_root } => unplug(manager_root).and_then(print_json),
        Cli::Status { manager_root } => status(manager_root, &runner).and_then(print_json),
        Cli::Purge { manager_root } => purge(manager_root).and_then(|report| {
            remove_user_path(&report.manager_root, &runner)?;
            print_json(report)
        }),
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

fn activate_user_path(
    report: &mut csa::activation::PlugReport,
    runner: &RealProcessRunner,
) -> csa::error::Result<()> {
    #[cfg(windows)]
    {
        let manager_root = report
            .activation
            .managed_bin
            .parent()
            .ok_or_else(|| {
                ManagerError::new("unsafe_manager_root", "managed bin has no manager root")
            })?
            .to_path_buf();
        let user_path = match csa::activation::prioritize_windows_user_path(
            &report.activation,
            runner,
        ) {
            Ok(user_path) => user_path,
            Err(install_error) => {
                let path_rollback = csa::activation::remove_windows_user_path(
                    &report.activation.managed_bin,
                    runner,
                )
                .err();
                let shim_rollback = unplug(Some(manager_root)).err();
                if let Some(rollback_error) = path_rollback.or(shim_rollback) {
                    return Err(ManagerError::new(
                        "path_activation_rollback_failed",
                        format!(
                            "PATH activation failed: {install_error}; rollback failed: {rollback_error}"
                        ),
                    ));
                }
                return Err(install_error);
            }
        };
        report.user_path = Some(user_path);
    }
    #[cfg(not(windows))]
    let _ = (report, runner);
    Ok(())
}

fn remove_user_path(
    manager_root: &std::path::Path,
    runner: &RealProcessRunner,
) -> csa::error::Result<()> {
    #[cfg(windows)]
    csa::activation::remove_windows_user_path(&manager_root.join("bin"), runner)?;
    #[cfg(not(windows))]
    let _ = (manager_root, runner);
    Ok(())
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
