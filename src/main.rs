mod ui;

use crate::ui::{
    InstallProgress, Operation, OutputMode, output_mode, write_doctor_error, write_doctor_report,
    write_error, write_report,
};
use csa::cli::{Cli, Invocation, USAGE, json_requested};
use csa::error::ManagerError;
use csa::manager::{
    InstallEvent, OfflineArtifactProvider, doctor, exec, install_with_progress, prepare, status,
    uninstall,
};
use csa::process::RealProcessRunner;
use csa::state::SystemClock;

fn main() {
    std::process::exit(run());
}

fn run() -> i32 {
    let runner = RealProcessRunner;
    if is_current_process_shim() {
        return match forward_current_shim(std::env::args_os().skip(1).collect(), &runner) {
            Ok(exit_code) => exit_code,
            Err(error) => write_error(output_mode(false), Operation::Shim, &error),
        };
    }
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let parse_mode = output_mode(json_requested(&args));
    let invocation = match Invocation::parse(args) {
        Ok(invocation) => invocation,
        Err(error) => return write_error(parse_mode, Operation::Parse, &error),
    };
    let mode = output_mode(invocation.explicit_json);
    let operation = match &invocation.command {
        Cli::Doctor(_) => Operation::Doctor,
        Cli::Install(_) => Operation::Install,
        Cli::Uninstall { .. } => Operation::Uninstall,
        Cli::Prepare(_) => Operation::Prepare,
        Cli::Plug { .. } => Operation::Plug,
        Cli::Unplug { .. } => Operation::Unplug,
        Cli::Status { .. } => Operation::Status,
        Cli::Purge { .. } => Operation::Purge,
        Cli::Exec(_) => Operation::Exec,
        Cli::Help | Cli::Version => Operation::Parse,
    };
    let result = match invocation.command {
        Cli::Doctor(options) => {
            return match run_doctor_command(options, mode, &runner) {
                Ok(exit_code) => exit_code,
                Err(error) => write_doctor_error(mode, &error),
            };
        }
        Cli::Install(options) => {
            let mut progress = InstallProgress::new(mode);
            let result = std::env::current_exe()
                .map_err(|error| ManagerError::io("resolve manager executable", error))
                .and_then(|source| {
                    install_with_progress(
                        options,
                        &runner,
                        &SystemClock,
                        &OfflineArtifactProvider,
                        &source,
                        &mut |event| progress.event(event),
                    )
                })
                .and_then(|mut report| {
                    activate_user_path(&mut report.activation, &runner, &mut |event| {
                        progress.event(event)
                    })?;
                    write_report(mode, &report)?;
                    Ok(())
                });
            progress.finish();
            result
        }
        Cli::Uninstall { manager_root } => uninstall(manager_root).and_then(|report| {
            remove_user_path(&report.manager_root, &runner)?;
            write_report(mode, &report)
        }),
        Cli::Prepare(options) => prepare(options, &runner, &SystemClock, &OfflineArtifactProvider)
            .and_then(|report| write_report(mode, &report)),
        Cli::Plug { manager_root } => std::env::current_exe()
            .map_err(|error| ManagerError::io("resolve manager executable", error))
            .and_then(|source| plug(manager_root, &runner, &SystemClock, &source))
            .and_then(|mut report| {
                activate_user_path(&mut report, &runner, &mut |_| {})?;
                write_report(mode, &report)?;
                Ok(())
            }),
        Cli::Unplug { manager_root } => {
            unplug(manager_root).and_then(|report| write_report(mode, &report))
        }
        Cli::Status { manager_root } => {
            status(manager_root, &runner).and_then(|report| write_report(mode, &report))
        }
        Cli::Purge { manager_root } => purge(manager_root).and_then(|report| {
            remove_user_path(&report.manager_root, &runner)?;
            write_report(mode, &report)
        }),
        Cli::Exec(options) => {
            return match exec(options, &runner) {
                Ok(outcome) => outcome.exit_code,
                Err(error) => write_error(mode, Operation::Exec, &error),
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
        Err(error) => write_error(mode, operation, &error),
    }
}

fn run_doctor_command(
    options: csa::manager::DoctorOptions,
    mode: OutputMode,
    runner: &RealProcessRunner,
) -> csa::error::Result<i32> {
    let report = doctor(options, runner)?;
    let status_report = if mode == OutputMode::Human {
        Some(status(Some(report.manager_root.clone()), runner)?)
    } else {
        None
    };
    write_doctor_report(mode, &report, status_report.as_ref())
}

fn activate_user_path(
    report: &mut csa::activation::PlugReport,
    runner: &RealProcessRunner,
    progress: &mut dyn FnMut(InstallEvent),
) -> csa::error::Result<()> {
    progress(InstallEvent::PrioritizingCommand);
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
                progress(InstallEvent::RollingBack);
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
    progress(InstallEvent::Completed);
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

use csa::activation::{forward_current_shim, is_current_process_shim, plug, purge, unplug};
