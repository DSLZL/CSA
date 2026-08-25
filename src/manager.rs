use crate::BUILD_TARGET;
use crate::activation::{
    ActivationReport, PlugReport, inspect as inspect_activation, plug, purge,
    recover as recover_activation, unplug,
};
use crate::compat::{ArtifactEntry, ContractStep, LoadedCompatibility, TestContract};
use crate::detect::{
    FileFingerprint, OfficialCodex, detect_official, find_executable, fingerprint, fingerprint_file,
};
use crate::error::{ManagerError, Result};
use crate::hash::sha256_bytes;
use crate::isolation::{IsolationPlan, IsolationRequest};
use crate::online::resolve_online_install;
use crate::process::{CommandResult, CommandSpec, ProcessRunner};
use crate::state::{
    Clock, ManagerPaths, PrepareLock, PreparedState, StateStore, ensure_managed_directory,
    remove_managed_tree, write_record,
};
use serde::Serialize;
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct DoctorOptions {
    pub manager_root: Option<PathBuf>,
    pub official: Option<PathBuf>,
    pub official_native: Option<PathBuf>,
    pub manifest: Option<PathBuf>,
}

#[derive(Clone, Debug, Serialize)]
pub struct DoctorReport {
    pub schema: u32,
    pub manager_root: PathBuf,
    pub manager_build_target: &'static str,
    pub official: OfficialCodex,
    pub compatibility: Option<CompatibilityReport>,
}

#[derive(Clone, Debug, Serialize)]
pub struct CompatibilityReport {
    pub compat_id: String,
    pub manifest_path: PathBuf,
    pub codex_version: String,
    pub build_target: String,
    pub exact_official_version: bool,
    pub supported_build_target: bool,
}

