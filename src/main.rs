mod ui;

use crate::ui::{
    INSTALLATION_CANCELLED, InstallProgress, Operation, OutputMode, output_mode,
    pick_install_candidate, streams_are_interactive, write_doctor_error, write_doctor_report,
    write_error, write_install_cancelled, write_report,
};
use csa::cli::{Cli, Invocation, json_requested, usage};
use csa::error::ManagerError;
use csa::i18n::Language;
use csa::manager::{
    InstallEvent, InstallOptions, OfflineArtifactProvider, doctor, exec, install_with_progress,
    install_with_progress_and_selector, prepare, status, uninstall,
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
        Cli::Install { .. } => Operation::Install,
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
        Cli::Install { options, yes } => {
            let picker = install_picker_requested(mode, yes, &options, streams_are_interactive());
            let installed = if picker {
                match &options {
                    InstallOptions::Online(options) => {
                        status(options.manager_root.clone(), &runner)
                            .ok()
                            .filter(|report| report.status == "prepared")
                            .and_then(|report| report.state.map(|state| state.compat_id))
                    }
                    InstallOptions::Local(_) => None,
                }
            } else {
                None
            };
            let mut progress = InstallProgress::new(mode, picker);
            let mut selector = |candidates: &[csa::online::InstallCandidate]| {
                pick_install_candidate(candidates, installed.as_deref())
            };
            let result = std::env::current_exe()
                .map_err(|error| ManagerError::io("resolve manager executable", error))
                .and_then(|source| {
                    if picker {
                        install_with_progress_and_selector(
                            options,
                            &runner,
                            &SystemClock,
                            &OfflineArtifactProvider,
                            &source,
                            &mut |event| progress.event(event),
                            Some(&mut selector),
                        )
                    } else {
                        install_with_progress(
                            options,
                            &runner,
                            &SystemClock,
                            &OfflineArtifactProvider,
                            &source,
                            &mut |event| progress.event(event),
                        )
                    }
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
            println!("{}", usage(Language::detected()));
            return 0;
        }
        Cli::Version => {
            println!("csa {}", env!("CARGO_PKG_VERSION"));
            return 0;
        }
    };
    match result {
        Ok(()) => 0,
        Err(error) if error.code == INSTALLATION_CANCELLED => write_install_cancelled(),
        Err(error) => write_error(mode, operation, &error),
    }
}

fn install_picker_requested(
    mode: OutputMode,
    yes: bool,
    options: &InstallOptions,
    streams_interactive: bool,
) -> bool {
    mode == OutputMode::Human
        && !yes
        && streams_interactive
        && matches!(options, InstallOptions::Online(options) if options.compat.is_none())
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

#[cfg(test)]
mod tests {
    use super::install_picker_requested;
    use crate::ui::OutputMode;
    use csa::manager::{InstallOptions, OnlineInstallOptions};

    fn online(compat: Option<&str>) -> InstallOptions {
        InstallOptions::Online(OnlineInstallOptions {
            manager_root: None,
            official: None,
            official_native: None,
            compat: compat.map(str::to_owned),
        })
    }

    #[test]
    fn install_picker_requires_bare_human_three_stream_tty_mode() {
        assert!(install_picker_requested(
            OutputMode::Human,
            false,
            &online(None),
            true
        ));
        assert!(!install_picker_requested(
            OutputMode::Human,
            true,
            &online(None),
            true
        ));
        assert!(!install_picker_requested(
            OutputMode::Json,
            false,
            &online(None),
            true
        ));
        assert!(!install_picker_requested(
            OutputMode::Human,
            false,
            &online(Some("rust-v0.150.1-native-join-p9")),
            true
        ));
        assert!(!install_picker_requested(
            OutputMode::Human,
            false,
            &online(None),
            false
        ));
    }
}
