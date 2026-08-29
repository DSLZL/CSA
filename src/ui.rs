use csa::activation::{CommandResolution, PlugReport, PurgeReport, UnplugReport};
use csa::error::{ManagerError, Result};
use csa::manager::{
    DoctorReport, InstallEvent, InstallReport, PrepareReport, StatusReport, UninstallReport,
};
use serde::Serialize;
use std::io::{self, IsTerminal, Write};
use std::time::{Duration, Instant};

const PROGRESS_REFRESH: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OutputMode {
    Human,
    Json,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Operation {
    Parse,
    Shim,
    Doctor,
    Install,
    Uninstall,
    Prepare,
    Plug,
    Unplug,
    Status,
    Purge,
    Exec,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CheckSeverity {
    Pass,
    Warn,
    Fail,
}

struct DoctorCheck {
    severity: CheckSeverity,
    name: &'static str,
    detail: String,
    impact: Option<&'static str>,
    action: Option<&'static str>,
}

struct DoctorAssessment {
    checks: Vec<DoctorCheck>,
    incomplete: bool,
}

struct StatusView {
    conclusion: &'static str,
    installed: bool,
    active: bool,
    healthy: bool,
}

pub(crate) fn output_mode(explicit_json: bool) -> OutputMode {
    resolve_output_mode(explicit_json, io::stdout().is_terminal())
}

pub(crate) struct InstallProgress {
    mode: OutputMode,
    download_started: Option<Instant>,
    last_draw: Option<Instant>,
    line_active: bool,
}

impl InstallProgress {
    pub(crate) fn new(mode: OutputMode) -> Self {
        Self {
            mode,
            download_started: None,
            last_draw: None,
            line_active: false,
        }
    }

    pub(crate) fn event(&mut self, event: InstallEvent) {
        let stderr = io::stderr();
        let _ = self.write_event_to(&mut stderr.lock(), event, Instant::now());
    }

    pub(crate) fn finish(&mut self) {
        let stderr = io::stderr();
        let _ = self.finish_to(&mut stderr.lock());
    }

    fn write_event_to(
        &mut self,
        writer: &mut dyn Write,
        event: InstallEvent,
        now: Instant,
    ) -> io::Result<()> {
        if self.mode == OutputMode::Json {
            return Ok(());
        }
        if let InstallEvent::ArtifactProgress {
            downloaded_bytes,
            total_bytes,
        } = event
        {
            if self.last_draw.is_some_and(|last| {
                now.saturating_duration_since(last) < PROGRESS_REFRESH
                    && downloaded_bytes < total_bytes
            }) {
                return Ok(());
            }
            let started = *self.download_started.get_or_insert(now);
            self.last_draw = Some(now);
            self.line_active = true;
            let total_bytes = total_bytes.max(1);
            let percent = downloaded_bytes.saturating_mul(100) / total_bytes;
            let mib = 1024.0 * 1024.0;
            let rate = downloaded_bytes as f64
                / mib
                / now
                    .saturating_duration_since(started)
                    .as_secs_f64()
                    .max(0.001);
            write!(
                writer,
                "\rDownloading patched Codex: {percent:>3}% ({:.1}/{:.1} MiB, {rate:.1} MiB/s)    ",
                downloaded_bytes as f64 / mib,
                total_bytes as f64 / mib,
            )?;
            return writer.flush();
        }

        self.finish_to(writer)?;
        match event {
            InstallEvent::DetectingOfficial => writeln!(writer, "Detecting official Codex..."),
            InstallEvent::DiscoveringCompatibility => {
                writeln!(writer, "Discovering compatible releases...")
            }
            InstallEvent::SelectedCompatibility { compat_id } => {
                writeln!(writer, "Selected compatibility: {compat_id}")
            }
            InstallEvent::VerifyingArtifact => {
                writeln!(writer, "Verifying downloaded artifact...")
            }
            InstallEvent::Preparing => writeln!(writer, "Preparing patched Codex..."),
            InstallEvent::Activating => writeln!(writer, "Activating patched Codex..."),
            InstallEvent::Activated => writeln!(writer, "Patched Codex shim activated."),
            InstallEvent::RollingBack => writeln!(writer, "Rolling back activation..."),
            InstallEvent::PrioritizingCommand => {
                writeln!(writer, "Prioritizing and verifying the codex command...")
            }
            InstallEvent::Completed => Ok(()),
            InstallEvent::ArtifactProgress { .. } => unreachable!(),
        }?;
        writer.flush()
    }

    fn finish_to(&mut self, writer: &mut dyn Write) -> io::Result<()> {
        if self.mode == OutputMode::Human && self.line_active {
            self.line_active = false;
            writeln!(writer)?;
            writer.flush()?;
        }
        Ok(())
    }
}

fn resolve_output_mode(explicit_json: bool, stdout_is_terminal: bool) -> OutputMode {
    if explicit_json || !stdout_is_terminal {
        OutputMode::Json
    } else {
        OutputMode::Human
    }
}

pub(crate) trait HumanReport {
    fn write_human(&self, writer: &mut dyn Write) -> io::Result<()>;
}

pub(crate) fn write_report<T: HumanReport + Serialize>(mode: OutputMode, report: &T) -> Result<()> {
    let stdout = io::stdout();
    write_report_to(&mut stdout.lock(), mode, report)
}

pub(crate) fn write_doctor_report(
    mode: OutputMode,
    report: &DoctorReport,
    status: Option<&StatusReport>,
) -> Result<i32> {
    let stdout = io::stdout();
    write_doctor_report_to(&mut stdout.lock(), mode, report, status)
}

pub(crate) fn write_doctor_error(mode: OutputMode, error: &ManagerError) -> i32 {
    let stderr = io::stderr();
    write_doctor_error_to(&mut stderr.lock(), mode, error).unwrap_or(2)
}

fn write_report_to<T: HumanReport + Serialize>(
    writer: &mut dyn Write,
    mode: OutputMode,
    report: &T,
) -> Result<()> {
    match mode {
        OutputMode::Json => write_json(writer, report),
        OutputMode::Human => report
            .write_human(writer)
            .map_err(|error| ManagerError::io("write human output", error)),
    }
}

fn write_doctor_report_to(
    writer: &mut dyn Write,
    mode: OutputMode,
    report: &DoctorReport,
    status: Option<&StatusReport>,
) -> Result<i32> {
    if mode == OutputMode::Json {
        write_json(writer, report)?;
        return Ok(0);
    }
    let status = status.ok_or_else(|| {
        ManagerError::new(
            "doctor_status_unavailable",
            "status data is required for Human doctor output",
        )
    })?;
    let assessment = doctor_assessment(report, status);
    write_doctor_assessment(writer, &assessment)
        .map_err(|error| ManagerError::io("write human doctor output", error))?;
    Ok(assessment.exit_code())
}

fn write_doctor_error_to(
    writer: &mut dyn Write,
    mode: OutputMode,
    error: &ManagerError,
) -> Result<i32> {
    let diagnosed = is_diagnosed_doctor_error(error.code);
    if mode == OutputMode::Json || !diagnosed {
        write_error_to(writer, mode, Operation::Doctor, error)?;
    } else {
        writeln!(writer, "CSA doctor")
            .and_then(|()| writeln!(writer, "FAIL Official Codex: {}", error.message))
            .and_then(|()| {
                writeln!(
                    writer,
                    "     Impact: CSA cannot safely verify the official Codex installation."
                )
            })
            .and_then(|()| {
                writeln!(
                    writer,
                    "     Safety: the official Codex installation was not modified."
                )
            })
            .and_then(|()| {
                writeln!(
                    writer,
                    "     Next: {}",
                    recovery_hint(Operation::Doctor, error.code)
                )
            })
            .and_then(|()| writeln!(writer, "Summary: 0 passed, 0 warnings, 1 failed"))
            .map_err(|error| ManagerError::io("write human doctor error", error))?;
    }
    Ok(if diagnosed { 1 } else { 2 })
}

pub(crate) fn write_error(mode: OutputMode, operation: Operation, error: &ManagerError) -> i32 {
    let stderr = io::stderr();
    let mut lock = stderr.lock();
    let _ = write_error_to(&mut lock, mode, operation, error);
    2
}

fn write_error_to(
    writer: &mut dyn Write,
    mode: OutputMode,
    operation: Operation,
    error: &ManagerError,
) -> Result<()> {
    match mode {
        OutputMode::Json => {
            let envelope = serde_json::json!({
                "schema": 1,
                "error": {
                    "code": error.code,
                    "message": error.message,
                }
            });
            write_json(writer, &envelope)
        }
        OutputMode::Human => write_human_error(writer, operation, error)
            .map_err(|error| ManagerError::io("write human error", error)),
    }
}

fn write_human_error(
    writer: &mut dyn Write,
    operation: Operation,
    error: &ManagerError,
) -> io::Result<()> {
    writeln!(writer, "ERROR [{}] {}", error.code, error.message)?;
    writeln!(
        writer,
        "Safety: the official Codex installation was not modified."
    )?;
    if operation == Operation::Install {
        let state = if error.code == "output_error" {
            "Installation may have completed; run `csa status --json` to confirm the active state."
        } else if matches!(
            error.code,
            "install_rollback_failed" | "path_activation_rollback_failed"
        ) {
            "Activation rollback did not complete; inspect CSA state before retrying."
        } else {
            "The failed install did not keep a new CSA activation."
        };
        writeln!(writer, "State: {state}")?;
    }
    writeln!(writer, "Recovery: {}", recovery_hint(operation, error.code))
}

fn write_json(writer: &mut dyn Write, value: &impl Serialize) -> Result<()> {
    serde_json::to_writer_pretty(&mut *writer, value).map_err(|error| {
        ManagerError::new("output_error", format!("serialize JSON output: {error}"))
    })?;
    writeln!(writer).map_err(|error| ManagerError::io("write JSON output", error))
}

fn recovery_hint(operation: Operation, code: &str) -> &'static str {
    match code {
        "invalid_cli" => "Run `csa --help` and retry with valid options.",
        "official_not_found" => {
            "Install official `@openai/codex` or put its launcher on PATH, then retry."
        }
        "official_runtime_incomplete"
        | "official_runtime_ambiguous"
        | "official_version_mismatch" => {
            "Reinstall the official `@openai/codex` package, then retry."
        }
        "official_in_manager_root" => {
            "Use an official Codex installation outside the CSA manager root."
        }
        "ambiguous_compatibility_revision" => {
            "Retry with `csa install --compat <compat-id>` to select an exact release."
        }
        "no_installable_compatibility_releases" => {
            "Install an official Codex version with an exact CSA compatibility release, then retry."
        }
        "not_prepared" | "state_upgrade_required" => "Run `csa install` again.",
        "unsupported_official_version" | "unsupported_build_target" => {
            "Choose a release that exactly matches the installed official Codex and target."
        }
        "github_api_forbidden" | "network_error" => {
            "Check the network or proxy, then retry the command."
        }
        "path_activation_failed" | "path_activation_rollback_failed" => {
            "Open a new terminal and run `csa doctor --json` before retrying."
        }
        "install_rollback_failed" => "Run `csa doctor --json` before retrying installation.",
        _ if matches!(operation, Operation::Doctor | Operation::Status) => {
            "Retry with `--json` for machine-readable diagnostics."
        }
        _ => "Run `csa doctor --json` for diagnostics, then retry.",
    }
}

fn is_diagnosed_doctor_error(code: &str) -> bool {
    matches!(
        code,
        "official_not_found"
            | "official_runtime_incomplete"
            | "official_runtime_ambiguous"
            | "official_version_mismatch"
            | "official_in_manager_root"
    )
}

fn status_view(report: &StatusReport) -> StatusView {
    let active = report.activation.status == "plugged" && report.activation.effective;
    let (conclusion, healthy) = match (
        report.status,
        report.activation.status,
        report.activation.effective,
    ) {
        ("unprepared", "fallback", _) => ("not installed, activation fallback", false),
        ("unprepared", _, _) => ("not installed", true),
        ("prepared", "unplugged", _) => ("installed, inactive", true),
        ("prepared", "plugged", true) => ("active and healthy", true),
        ("prepared", "plugged", false) => ("installed, activation ineffective", false),
        ("prepared", "fallback", _) => ("installed, activation fallback", false),
        ("invalidated", _, _) => ("installed state invalidated", false),
        _ => ("unknown state", false),
    };
    StatusView {
        conclusion,
        installed: report.state.is_some(),
        active,
        healthy,
    }
}

fn doctor_assessment(report: &DoctorReport, status: &StatusReport) -> DoctorAssessment {
    let mut checks = vec![
        check(
            CheckSeverity::Pass,
            "Official Codex",
            format!(
                "{} at {}",
                report.official.version,
                report.official.executable.path.display()
            ),
            None,
            None,
        ),
        check(
            CheckSeverity::Pass,
            "Manager target",
            report.manager_build_target.to_owned(),
            None,
            None,
        ),
    ];

    if let Some(compatibility) = &report.compatibility {
        checks.push(if compatibility.exact_official_version {
            check(
                CheckSeverity::Pass,
                "Compatibility version",
                format!("{} matches official Codex", compatibility.codex_version),
                None,
                None,
            )
        } else {
            check(
                CheckSeverity::Fail,
                "Compatibility version",
                format!(
                    "manifest requires {}, official Codex is {}",
                    compatibility.codex_version, report.official.version
                ),
                Some("This payload cannot safely patch the installed official Codex."),
                Some("Choose a compatibility release matching the official Codex version."),
            )
        });
        checks.push(if compatibility.supported_build_target {
            check(
                CheckSeverity::Pass,
                "Compatibility target",
                compatibility.build_target.clone(),
                None,
                None,
            )
        } else {
            check(
                CheckSeverity::Fail,
                "Compatibility target",
                format!(
                    "manifest targets {}, manager targets {}",
                    compatibility.build_target, report.manager_build_target
                ),
                Some("This payload cannot run on the current CSA build target."),
                Some("Choose a compatibility release matching the manager target."),
            )
        });
    }

    let mut incomplete = false;
    match (status.status, status.state.as_ref()) {
        ("unprepared", None) => checks.push(check(
            CheckSeverity::Warn,
            "Prepared state",
            "no patched Codex is installed".to_owned(),
            Some("The official Codex remains available, but CSA is not managing it."),
            Some("Run `csa install`."),
        )),
        ("prepared", Some(state)) => checks.push(check(
            CheckSeverity::Pass,
            "Prepared state",
            format!("{} is verified", state.compat_id),
            None,
            None,
        )),
        ("invalidated", Some(_)) => checks.push(check(
            CheckSeverity::Fail,
            "Prepared state",
            status
                .reason
                .clone()
                .unwrap_or_else(|| "prepared state failed verification".to_owned()),
            Some("CSA will not trust or execute the prepared patched binary."),
            Some("Run `csa install` to replace the invalid state."),
        )),
        _ => {
            incomplete = true;
            checks.push(check(
                CheckSeverity::Fail,
                "Prepared state",
                format!("unrecognized status: {}", status.status),
                Some("CSA could not complete a reliable state assessment."),
                Some("Retry with `csa doctor --json` and inspect the raw report."),
            ));
        }
    }

    match (status.activation.status, status.activation.effective) {
        ("unplugged", _) => checks.push(check(
            CheckSeverity::Warn,
            "Activation",
            "patched Codex is inactive".to_owned(),
            Some("Running `codex` will not use the prepared patched binary."),
            Some(if status.state.is_some() {
                "Run `csa plug`, then open a new terminal."
            } else {
                "Run `csa install`."
            }),
        )),
        ("plugged", true) => checks.push(check(
            CheckSeverity::Pass,
            "Activation",
            "patched Codex is active".to_owned(),
            None,
            None,
        )),
        ("plugged", false) => checks.push(check(
            CheckSeverity::Fail,
            "Activation",
            "the CSA shim exists but is not command-effective".to_owned(),
            Some("Running `codex` selects another installation."),
            Some("Run `csa plug`, open a new terminal, then rerun `csa doctor`."),
        )),
        ("fallback", _) => checks.push(check(
            CheckSeverity::Fail,
            "Activation",
            status
                .activation
                .reason
                .clone()
                .unwrap_or_else(|| "activation entered fallback mode".to_owned()),
            Some("CSA will fall back instead of trusting the patched activation."),
            Some("Run `csa install` to rebuild and reactivate the patched Codex."),
        )),
        _ => {
            incomplete = true;
            checks.push(check(
                CheckSeverity::Fail,
                "Activation",
                format!("unrecognized activation: {}", status.activation.status),
                Some("CSA could not determine whether patched Codex is active."),
                Some("Retry with `csa doctor --json` and inspect the raw report."),
            ));
        }
    }

    let resolved = report
        .command_resolution
        .resolved_codex
        .as_ref()
        .map_or_else(|| "not found".to_owned(), |path| path.display().to_string());
    if matches!(status.activation.status, "plugged" | "fallback")
        && report.command_resolution.resolves_to_managed_shim
    {
        checks.push(check(
            CheckSeverity::Pass,
            "Command precedence",
            format!("codex resolves to {resolved}"),
            None,
            None,
        ));
    } else if status.activation.status == "unplugged" {
        checks.push(check(
            CheckSeverity::Warn,
            "Command precedence",
            format!("codex resolves to {resolved}"),
            Some("The current command does not use CSA's patched Codex."),
            Some(if status.state.is_some() {
                "Run `csa plug`, then open a new terminal."
            } else {
                "Run `csa install`."
            }),
        ));
    } else if matches!(status.activation.status, "plugged" | "fallback") {
        checks.push(check(
            CheckSeverity::Fail,
            "Command precedence",
            format!("codex resolves to {resolved}, not the managed shim"),
            Some("The patched Codex is not selected by the current command."),
            Some("Run `csa plug`, open a new terminal, then rerun `csa doctor`."),
        ));
    } else {
        incomplete = true;
        checks.push(check(
            CheckSeverity::Fail,
            "Command precedence",
            format!("cannot assess command resolution for {resolved}"),
            Some("CSA could not complete a reliable PATH assessment."),
            Some("Retry with `csa doctor --json` and inspect the raw report."),
        ));
    }

    DoctorAssessment { checks, incomplete }
}

fn check(
    severity: CheckSeverity,
    name: &'static str,
    detail: String,
    impact: Option<&'static str>,
    action: Option<&'static str>,
) -> DoctorCheck {
    DoctorCheck {
        severity,
        name,
        detail,
        impact,
        action,
    }
}

impl DoctorAssessment {
    fn exit_code(&self) -> i32 {
        if self.incomplete {
            2
        } else if self
            .checks
            .iter()
            .any(|check| check.severity == CheckSeverity::Fail)
        {
            1
        } else {
            0
        }
    }
}

fn write_doctor_assessment(
    writer: &mut dyn Write,
    assessment: &DoctorAssessment,
) -> io::Result<()> {
    writeln!(writer, "CSA doctor")?;
    let mut totals = [0_u32; 3];
    for check in &assessment.checks {
        let (label, index) = match check.severity {
            CheckSeverity::Pass => ("PASS", 0),
            CheckSeverity::Warn => ("WARN", 1),
            CheckSeverity::Fail => ("FAIL", 2),
        };
        totals[index] += 1;
        writeln!(writer, "{label} {}: {}", check.name, check.detail)?;
        if let Some(impact) = check.impact {
            writeln!(writer, "     Impact: {impact}")?;
        }
        if let Some(action) = check.action {
            writeln!(writer, "     Next: {action}")?;
        }
    }
    writeln!(
        writer,
        "Summary: {} passed, {} warnings, {} failed",
        totals[0], totals[1], totals[2]
    )
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

impl HumanReport for PrepareReport {
    fn write_human(&self, writer: &mut dyn Write) -> io::Result<()> {
        writeln!(writer, "OK Patched Codex prepared")?;
        writeln!(writer, "Compatibility: {}", self.state.compat_id)?;
        writeln!(writer, "Artifact: {}", self.state.artifact_path.display())?;
        writeln!(writer, "Official Codex unchanged: yes")
    }
}

impl HumanReport for InstallReport {
    fn write_human(&self, writer: &mut dyn Write) -> io::Result<()> {
        writeln!(writer, "OK Patched Codex installed and activated")?;
        writeln!(writer, "Compatibility: {}", self.prepare.state.compat_id)?;
        writeln!(
            writer,
            "Managed command: {}",
            self.activation.activation.shim_path.display()
        )?;
        if let Some(user_path) = &self.activation.user_path {
            writeln!(writer, "User PATH: {}", user_path.status)?;
        }
        writeln!(writer, "Next: open a new terminal, then run `codex`.")
    }
}

impl HumanReport for UninstallReport {
    fn write_human(&self, writer: &mut dyn Write) -> io::Result<()> {
        let message = if self.changed {
            "OK CSA uninstalled"
        } else {
            "OK CSA was already uninstalled"
        };
        writeln!(writer, "{message}")?;
        writeln!(writer, "Manager root: {}", self.manager_root.display())
    }
}

impl HumanReport for PlugReport {
    fn write_human(&self, writer: &mut dyn Write) -> io::Result<()> {
        let message = if self.changed {
            "OK Patched Codex activated"
        } else {
            "OK Patched Codex was already active"
        };
        writeln!(writer, "{message}")?;
        writeln!(
            writer,
            "Managed command: {}",
            self.activation.shim_path.display()
        )?;
        if let Some(user_path) = &self.user_path {
            writeln!(writer, "User PATH: {}", user_path.status)?;
        }
        writeln!(writer, "Next: open a new terminal, then run `codex`.")
    }
}

impl HumanReport for UnplugReport {
    fn write_human(&self, writer: &mut dyn Write) -> io::Result<()> {
        let message = if self.changed {
            "OK Patched Codex deactivated"
        } else {
            "OK Patched Codex was already inactive"
        };
        writeln!(writer, "{message}")?;
        writeln!(writer, "Managed bin: {}", self.managed_bin.display())
    }
}

impl HumanReport for PurgeReport {
    fn write_human(&self, writer: &mut dyn Write) -> io::Result<()> {
        let message = if self.changed {
            "OK CSA managed data removed"
        } else {
            "OK CSA had no managed data to remove"
        };
        writeln!(writer, "{message}")?;
        writeln!(writer, "Manager root: {}", self.manager_root.display())
    }
}

impl HumanReport for StatusReport {
    fn write_human(&self, writer: &mut dyn Write) -> io::Result<()> {
        let view = status_view(self);
        writeln!(writer, "CSA status: {}", view.conclusion)?;
        writeln!(writer, "Installed: {}", yes_no(view.installed))?;
        writeln!(writer, "Active: {}", yes_no(view.active))?;
        writeln!(writer, "Healthy: {}", yes_no(view.healthy))?;
        match &self.state {
            Some(state) => {
                writeln!(writer, "Official Codex: {}", state.official.version)?;
                writeln!(writer, "Patched Codex: {}", state.compat_id)?;
            }
            None => {
                writeln!(writer, "Official Codex: not recorded")?;
                writeln!(writer, "Patched Codex: not installed")?;
            }
        }
        write_resolution(writer, &self.activation.command_resolution)?;
        writeln!(writer, "Activation detail: {}", self.activation.status)?;
        if let Some(reason) = &self.reason {
            writeln!(writer, "Reason: {reason}")?;
        }
        Ok(())
    }
}

fn write_resolution(writer: &mut dyn Write, resolution: &CommandResolution) -> io::Result<()> {
    match &resolution.resolved_codex {
        Some(path) => writeln!(writer, "Codex command: {}", path.display()),
        None => writeln!(writer, "Codex command: not found"),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        HumanReport, InstallProgress, Operation, OutputMode, resolve_output_mode,
        write_doctor_error_to, write_doctor_report_to, write_error_to, write_report_to,
    };
    use csa::activation::{ActivationReport, CommandResolution};
    use csa::detect::{FileFingerprint, OfficialCodex};
    use csa::manager::{CompatibilityReport, DoctorReport, InstallEvent, StatusReport};
    use csa::state::PreparedState;
    use serde::Serialize;
    use std::io::{self, Write};
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    #[derive(Serialize)]
    struct Report {
        schema: u32,
        status: &'static str,
    }

    impl HumanReport for Report {
        fn write_human(&self, writer: &mut dyn Write) -> io::Result<()> {
            writeln!(writer, "OK {}", self.status)
        }
    }

    fn official() -> OfficialCodex {
        OfficialCodex {
            executable: FileFingerprint {
                path: PathBuf::from("C:/official/codex.exe"),
                sha256: "a".repeat(64),
                size: 10,
            },
            version: "0.150.1".to_owned(),
            native: None,
            runtime: None,
        }
    }

    fn prepared() -> PreparedState {
        PreparedState {
            schema: 2,
            compat_id: "rust-v0.150.1-native-join-p10".to_owned(),
            manifest_path: PathBuf::from("C:/csa/manifest.toml"),
            build_target: "x86_64-pc-windows-msvc".to_owned(),
            artifact_path: PathBuf::from("C:/csa/patched-codex.exe"),
            artifact_sha256: "b".repeat(64),
            artifact_size: 20,
            official: official(),
            prepared_at_unix_seconds: 1,
        }
    }

    fn status_fixture(
        status: &'static str,
        activation: &'static str,
        effective: bool,
        installed: bool,
    ) -> StatusReport {
        let command_resolution = CommandResolution {
            managed_bin_on_path: effective,
            resolved_codex: Some(if effective {
                PathBuf::from("C:/csa/bin/codex.exe")
            } else {
                PathBuf::from("C:/official/codex.exe")
            }),
            resolves_to_managed_shim: effective,
        };
        StatusReport {
            schema: 1,
            status,
            manager_root: PathBuf::from("C:/csa"),
            state: installed.then(prepared),
            reason: (status == "invalidated").then(|| "artifact_invalidated: changed".to_owned()),
            activation: ActivationReport {
                status: activation,
                effective,
                managed_bin: PathBuf::from("C:/csa/bin"),
                shim_path: PathBuf::from("C:/csa/bin/codex.exe"),
                command_resolution,
                state: None,
                reason: (activation == "fallback")
                    .then(|| "activation_fallback: invalid state".to_owned()),
            },
        }
    }

    fn doctor_fixture(resolves_to_managed_shim: bool) -> DoctorReport {
        DoctorReport {
            schema: 1,
            manager_root: PathBuf::from("C:/csa"),
            manager_build_target: "x86_64-pc-windows-msvc",
            official: official(),
            command_resolution: CommandResolution {
                managed_bin_on_path: resolves_to_managed_shim,
                resolved_codex: Some(if resolves_to_managed_shim {
                    PathBuf::from("C:/csa/bin/codex.exe")
                } else {
                    PathBuf::from("C:/official/codex.exe")
                }),
                resolves_to_managed_shim,
            },
            compatibility: None,
        }
    }

    fn render_status(report: &StatusReport) -> String {
        let mut output = Vec::new();
        report.write_human(&mut output).unwrap();
        String::from_utf8(output).unwrap()
    }

    fn render_doctor(report: &DoctorReport, status: &StatusReport) -> (i32, String) {
        let mut output = Vec::new();
        let exit_code =
            write_doctor_report_to(&mut output, OutputMode::Human, report, Some(status)).unwrap();
        (exit_code, String::from_utf8(output).unwrap())
    }

    #[test]
    fn status_views_cover_all_manager_states_without_changing_json() {
        for (report, expected) in [
            (
                status_fixture("unprepared", "unplugged", false, false),
                "CSA status: not installed\nInstalled: no\nActive: no\nHealthy: yes\n",
            ),
            (
                status_fixture("unprepared", "fallback", false, false),
                "CSA status: not installed, activation fallback\nInstalled: no\nActive: no\nHealthy: no\n",
            ),
            (
                status_fixture("prepared", "unplugged", false, true),
                "CSA status: installed, inactive\nInstalled: yes\nActive: no\nHealthy: yes\n",
            ),
            (
                status_fixture("prepared", "plugged", true, true),
                "CSA status: active and healthy\nInstalled: yes\nActive: yes\nHealthy: yes\n",
            ),
            (
                status_fixture("prepared", "plugged", false, true),
                "CSA status: installed, activation ineffective\nInstalled: yes\nActive: no\nHealthy: no\n",
            ),
            (
                status_fixture("prepared", "fallback", false, true),
                "CSA status: installed, activation fallback\nInstalled: yes\nActive: no\nHealthy: no\n",
            ),
            (
                status_fixture("invalidated", "fallback", false, true),
                "CSA status: installed state invalidated\nInstalled: yes\nActive: no\nHealthy: no\n",
            ),
        ] {
            assert!(render_status(&report).starts_with(expected));
        }

        let report = status_fixture("prepared", "plugged", true, true);
        let mut output = Vec::new();
        write_report_to(&mut output, OutputMode::Json, &report).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
        let mut keys: Vec<_> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "activation",
                "manager_root",
                "reason",
                "schema",
                "state",
                "status"
            ]
        );
    }

    #[test]
    fn doctor_checks_actions_json_and_exit_codes_are_stable() {
        let active = status_fixture("prepared", "plugged", true, true);
        let (exit_code, output) = render_doctor(&doctor_fixture(true), &active);
        assert_eq!(exit_code, 0);
        assert!(output.contains("Summary: 5 passed, 0 warnings, 0 failed"));

        for (doctor, status, exit_code, summary) in [
            (
                doctor_fixture(false),
                status_fixture("unprepared", "unplugged", false, false),
                0,
                "Summary: 2 passed, 3 warnings, 0 failed",
            ),
            (
                doctor_fixture(false),
                status_fixture("prepared", "unplugged", false, true),
                0,
                "Summary: 3 passed, 2 warnings, 0 failed",
            ),
            (
                doctor_fixture(false),
                status_fixture("prepared", "fallback", false, true),
                1,
                "Summary: 3 passed, 0 warnings, 2 failed",
            ),
        ] {
            let (actual_exit, output) = render_doctor(&doctor, &status);
            assert_eq!(actual_exit, exit_code);
            assert!(output.contains(summary));
            let problems = output.matches("WARN ").count() + output.matches("FAIL ").count();
            assert_eq!(output.matches("Impact:").count(), problems);
            assert_eq!(output.matches("Next:").count(), problems);
        }

        let mut incompatible = doctor_fixture(true);
        incompatible.compatibility = Some(CompatibilityReport {
            compat_id: "wrong".to_owned(),
            manifest_path: PathBuf::from("C:/wrong/manifest.toml"),
            codex_version: "0.1.0".to_owned(),
            build_target: "wrong-target".to_owned(),
            exact_official_version: false,
            supported_build_target: false,
        });
        let (exit_code, output) = render_doctor(&incompatible, &active);
        assert_eq!(exit_code, 1);
        assert!(output.contains("Summary: 5 passed, 0 warnings, 2 failed"));

        let fallback = status_fixture("prepared", "fallback", false, true);
        let (exit_code, output) = render_doctor(&doctor_fixture(true), &fallback);
        assert_eq!(exit_code, 1);
        assert!(output.contains("PASS Command precedence:"));
        assert!(output.contains("Summary: 4 passed, 0 warnings, 1 failed"));

        let unknown = status_fixture("future", "future", false, true);
        assert_eq!(render_doctor(&doctor_fixture(false), &unknown).0, 2);

        let mut output = Vec::new();
        assert_eq!(
            write_doctor_report_to(&mut output, OutputMode::Json, &doctor_fixture(true), None,)
                .unwrap(),
            0
        );
        let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
        let mut keys: Vec<_> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "command_resolution",
                "compatibility",
                "manager_build_target",
                "manager_root",
                "official",
                "schema"
            ]
        );

        let diagnosed = csa::error::ManagerError::new("official_not_found", "missing");
        let mut output = Vec::new();
        assert_eq!(
            write_doctor_error_to(&mut output, OutputMode::Human, &diagnosed).unwrap(),
            1
        );
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("FAIL Official Codex: missing"));
        assert!(output.contains("Safety: the official Codex installation was not modified."));
        assert!(output.contains("Summary: 0 passed, 0 warnings, 1 failed"));

        let mut output = Vec::new();
        assert_eq!(
            write_doctor_error_to(&mut output, OutputMode::Json, &diagnosed).unwrap(),
            1
        );
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "{\n  \"error\": {\n    \"code\": \"official_not_found\",\n    \"message\": \"missing\"\n  },\n  \"schema\": 1\n}\n"
        );

        let unknown = csa::error::ManagerError::new("io_error", "unreadable");
        let mut output = Vec::new();
        assert_eq!(
            write_doctor_error_to(&mut output, OutputMode::Human, &unknown).unwrap(),
            2
        );
        assert!(
            String::from_utf8(output)
                .unwrap()
                .contains("ERROR [io_error] unreadable")
        );
    }

    #[test]
    fn output_mode_and_json_contract_are_stable() {
        assert_eq!(resolve_output_mode(false, true), OutputMode::Human);
        assert_eq!(resolve_output_mode(false, false), OutputMode::Json);
        assert_eq!(resolve_output_mode(true, true), OutputMode::Json);

        let mut output = Vec::new();
        write_report_to(
            &mut output,
            OutputMode::Json,
            &Report {
                schema: 1,
                status: "prepared",
            },
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "{\n  \"schema\": 1,\n  \"status\": \"prepared\"\n}\n"
        );

        let mut output = Vec::new();
        write_report_to(
            &mut output,
            OutputMode::Human,
            &Report {
                schema: 1,
                status: "prepared",
            },
        )
        .unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), "OK prepared\n");

        let error = csa::error::ManagerError::new("invalid_cli", "bad option");
        let mut output = Vec::new();
        write_error_to(&mut output, OutputMode::Json, Operation::Parse, &error).unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "{\n  \"error\": {\n    \"code\": \"invalid_cli\",\n    \"message\": \"bad option\"\n  },\n  \"schema\": 1\n}\n"
        );

        let mut output = Vec::new();
        write_error_to(&mut output, OutputMode::Human, Operation::Parse, &error).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("ERROR [invalid_cli] bad option"));
        assert!(output.contains("Safety:"));
        assert!(output.contains("Recovery:"));

        let start = Instant::now();
        let mut progress = InstallProgress::new(OutputMode::Human);
        let mut output = Vec::new();
        progress
            .write_event_to(&mut output, InstallEvent::DetectingOfficial, start)
            .unwrap();
        progress
            .write_event_to(
                &mut output,
                InstallEvent::ArtifactProgress {
                    downloaded_bytes: 1024 * 1024,
                    total_bytes: 2 * 1024 * 1024,
                },
                start + Duration::from_secs(1),
            )
            .unwrap();
        progress
            .write_event_to(
                &mut output,
                InstallEvent::ArtifactProgress {
                    downloaded_bytes: 1024 * 1024 + 1,
                    total_bytes: 2 * 1024 * 1024,
                },
                start + Duration::from_millis(1_050),
            )
            .unwrap();
        progress
            .write_event_to(
                &mut output,
                InstallEvent::ArtifactProgress {
                    downloaded_bytes: 2 * 1024 * 1024,
                    total_bytes: 2 * 1024 * 1024,
                },
                start + Duration::from_secs(2),
            )
            .unwrap();
        progress
            .write_event_to(
                &mut output,
                InstallEvent::VerifyingArtifact,
                start + Duration::from_secs(2),
            )
            .unwrap();
        let output = String::from_utf8(output).unwrap();
        assert_eq!(output.matches("Downloading patched Codex").count(), 2);
        assert!(output.contains(" 50%"));
        assert!(output.contains("100%"));
        assert!(output.contains("Verifying downloaded artifact..."));

        let mut progress = InstallProgress::new(OutputMode::Json);
        let mut output = Vec::new();
        progress
            .write_event_to(&mut output, InstallEvent::DetectingOfficial, start)
            .unwrap();
        assert!(output.is_empty());
    }
}