#[derive(Clone, Debug)]
pub struct PrepareOptions {
    pub manager_root: Option<PathBuf>,
    pub official: Option<PathBuf>,
    pub official_native: Option<PathBuf>,
    pub manifest: PathBuf,
    pub artifact: Option<PathBuf>,
    pub source: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct OnlineInstallOptions {
    pub manager_root: Option<PathBuf>,
    pub official: Option<PathBuf>,
    pub official_native: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub enum InstallOptions {
    Online(OnlineInstallOptions),
    Local(PrepareOptions),
}

#[derive(Clone, Debug, Serialize)]
pub struct PrepareReport {
    pub schema: u32,
    pub status: &'static str,
    pub state: PreparedState,
    pub official_unchanged: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct InstallReport {
    pub schema: u32,
    pub status: &'static str,
    pub prepare: PrepareReport,
    pub activation: PlugReport,
}

#[derive(Clone, Debug, Serialize)]
pub struct UninstallReport {
    pub schema: u32,
    pub status: &'static str,
    pub changed: bool,
    pub manager_root: PathBuf,
}

#[derive(Clone, Debug, Serialize)]
pub struct StatusReport {
    pub schema: u32,
    pub status: &'static str,
    pub manager_root: PathBuf,
    pub state: Option<PreparedState>,
    pub reason: Option<String>,
    pub activation: ActivationReport,
}

#[derive(Clone, Debug)]
pub struct ExecOptions {
    pub manager_root: Option<PathBuf>,
    pub isolation: IsolationRequest,
    pub args: Vec<OsString>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExecutionRecord {
    pub schema: u32,
    pub control_plane_executable: PathBuf,
    pub official_before: OfficialCodex,
    pub official_after: OfficialCodex,
    pub patched_executable: PathBuf,
    pub patched_before: FileFingerprint,
    pub patched_after: FileFingerprint,
    pub codex_home: PathBuf,
    pub cwd: PathBuf,
    pub logs_dir: PathBuf,
    pub state_dir: PathBuf,
    pub path_prefix: Option<PathBuf>,
    pub parent_path_sha256: String,
    pub npm_prefix: Option<PathBuf>,
    pub exit_code: Option<i32>,
    pub result: &'static str,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ExecOutcome {
    pub exit_code: i32,
    pub record: ExecutionRecord,
}

pub trait ArtifactProvider: Send + Sync {
    fn materialize(
        &self,
        entry: &ArtifactEntry,
        local_override: Option<&Path>,
        destination: &Path,
    ) -> Result<()>;
}

#[derive(Debug, Default)]
pub struct OfflineArtifactProvider;

impl ArtifactProvider for OfflineArtifactProvider {
    fn materialize(
        &self,
        _: &ArtifactEntry,
        local_override: Option<&Path>,
        destination: &Path,
    ) -> Result<()> {
        let source = local_override.ok_or_else(|| {
            ManagerError::new(
                "artifact_unavailable",
                "prepare requires a verified local artifact or source",
            )
        })?;
        if !source.is_absolute() {
            return Err(ManagerError::new(
                "unsafe_artifact_path",
                "artifact override must be absolute",
            ));
        }
        fs::copy(source, destination).map_err(|error| {
            ManagerError::io(
                &format!(
                    "copy artifact {} to {}",
                    source.display(),
                    destination.display()
                ),
                error,
            )
        })?;
        Ok(())
    }
}

pub fn doctor(options: DoctorOptions, runner: &dyn ProcessRunner) -> Result<DoctorReport> {
    let paths = ManagerPaths::resolve(options.manager_root)?;
    let official = detect_official(
        runner,
        options.official.as_deref(),
        options.official_native.as_deref(),
        std::slice::from_ref(&paths.root),
    )?;
    let compatibility = options
        .manifest
        .as_deref()
        .map(LoadedCompatibility::load)
        .transpose()?
        .map(|loaded| CompatibilityReport {
            compat_id: loaded.manifest.compat_id.clone(),
            manifest_path: loaded.manifest_path,
            codex_version: loaded.manifest.codex_version.clone(),
            build_target: loaded.manifest.build_target.clone(),
            exact_official_version: loaded.manifest.codex_version == official.version,
            supported_build_target: loaded.manifest.build_target == BUILD_TARGET,
        });
    Ok(DoctorReport {
        schema: 1,
        manager_root: paths.root,
        manager_build_target: BUILD_TARGET,
        official,
        compatibility,
    })
}

pub fn prepare(
    options: PrepareOptions,
    runner: &dyn ProcessRunner,
    clock: &dyn Clock,
    provider: &dyn ArtifactProvider,
) -> Result<PrepareReport> {
    if options.artifact.is_some() == options.source.is_some() {
        return Err(ManagerError::new(
            "invalid_prepare_source",
            "pass exactly one of --artifact or --source",
        ));
    }
    let compatibility = LoadedCompatibility::load(&options.manifest)?;
    compatibility.test_contract()?;
    if compatibility.manifest.build_target != BUILD_TARGET {
        return Err(ManagerError::new(
            "unsupported_build_target",
            format!(
                "payload targets {}, manager targets {BUILD_TARGET}",
                compatibility.manifest.build_target
            ),
        ));
    }
    let paths = ManagerPaths::resolve(options.manager_root)?;
    let _lock = PrepareLock::acquire(&paths)?;
    let store = StateStore::new(&paths);
    store.recover()?;
    let official_before = detect_official(
        runner,
        options.official.as_deref(),
        options.official_native.as_deref(),
        std::slice::from_ref(&paths.root),
    )?;
    require_compatible_official(&compatibility, &official_before)?;

    if let Some(source_artifact) = options.artifact.as_deref() {
        reject_same_file(source_artifact, &official_before)?;
    }
    let candidate = if let Some(source) = options.source.as_deref() {
        build_from_source(&compatibility, &paths, source, runner)?
    } else {
        options
            .artifact
            .clone()
            .expect("exclusive prepare source checked")
    };
    let artifact_path = publish_artifact(
        &compatibility,
        &paths,
        &candidate,
        &official_before,
        provider,
    )?;
    let official_after = detect_official(
        runner,
        Some(&official_before.executable.path),
        official_before
            .native
            .as_ref()
            .map(|native| native.path.as_path()),
        std::slice::from_ref(&paths.root),
    )?;
    if official_before != official_after {
        return Err(ManagerError::new(
            "official_changed",
            "official Codex fingerprint changed during prepare",
        ));
    }
    let artifact = fingerprint(&artifact_path)?;
    let compatibility = publish_compatibility(&compatibility, &paths)?;
    let state = PreparedState {
        schema: 2,
        compat_id: compatibility.manifest.compat_id.clone(),
        manifest_path: compatibility.manifest_path.clone(),
        build_target: compatibility.manifest.build_target.clone(),
        artifact_path: artifact.path,
        artifact_sha256: artifact.sha256,
        artifact_size: artifact.size,
        official: official_after,
        prepared_at_unix_seconds: clock.unix_seconds()?,
    };
    store.save(&state)?;
    Ok(PrepareReport {
        schema: 1,
        status: "prepared",
        state,
        official_unchanged: true,
    })
}

pub fn install(
    options: InstallOptions,
    runner: &dyn ProcessRunner,
    clock: &dyn Clock,
    provider: &dyn ArtifactProvider,
    manager_executable: &Path,
) -> Result<InstallReport> {
    match options {
        InstallOptions::Local(options) => {
            install_local(options, runner, clock, provider, manager_executable)
        }
        InstallOptions::Online(options) => {
            let bundle = resolve_online_install(&options, runner)?;
            install_local(
                bundle.prepare_options(),
                runner,
                clock,
                provider,
                manager_executable,
            )
        }
    }
}

fn install_local(
    options: PrepareOptions,
    runner: &dyn ProcessRunner,
    clock: &dyn Clock,
    provider: &dyn ArtifactProvider,
    manager_executable: &Path,
) -> Result<InstallReport> {
    let manager_root = options.manager_root.clone();
    let prepare = prepare(options, runner, clock, provider)?;
    let activation = match plug(manager_root.clone(), runner, clock, manager_executable) {
        Ok(report) => report,
        Err(install_error) => {
            if let Err(rollback_error) = unplug(manager_root) {
                return Err(ManagerError::new(
                    "install_rollback_failed",
                    format!("install failed: {install_error}; rollback failed: {rollback_error}"),
                ));
            }
            return Err(install_error);
        }
    };
    Ok(InstallReport {
        schema: 1,
        status: "installed",
        prepare,
        activation,
    })
}

fn publish_compatibility(
    compatibility: &LoadedCompatibility,
    paths: &ManagerPaths,
) -> Result<LoadedCompatibility> {
    let final_root = paths.manifests.join(&compatibility.manifest.compat_id);
    let final_manifest = final_root.join("manifest.toml");
    let files = compatibility.payload_files()?;
    if final_root.exists() {
        let metadata = fs::symlink_metadata(&final_root).map_err(|error| {
            ManagerError::io(
                &format!("inspect compatibility cache {}", final_root.display()),
                error,
            )
        })?;
        let canonical = final_root.canonicalize().map_err(|error| {
            ManagerError::io(
                &format!("canonicalize compatibility cache {}", final_root.display()),
                error,
            )
        })?;
        if metadata.file_type().is_symlink()
            || !metadata.is_dir()
            || !canonical.starts_with(paths.manifests.canonicalize().map_err(|error| {
                ManagerError::io("canonicalize managed manifests directory", error)
            })?)
        {
            return Err(ManagerError::new(
                "unsafe_compatibility_cache",
                "compatibility cache must be a real directory inside the manager root",
            ));
        }
        let existing = LoadedCompatibility::load(&final_manifest)?;
        existing.test_contract()?;
        compare_payload_files(&files, &existing.payload_files()?)?;
        return Ok(existing);
    }

    ensure_managed_directory(&paths.root, &paths.manifests)?;
    let staged = paths.manifests.join(format!(
        ".{}.staging-{}",
        compatibility.manifest.compat_id,
        std::process::id()
    ));
    remove_managed_tree(&paths.root, &staged)?;
    ensure_managed_directory(&paths.root, &staged)?;
    let result = (|| {
        for (relative, contents) in &files {
            let destination = staged.join(relative);
            let parent = destination.parent().ok_or_else(|| {
                ManagerError::new("invalid_payload_path", "payload file has no parent")
            })?;
            ensure_managed_directory(&paths.root, parent)?;
            fs::write(&destination, contents).map_err(|error| {
                ManagerError::io(
                    &format!("write compatibility file {}", destination.display()),
                    error,
                )
            })?;
        }
        fs::rename(&staged, &final_root)
            .map_err(|error| ManagerError::io("publish compatibility payload", error))?;
        let published = LoadedCompatibility::load(&final_manifest)?;
        published.test_contract()?;
        compare_payload_files(&files, &published.payload_files()?)?;
        Ok(published)
    })();
    if result.is_err() {
        let _ = remove_managed_tree(&paths.root, &staged);
        let _ = remove_managed_tree(&paths.root, &final_root);
    }
    result
}

fn compare_payload_files(
    expected: &BTreeMap<String, Vec<u8>>,
    actual: &BTreeMap<String, Vec<u8>>,
) -> Result<()> {
    if expected.keys().ne(actual.keys()) {
        return Err(ManagerError::new(
            "compatibility_cache_mismatch",
            "cached compatibility payload has a different file set",
        ));
    }
    for (relative, expected_contents) in expected {
        if expected_contents != &actual[relative] {
            return Err(ManagerError::new(
                "compatibility_cache_mismatch",
                format!("cached compatibility file differs: {relative}"),
            ));
        }
    }
    Ok(())
}

pub fn uninstall(manager_root: Option<PathBuf>) -> Result<UninstallReport> {
    let report = purge(manager_root)?;
    Ok(UninstallReport {
        schema: report.schema,
        status: "uninstalled",
        changed: report.changed,
        manager_root: report.manager_root,
    })
}

pub fn status(manager_root: Option<PathBuf>, runner: &dyn ProcessRunner) -> Result<StatusReport> {
    let paths = ManagerPaths::resolve(manager_root)?;
    if !paths.root.exists() {
        return Ok(StatusReport {
            schema: 1,
            status: "unprepared",
            manager_root: paths.root.clone(),
            state: None,
            reason: None,
            activation: inspect_activation(&paths, None),
        });
    }
    let _lock = PrepareLock::acquire(&paths)?;
    recover_activation(&paths)?;
    let store = StateStore::new(&paths);
    store.recover()?;
    let Some(state) = store.load()? else {
        return Ok(StatusReport {
            schema: 1,
            status: "unprepared",
            manager_root: paths.root.clone(),
            state: None,
            reason: None,
            activation: inspect_activation(&paths, None),
        });
    };
    let validation = validate_prepared_state(&state, &paths, runner);
    let activation = inspect_activation(&paths, validation.as_ref().ok().map(|_| &state));
    match validation {
        Ok(_) => Ok(StatusReport {
            schema: 1,
            status: "prepared",
            manager_root: paths.root,
            state: Some(state),
            reason: None,
            activation,
        }),
        Err(error) => Ok(StatusReport {
            schema: 1,
            status: "invalidated",
            manager_root: paths.root,
            state: Some(state),
            reason: Some(error.to_string()),
            activation,
        }),
    }
}

pub fn exec(options: ExecOptions, runner: &dyn ProcessRunner) -> Result<ExecOutcome> {
    let paths = ManagerPaths::resolve(options.manager_root)?;
    let (state, official_before) = {
        let _lock = PrepareLock::acquire(&paths)?;
        let store = StateStore::new(&paths);
        store.recover()?;
        let state = store.load()?.ok_or_else(|| {
            ManagerError::new("not_prepared", "no verified prepared state exists")
        })?;
        let official = validate_prepared_state(&state, &paths, runner)?;
        (state, official)
    };
    let isolation = IsolationPlan::create(options.isolation, &paths, &official_before)?;
    let patched_before = fingerprint(&state.artifact_path)?;
    let mut command = patched_command(
        CommandSpec::captured(&state.artifact_path)
            .args(options.args)
            .cwd(&isolation.cwd)
            .env("CODEX_HOME", isolation.codex_home.as_os_str())
            .env("CSA_LOG_DIR", isolation.logs_dir.as_os_str())
            .env("CSA_STATE_DIR", isolation.state_dir.as_os_str())
            .inherited(),
        &official_before,
    )?;
    if let Some(prefix) = &isolation.npm_prefix {
        command = command.env("npm_config_prefix", prefix.as_os_str());
    }

    let child = match runner.run(&command) {
        Ok(result) => result,
        Err(error) => {
            let official_after = redetect(&official_before, &paths, runner)?;
            let patched_after = fingerprint(&state.artifact_path)?;
            let record = execution_record(
                &official_before,
                official_after,
                patched_before,
                patched_after,
                &isolation,
                RecordedOutcome {
                    exit_code: None,
                    result: "failed",
                    error: Some(error.to_string()),
                },
            );
            write_record(&isolation.record_path, &record)?;
            return Err(error);
        }
    };
    let official_after = redetect(&official_before, &paths, runner)?;
    let patched_after = fingerprint(&state.artifact_path)?;
    let mut result = if child.code == Some(0) {
        "pass"
    } else {
        "child_exit"
    };
    let mut error = None;
    if official_after != official_before {
        result = "failed";
        error = Some("official Codex changed during isolated exec".to_owned());
    } else if patched_after != patched_before {
        result = "failed";
        error = Some("patched artifact changed during isolated exec".to_owned());
    }
    let record = execution_record(
        &official_before,
        official_after,
        patched_before,
        patched_after,
        &isolation,
        RecordedOutcome {
            exit_code: child.code,
            result,
            error: error.clone(),
        },
    );
    write_record(&isolation.record_path, &record)?;
    if let Some(error) = error {
        return Err(ManagerError::new("execution_integrity_failure", error));
    }
    Ok(ExecOutcome {
        exit_code: child.code.unwrap_or(1),
        record,
    })
}

pub(crate) fn patched_command(
    command: CommandSpec,
    official: &OfficialCodex,
) -> Result<CommandSpec> {
    command_with_official_runtime(command, official, true)
}

pub(crate) fn official_command(
    command: CommandSpec,
    official: &OfficialCodex,
) -> Result<CommandSpec> {
    command_with_official_runtime(command, official, false)
}

fn command_with_official_runtime(
    mut command: CommandSpec,
    official: &OfficialCodex,
    patched: bool,
) -> Result<CommandSpec> {
    for key in [
        "CODEX_MANAGED_BY_NPM",
        "CODEX_MANAGED_BY_BUN",
        "CODEX_MANAGED_BY_PNPM",
        "CODEX_MANAGED_PACKAGE_ROOT",
        "CSA_CODEX_OFFICIAL_PACKAGE_ROOT",
    ] {
        command = command.env_remove(key);
    }
    let Some(runtime) = &official.runtime else {
        #[cfg(windows)]
        if patched {
            return Err(ManagerError::new(
                "state_upgrade_required",
                "patched Codex has no verified official runtime binding; run csa install again",
            ));
        }
        return Ok(command);
    };
    command = command
        .env(
            "CODEX_MANAGED_PACKAGE_ROOT",
            runtime.managed_package_root.as_os_str(),
        )
        .env(runtime.package_manager.environment_key(), "1");
    if patched {
        command = command.env(
            "CSA_CODEX_OFFICIAL_PACKAGE_ROOT",
            runtime.package_root.as_os_str(),
        );
    }
    Ok(command)
}

fn execution_record(
    official_before: &OfficialCodex,
    official_after: OfficialCodex,
    patched_before: FileFingerprint,
    patched_after: FileFingerprint,
    isolation: &IsolationPlan,
    outcome: RecordedOutcome,
) -> ExecutionRecord {
    ExecutionRecord {
        schema: 1,
        control_plane_executable: official_before.executable.path.clone(),
        official_before: official_before.clone(),
        official_after,
        patched_executable: patched_before.path.clone(),
        patched_before,
        patched_after,
        codex_home: isolation.codex_home.clone(),
        cwd: isolation.cwd.clone(),
        logs_dir: isolation.logs_dir.clone(),
        state_dir: isolation.state_dir.clone(),
        path_prefix: isolation.path_prefix.clone(),
        parent_path_sha256: isolation.parent_path_sha256.clone(),
        npm_prefix: isolation.npm_prefix.clone(),
        exit_code: outcome.exit_code,
        result: outcome.result,
        error: outcome.error,
    }
}

struct RecordedOutcome {
    exit_code: Option<i32>,
    result: &'static str,
    error: Option<String>,
}

fn require_compatible_official(
    compatibility: &LoadedCompatibility,
    official: &OfficialCodex,
) -> Result<()> {
    if official.version != compatibility.manifest.codex_version {
        return Err(ManagerError::new(
            "unsupported_official_version",
            format!(
                "payload requires {}, official Codex is {}",
                compatibility.manifest.codex_version, official.version
            ),
        ));
    }
    #[cfg(windows)]
    if official.runtime.is_none() {
        return Err(ManagerError::new(
            "official_runtime_incomplete",
            "the selected Codex launcher is not backed by a complete official npm, Bun, or pnpm package",
        ));
    }
    Ok(())
}

pub(crate) fn validate_prepared_state(
    state: &PreparedState,
    paths: &ManagerPaths,
    runner: &dyn ProcessRunner,
) -> Result<OfficialCodex> {
    if state.schema != 2 {
        return Err(ManagerError::new(
            "state_upgrade_required",
            "prepared state predates official runtime binding; run csa install again",
        ));
    }
    let compatibility = LoadedCompatibility::load(&state.manifest_path)?;
    if state.compat_id != compatibility.manifest.compat_id
        || state.build_target != compatibility.manifest.build_target
        || state.artifact_sha256 != compatibility.artifact().sha256
        || state.artifact_size != compatibility.artifact().size
    {
        return Err(ManagerError::new(
            "state_manifest_mismatch",
            "prepared state no longer matches the compatibility manifest",
        ));
    }
    let artifact = fingerprint(&state.artifact_path)?;
    if artifact.sha256 != state.artifact_sha256 || artifact.size != state.artifact_size {
        return Err(ManagerError::new(
            "artifact_invalidated",
            "prepared artifact hash or size changed",
        ));
    }
    let official = redetect(&state.official, paths, runner)?;
    if official != state.official {
        return Err(ManagerError::new(
            "official_invalidated",
            "official Codex path, version, or hash changed",
        ));
    }
    reject_same_fingerprint(&artifact, &official)?;
    Ok(official)
}

fn redetect(
    official: &OfficialCodex,
    paths: &ManagerPaths,
    runner: &dyn ProcessRunner,
) -> Result<OfficialCodex> {
    detect_official(
        runner,
        Some(&official.executable.path),
        official.native.as_ref().map(|native| native.path.as_path()),
        std::slice::from_ref(&paths.root),
    )
}

fn reject_same_file(path: &Path, official: &OfficialCodex) -> Result<()> {
    let candidate = fingerprint(path)?;
    reject_same_fingerprint(&candidate, official)
}

fn reject_same_fingerprint(candidate: &FileFingerprint, official: &OfficialCodex) -> Result<()> {
    if candidate.path == official.executable.path
        || official
            .native
            .as_ref()
            .is_some_and(|native| candidate.path == native.path)
    {
        return Err(ManagerError::new(
            "official_patched_same_path",
            "official and patched executables resolve to the same path",
        ));
    }
    Ok(())
}

fn publish_artifact(
    compatibility: &LoadedCompatibility,
    paths: &ManagerPaths,
    source: &Path,
    official: &OfficialCodex,
    provider: &dyn ArtifactProvider,
) -> Result<PathBuf> {
    let entry = compatibility.artifact();
    let candidate = fingerprint(source)?;
    if candidate.sha256 != entry.sha256 || candidate.size != entry.size {
        return Err(ManagerError::new(
            "artifact_hash_mismatch",
            format!(
                "artifact expected {} bytes / {}, got {} bytes / {}",
                entry.size, entry.sha256, candidate.size, candidate.sha256
            ),
        ));
    }
    reject_same_fingerprint(&candidate, official)?;
    let directory = paths
        .artifacts
        .join(&compatibility.manifest.compat_id)
        .join(&entry.sha256)
        .join("runtime")
        .join("bin");
    ensure_managed_directory(&paths.root, &directory)?;
    let final_path = directory.join(&entry.filename);
    if final_path.exists() {
        let existing = fingerprint(&final_path)?;
        if existing.sha256 != entry.sha256 || existing.size != entry.size {
            return Err(ManagerError::new(
                "artifact_cache_corrupt",
                format!("cached artifact is invalid: {}", final_path.display()),
            ));
        }
        reject_same_fingerprint(&existing, official)?;
        return Ok(existing.path);
    }
    let staged = directory.join(format!(
        ".{}.staging-{}",
        entry.filename,
        std::process::id()
    ));
    remove_staged_file(&staged)?;
    provider.materialize(entry, Some(source), &staged)?;
    let staged_fingerprint = fingerprint_file(&staged)?;
    if staged_fingerprint.sha256 != entry.sha256 || staged_fingerprint.size != entry.size {
        let _ = fs::remove_file(&staged);
        return Err(ManagerError::new(
            "artifact_hash_mismatch",
            format!(
                "artifact expected {} bytes / {}, got {} bytes / {}",
                entry.size, entry.sha256, staged_fingerprint.size, staged_fingerprint.sha256
            ),
        ));
    }
    fs::rename(&staged, &final_path)
        .map_err(|error| ManagerError::io("publish verified artifact", error))?;
    Ok(fingerprint(&final_path)?.path)
}

fn remove_staged_file(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(ManagerError::new(
                "unsafe_staging_path",
                format!("staging path is not a real file: {}", path.display()),
            ))
        }
        Ok(_) => fs::remove_file(path)
            .map_err(|error| ManagerError::io(&format!("remove {}", path.display()), error)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(ManagerError::io(
            &format!("inspect {}", path.display()),
            error,
        )),
    }
}

fn build_from_source(
    compatibility: &LoadedCompatibility,
    paths: &ManagerPaths,
    source: &Path,
    runner: &dyn ProcessRunner,
) -> Result<PathBuf> {
    if !source.is_absolute() {
        return Err(ManagerError::new(
            "unsafe_source_path",
            "source checkout must be absolute",
        ));
    }
    let source = source.canonicalize().map_err(|error| {
        ManagerError::io(&format!("canonicalize source {}", source.display()), error)
    })?;
    if !source.is_dir() || source.starts_with(&paths.root) || paths.root.starts_with(&source) {
        return Err(ManagerError::new(
            "unsafe_source_path",
            "source must be a real directory outside the manager root",
        ));
    }
    let path_value = std::env::var_os("PATH");
    let git = find_executable(
        "git",
        path_value.as_deref(),
        std::slice::from_ref(&paths.root),
    )?;
    let rustup = find_executable(
        "rustup",
        path_value.as_deref(),
        std::slice::from_ref(&paths.root),
    )?;
    let clone = paths.sources.join(format!(
        "{}-{}",
        compatibility.manifest.compat_id,
        &compatibility.manifest.upstream_commit[..12]
    ));
    remove_managed_tree(&paths.root, &clone)?;
    run_success(
        runner,
        CommandSpec::captured(&git).args([
            os("clone"),
            os("--no-checkout"),
            os("--local"),
            source.as_os_str().to_owned(),
            clone.as_os_str().to_owned(),
        ]),
        "clone exact local Codex source",
    )?;
    ensure_managed_directory(&paths.root, &clone)?;
    run_success(
        runner,
        git_command(&git, &clone).args([
            os("checkout"),
            os("--detach"),
            os(&compatibility.manifest.upstream_commit),
        ]),
        "checkout exact Codex commit",
    )?;
    verify_and_apply_patches(compatibility, paths, &clone, &git, runner)?;
    run_test_contract(compatibility, paths, &clone, &rustup, runner)?;
    Ok(paths
        .builds
        .join(&compatibility.manifest.compat_id)
        .join("target")
        .join(&compatibility.manifest.build_target)
        .join("release")
        .join(&compatibility.artifact().filename))
}

fn verify_and_apply_patches(
    compatibility: &LoadedCompatibility,
    paths: &ManagerPaths,
    source: &Path,
    git: &Path,
    runner: &dyn ProcessRunner,
) -> Result<()> {
    let manifest = &compatibility.manifest;
    let status = run_success(
        runner,
        git_command(git, source).args([
            os("status"),
            os("--porcelain=v1"),
            os("--untracked-files=all"),
        ]),
        "inspect source worktree",
    )?;
    if !status.stdout.is_empty() {
        return Err(ManagerError::new(
            "dirty_source",
            "managed source checkout is not clean before patching",
        ));
    }
    let head = captured_text(
        run_success(
            runner,
            git_command(git, source).args([os("rev-parse"), os("HEAD")]),
            "resolve source commit",
        )?,
        "source commit",
    )?;
    if head != manifest.upstream_commit {
        return Err(ManagerError::new(
            "source_commit_mismatch",
            format!("expected {}, got {head}", manifest.upstream_commit),
        ));
    }
    let tag = captured_text(
        run_success(
            runner,
            git_command(git, source).args([
                os("rev-parse"),
                os(format!("refs/tags/{}^{{commit}}", manifest.upstream_tag)),
            ]),
            "resolve source release tag",
        )?,
        "source tag commit",
    )?;
    if tag != head {
        return Err(ManagerError::new(
            "source_tag_mismatch",
            "upstream tag does not peel to the exact commit",
        ));
    }
    let cargo_text = fs::read_to_string(source.join("codex-rs/Cargo.toml"))
        .map_err(|error| ManagerError::io("read Codex workspace Cargo.toml", error))?;
    let cargo: toml::Value = toml::from_str(&cargo_text)
        .map_err(|error| ManagerError::new("invalid_source", format!("Cargo.toml: {error}")))?;
    let source_version = cargo
        .get("workspace")
        .and_then(|workspace| workspace.get("package"))
        .and_then(|package| package.get("version"))
        .and_then(toml::Value::as_str);
    if source_version != Some(&manifest.codex_version) {
        return Err(ManagerError::new(
            "source_version_mismatch",
            "Codex workspace version differs from the manifest",
        ));
    }
    for (relative, expected) in &manifest.preimage {
        if !source.join(relative).is_file() {
            return Err(ManagerError::new(
                "preimage_missing",
                format!("preimage file is missing: {relative}"),
            ));
        }
        let blob = run_success(
            runner,
            git_command(git, source).args([
                os("show"),
                os(format!("{}:{relative}", manifest.upstream_commit)),
            ]),
            "read source preimage",
        )?;
        if sha256_bytes(&blob.stdout) != *expected {
            return Err(ManagerError::new(
                "preimage_hash_mismatch",
                format!("preimage hash mismatch: {relative}"),
            ));
        }
    }
    for relative in &manifest.preimage_absent {
        if source.join(relative).exists() {
            return Err(ManagerError::new(
                "absent_preimage_exists",
                format!("expected absent path exists: {relative}"),
            ));
        }
        let result = runner.run(&git_command(git, source).args([
            os("cat-file"),
            os("-e"),
            os(format!("{}:{relative}", manifest.upstream_commit)),
        ]))?;
        if result.code == Some(0) {
            return Err(ManagerError::new(
                "absent_preimage_exists",
                format!("expected absent path exists in commit: {relative}"),
            ));
        }
    }

    let build_root = paths.builds.join(&manifest.compat_id);
    ensure_managed_directory(&paths.root, &build_root)?;
    let patch_index = build_root.join("patch-preflight.index");
    remove_staged_file(&patch_index)?;
    let index_env = patch_index.as_os_str().to_owned();
    run_success(
        runner,
        git_command(git, source)
            .args([os("read-tree"), os(&manifest.upstream_commit)])
            .env("GIT_INDEX_FILE", &index_env),
        "seed patch preflight index",
    )?;
    for patch in &compatibility.patch_paths {
        run_success(
            runner,
            git_command(git, source)
                .args([
                    os("apply"),
                    os("--cached"),
                    os("--check"),
                    os("--whitespace=error-all"),
                    patch.as_os_str().to_owned(),
                ])
                .env("GIT_INDEX_FILE", &index_env),
            "preflight ordered patch",
        )?;
        run_success(
            runner,
            git_command(git, source)
                .args([
                    os("apply"),
                    os("--cached"),
                    os("--whitespace=error-all"),
                    patch.as_os_str().to_owned(),
                ])
                .env("GIT_INDEX_FILE", &index_env),
            "stage ordered patch in preflight index",
        )?;
    }
    remove_staged_file(&patch_index)?;
    for patch in &compatibility.patch_paths {
        run_success(
            runner,
            git_command(git, source).args([
                os("apply"),
                os("--check"),
                os("--whitespace=error-all"),
                patch.as_os_str().to_owned(),
            ]),
            "check ordered patch against worktree",
        )?;
        run_success(
            runner,
            git_command(git, source).args([
                os("apply"),
                os("--whitespace=error-all"),
                patch.as_os_str().to_owned(),
            ]),
            "apply ordered patch to worktree",
        )?;
    }
    Ok(())
}

fn run_test_contract(
    compatibility: &LoadedCompatibility,
    paths: &ManagerPaths,
    source: &Path,
    rustup: &Path,
    runner: &dyn ProcessRunner,
) -> Result<()> {
    let manifest = &compatibility.manifest;
    let contract = compatibility.test_contract()?;
    let rustc = run_success(
        runner,
        CommandSpec::captured(rustup).args([
            os("run"),
            os(&manifest.rust_toolchain),
            os("rustc"),
            os("--version"),
            os("--verbose"),
        ]),
        "inspect pinned Rust toolchain",
    )?;
    let rustc = std::str::from_utf8(&rustc.stdout)
        .map_err(|_| ManagerError::new("toolchain_mismatch", "rustc output is not UTF-8"))?;
    if !rustc
        .lines()
        .any(|line| line.trim() == format!("commit-hash: {}", manifest.rustc_commit))
        || !rustc
            .lines()
            .any(|line| line.trim() == format!("release: {}", manifest.rust_toolchain))
    {
        return Err(ManagerError::new(
            "toolchain_mismatch",
            "installed Rust toolchain does not match manifest release and commit",
        ));
    }
    let target = paths.builds.join(&manifest.compat_id).join("target");
    ensure_managed_directory(&paths.root, &target)?;
    for step in &contract.generation {
        run_contract_step(
            &contract,
            step,
            source,
            &target,
            rustup,
            &manifest.rust_toolchain,
            runner,
        )?;
    }
    for step in &contract.tests {
        run_contract_step(
            &contract,
            step,
            source,
            &target,
            rustup,
            &manifest.rust_toolchain,
            runner,
        )?;
    }
    let build_step = ContractStep {
        name: "release build".to_owned(),
        env: contract.build.env.clone(),
        argv: contract.build.argv.clone(),
        output: None,
    };
    run_contract_step(
        &contract,
        &build_step,
        source,
        &target,
        rustup,
        &manifest.rust_toolchain,
        runner,
    )?;
    Ok(())
}

fn run_contract_step(
    contract: &TestContract,
    step: &ContractStep,
    source: &Path,
    target: &Path,
    rustup: &Path,
    toolchain: &str,
    runner: &dyn ProcessRunner,
) -> Result<()> {
    let mut env = contract.common_env.clone();
    env.extend(step.env.clone());
    let mut command = CommandSpec::captured(rustup).args([os("run"), os(toolchain), os("cargo")]);
    command.args.extend(step.argv.iter().skip(1).map(os));
    command.cwd = Some(source.join("codex-rs"));
    command.env = expand_env(env, source, target)?;
    run_success(runner, command, &step.name)?;
    Ok(())
}

fn expand_env(
    env: BTreeMap<String, String>,
    source: &Path,
    target: &Path,
) -> Result<BTreeMap<OsString, OsString>> {
    let source = source.to_str().ok_or_else(|| {
        ManagerError::new("non_utf8_build_path", "source path is not valid UTF-8")
    })?;
    let target = target.to_str().ok_or_else(|| {
        ManagerError::new("non_utf8_build_path", "target path is not valid UTF-8")
    })?;
    Ok(env
        .into_iter()
        .map(|(key, value)| {
            (
                OsString::from(key),
                OsString::from(
                    value
                        .replace("{source}", source)
                        .replace("{cargo_target}", target),
                ),
            )
        })
        .collect())
}

fn git_command(git: &Path, cwd: &Path) -> CommandSpec {
    CommandSpec::captured(git).args([os("-C"), cwd.as_os_str().to_owned()])
}

fn run_success(
    runner: &dyn ProcessRunner,
    command: CommandSpec,
    context: &str,
) -> Result<CommandResult> {
    runner.run(&command)?.require_success(context)
}

fn captured_text(result: CommandResult, context: &str) -> Result<String> {
    std::str::from_utf8(&result.stdout)
        .map(str::trim)
        .map(str::to_owned)
        .map_err(|_| ManagerError::new("invalid_command_output", format!("{context} is not UTF-8")))
}

fn os(value: impl AsRef<OsStr>) -> OsString {
    value.as_ref().to_os_string()
}
