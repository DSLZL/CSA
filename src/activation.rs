use crate::detect::{
    FileFingerprint, OfficialCodex, detect_official, find_codex_launcher, fingerprint,
};
#[cfg(windows)]
use crate::detect::{parse_codex_version, windows_csa_system_bin};
use crate::error::{ManagerError, Result};
use crate::manager::{official_command, patched_command, validate_prepared_state};
use crate::process::{CommandSpec, ProcessRunner};
use crate::state::{
    Clock, ManagerPaths, PrepareLock, PreparedState, StateStore, remove_file_if_exists,
    remove_managed_tree, write_new_synced,
};
use serde::{Deserialize, Serialize};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

#[cfg(windows)]
const SHIM_NAME: &str = "codex.exe";
#[cfg(not(windows))]
const SHIM_NAME: &str = "codex";
#[cfg(windows)]
const STAGED_SHIM_NAME: &str = ".csa.staging.exe";
#[cfg(not(windows))]
const STAGED_SHIM_NAME: &str = ".csa.staging";
#[cfg(windows)]
const REMOVED_SHIM_NAME: &str = ".csa.removed.exe";
#[cfg(not(windows))]
const REMOVED_SHIM_NAME: &str = ".csa.removed";
#[cfg(windows)]
const MANAGER_ROOT_ENV: &str = "DSLZL_CSA_MANAGER_ROOT";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActivationBinding {
    pub compat_id: String,
    pub manifest_path: PathBuf,
    pub artifact_path: PathBuf,
    pub artifact_sha256: String,
    pub artifact_size: u64,
    pub official: OfficialCodex,
}

impl ActivationBinding {
    fn from_prepared(state: &PreparedState) -> Self {
        Self {
            compat_id: state.compat_id.clone(),
            manifest_path: state.manifest_path.clone(),
            artifact_path: state.artifact_path.clone(),
            artifact_sha256: state.artifact_sha256.clone(),
            artifact_size: state.artifact_size,
            official: state.official.clone(),
        }
    }

    fn matches(&self, state: &PreparedState) -> bool {
        self == &Self::from_prepared(state)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActivationState {
    pub schema: u32,
    pub binding: ActivationBinding,
    pub shim_sha256: String,
    pub shim_size: u64,
    pub activated_at_unix_seconds: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ActivationReport {
    pub status: &'static str,
    pub effective: bool,
    pub managed_bin: PathBuf,
    pub shim_path: PathBuf,
    pub command_resolution: CommandResolution,
    pub state: Option<ActivationState>,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CommandResolution {
    pub managed_bin_on_path: bool,
    pub resolved_codex: Option<PathBuf>,
    pub resolves_to_managed_shim: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct UserPathReport {
    pub status: &'static str,
    pub changed: bool,
    pub command_resolution: CommandResolution,
}

#[cfg(windows)]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WindowsPathUpdate {
    changed: bool,
    effective_path: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct PlugReport {
    pub schema: u32,
    pub status: &'static str,
    pub changed: bool,
    pub managed_bin_on_path: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_path: Option<UserPathReport>,
    pub activation: ActivationReport,
}

#[derive(Clone, Debug, Serialize)]
pub struct UnplugReport {
    pub schema: u32,
    pub status: &'static str,
    pub changed: bool,
    pub managed_bin: PathBuf,
}

#[derive(Clone, Debug, Serialize)]
pub struct PurgeReport {
    pub schema: u32,
    pub status: &'static str,
    pub changed: bool,
    pub manager_root: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShimSelection {
    pub mode: &'static str,
    pub target: PathBuf,
    pub official: OfficialCodex,
    pub compat_id: Option<String>,
    pub fallback_reason: Option<String>,
}

pub fn shim_path(paths: &ManagerPaths) -> PathBuf {
    paths.bin.join(SHIM_NAME)
}

pub fn is_current_process_shim() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.file_name().map(OsStr::to_os_string))
        .is_some_and(|name| name.eq_ignore_ascii_case(OsStr::new(SHIM_NAME)))
}

pub fn plug(
    manager_root: Option<PathBuf>,
    runner: &dyn ProcessRunner,
    clock: &dyn Clock,
    shim_source: &Path,
) -> Result<PlugReport> {
    let paths = ManagerPaths::resolve(manager_root)?;
    let _lock = PrepareLock::acquire(&paths)?;
    recover(&paths)?;
    let store = StateStore::new(&paths);
    store.recover()?;
    let prepared = store
        .load()?
        .ok_or_else(|| ManagerError::new("not_prepared", "prepare must succeed before plug"))?;
    let official = validate_prepared_state(&prepared, &paths, runner)?;
    let source = fingerprint(shim_source)?;
    reject_shim_source(&source, &paths, &prepared, &official)?;

    if let Ok(active) = read_active(&paths.active) {
        let final_shim = shim_path(&paths);
        if active.schema == 2
            && active.binding.matches(&prepared)
            && active.shim_sha256 == source.sha256
            && active.shim_size == source.size
            && fingerprint(&final_shim)
                .is_ok_and(|shim| shim.sha256 == source.sha256 && shim.size == source.size)
        {
            let activation = inspect(&paths, Some(&prepared));
            return Ok(PlugReport {
                schema: 1,
                status: "plugged",
                changed: false,
                managed_bin_on_path: activation.command_resolution.managed_bin_on_path,
                user_path: None,
                activation,
            });
        }
    }

    deactivate_locked(&paths)?;
    let staged = paths.bin.join(STAGED_SHIM_NAME);
    remove_owned_file(&staged)?;
    if let Err(error) = copy_synced(&source.path, &staged) {
        let _ = remove_owned_file(&staged);
        return Err(error);
    }
    let staged_fingerprint = fingerprint(&staged)?;
    if staged_fingerprint.sha256 != source.sha256 || staged_fingerprint.size != source.size {
        remove_owned_file(&staged)?;
        return Err(ManagerError::new(
            "shim_hash_mismatch",
            "staged activation shim differs from the manager executable",
        ));
    }

    let active = ActivationState {
        schema: 2,
        binding: ActivationBinding::from_prepared(&prepared),
        shim_sha256: staged_fingerprint.sha256,
        shim_size: staged_fingerprint.size,
        activated_at_unix_seconds: clock.unix_seconds()?,
    };
    publish_active(&paths, &active)?;
    let final_shim = shim_path(&paths);
    if let Err(error) = fs::rename(&staged, &final_shim) {
        let _ = remove_file_if_exists(&paths.active);
        let _ = remove_owned_file(&staged);
        return Err(ManagerError::io("publish activation shim", error));
    }
    if let Err(error) = sync_directory(&paths.bin) {
        deactivate_locked(&paths)?;
        return Err(error);
    }

    if let Err(error) = validate_prepared_state(&prepared, &paths, runner) {
        deactivate_locked(&paths)?;
        return Err(error);
    }
    let activation = inspect(&paths, Some(&prepared));
    if activation.status != "plugged" {
        deactivate_locked(&paths)?;
        return Err(ManagerError::new(
            "activation_verification_failed",
            activation
                .reason
                .unwrap_or_else(|| "activation post-verification failed".to_owned()),
        ));
    }
    Ok(PlugReport {
        schema: 1,
        status: "plugged",
        changed: true,
        managed_bin_on_path: activation.command_resolution.managed_bin_on_path,
        user_path: None,
        activation,
    })
}

pub fn unplug(manager_root: Option<PathBuf>) -> Result<UnplugReport> {
    let paths = ManagerPaths::resolve(manager_root)?;
    if !paths.root.exists() {
        return Ok(UnplugReport {
            schema: 1,
            status: "unplugged",
            changed: false,
            managed_bin: paths.bin,
        });
    }
    let _lock = PrepareLock::acquire(&paths)?;
    let recovered = recover(&paths)?;
    let changed = recovered || deactivate_locked(&paths)?;
    Ok(UnplugReport {
        schema: 1,
        status: "unplugged",
        changed,
        managed_bin: paths.bin,
    })
}

pub fn purge(manager_root: Option<PathBuf>) -> Result<PurgeReport> {
    let paths = ManagerPaths::resolve(manager_root)?;
    if !paths.root.exists() {
        return Ok(PurgeReport {
            schema: 1,
            status: "purged",
            changed: false,
            manager_root: paths.root,
        });
    }
    let changed_before = managed_data_exists(&paths)?;
    let _lock = PrepareLock::acquire(&paths)?;
    recover(&paths)?;
    deactivate_locked(&paths)?;
    for directory in [
        &paths.artifacts,
        &paths.manifests,
        &paths.downloads,
        &paths.sources,
        &paths.builds,
    ] {
        remove_managed_tree(&paths.root, directory)?;
    }
    for state in [
        paths.state.clone(),
        paths.root.join("state.json.next"),
        paths.root.join("state.json.previous"),
    ] {
        remove_owned_file(&state)?;
    }
    Ok(PurgeReport {
        schema: 1,
        status: "purged",
        changed: changed_before,
        manager_root: paths.root,
    })
}

pub fn inspect(paths: &ManagerPaths, prepared: Option<&PreparedState>) -> ActivationReport {
    let final_shim = shim_path(paths);
    let path_value = std::env::var_os("PATH");
    let command_resolution = inspect_command_resolution(paths, path_value.as_deref());
    let active_exists = fs::symlink_metadata(&paths.active).is_ok();
    let shim_exists = fs::symlink_metadata(&final_shim).is_ok();
    if !active_exists && !shim_exists {
        return ActivationReport {
            status: "unplugged",
            effective: false,
            managed_bin: paths.bin.clone(),
            shim_path: final_shim,
            command_resolution,
            state: None,
            reason: None,
        };
    }

    let checked = (|| {
        let active = read_active(&paths.active)?;
        let prepared = prepared.ok_or_else(|| {
            ManagerError::new("activation_fallback", "prepared state is not valid")
        })?;
        if !active.binding.matches(prepared) {
            return Err(ManagerError::new(
                "activation_state_mismatch",
                "active state does not bind the current prepared state",
            ));
        }
        let shim = fingerprint(&final_shim)?;
        if shim.sha256 != active.shim_sha256 || shim.size != active.shim_size {
            return Err(ManagerError::new(
                "shim_hash_mismatch",
                "activation shim hash or size changed",
            ));
        }
        Ok(active)
    })();
    match checked {
        Ok(active) => ActivationReport {
            status: "plugged",
            effective: command_resolution.resolves_to_managed_shim,
            managed_bin: paths.bin.clone(),
            shim_path: final_shim,
            command_resolution,
            state: Some(active),
            reason: None,
        },
        Err(error) => ActivationReport {
            status: "fallback",
            effective: false,
            managed_bin: paths.bin.clone(),
            shim_path: final_shim,
            command_resolution,
            state: read_active(&paths.active).ok(),
            reason: Some(error.to_string()),
        },
    }
}

pub fn inspect_command_resolution(
    paths: &ManagerPaths,
    path_value: Option<&OsStr>,
) -> CommandResolution {
    let resolved_codex = find_codex_launcher(path_value, &[]).ok();
    let managed_shim = shim_path(paths).canonicalize().ok();
    #[cfg(windows)]
    let system_bin = windows_csa_system_bin().ok();
    #[cfg(windows)]
    let system_managed = resolved_codex.as_ref().is_some_and(|actual| {
        system_bin
            .as_ref()
            .and_then(|bin| bin.join(SHIM_NAME).canonicalize().ok())
            .is_some_and(|system_shim| {
                actual == &system_shim
                    && fingerprint(&system_shim)
                        .ok()
                        .zip(fingerprint(&shim_path(paths)).ok())
                        .is_some_and(|(system, user)| {
                            system.sha256 == user.sha256 && system.size == user.size
                        })
            })
    });
    #[cfg(not(windows))]
    let system_managed = false;
    #[cfg(windows)]
    let system_bin_on_path = system_bin
        .as_ref()
        .is_some_and(|bin| path_contains(bin, path_value));
    #[cfg(not(windows))]
    let system_bin_on_path = false;
    CommandResolution {
        managed_bin_on_path: path_contains(&paths.bin, path_value) || system_bin_on_path,
        resolves_to_managed_shim: resolved_codex.as_ref().is_some_and(|actual| {
            managed_shim
                .as_ref()
                .is_some_and(|expected| actual == expected)
        }) || system_managed,
        resolved_codex,
    }
}

#[cfg(windows)]
pub fn prioritize_windows_user_path(
    activation: &ActivationReport,
    runner: &dyn ProcessRunner,
) -> Result<UserPathReport> {
    let root = activation.managed_bin.parent().ok_or_else(|| {
        ManagerError::new("unsafe_manager_root", "managed bin has no manager root")
    })?;
    let powershell = windows_system_executable("System32/WindowsPowerShell/v1.0/powershell.exe")?;
    let update = runner
        .run(
            &CommandSpec::captured(&powershell)
                .args([
                    "-NoLogo",
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    WINDOWS_USER_PATH_SCRIPT,
                ])
                .env("CSA_MANAGED_BIN", activation.managed_bin.as_os_str())
                .env("CSA_MANAGER_ROOT", root.as_os_str())
                .env("CSA_PATH_MODE", "prepend"),
        )?
        .require_success("put the CSA managed bin first in the Windows user PATH")?;
    let mut update = parse_windows_path_update(&update.stdout, "path_activation_failed")?;
    let paths = ManagerPaths::resolve(Some(root.to_path_buf()))?;
    let mut verification_path = OsString::from(&update.effective_path);
    let mut command_resolution =
        inspect_command_resolution(&paths, Some(verification_path.as_os_str()));
    if !command_resolution.resolves_to_managed_shim {
        let elevated = prioritize_windows_machine_path(activation, root, &powershell, runner)?;
        update.changed |= elevated.changed;
        verification_path = OsString::from(elevated.effective_path);
        command_resolution =
            inspect_command_resolution(&paths, Some(verification_path.as_os_str()));
    }
    if !command_resolution.resolves_to_managed_shim {
        let resolved = command_resolution.resolved_codex.as_ref().map_or_else(
            || "no Codex command".to_owned(),
            |path| path.display().to_string(),
        );
        return Err(ManagerError::new(
            "path_precedence_conflict",
            format!(
                "Windows still selects {resolved} before the CSA shim {} after elevated PATH activation",
                activation.shim_path.display()
            ),
        ));
    }

    let active = activation.state.as_ref().ok_or_else(|| {
        ManagerError::new(
            "path_activation_failed",
            "active state is unavailable for `codex --version` verification",
        )
    })?;
    let resolved = command_resolution.resolved_codex.as_ref().ok_or_else(|| {
        ManagerError::new(
            "path_activation_failed",
            "the verified Codex command path is unavailable",
        )
    })?;
    let version = runner
        .run(
            &CommandSpec::captured(resolved)
                .arg("--version")
                .env("PATH", &verification_path)
                .env(MANAGER_ROOT_ENV, root.as_os_str()),
        )?
        .require_success("verify patched Codex with `codex --version`")?;
    let version_bytes = if version.stdout.is_empty() {
        &version.stderr
    } else {
        &version.stdout
    };
    let version_text = std::str::from_utf8(version_bytes).map_err(|_| {
        ManagerError::new(
            "path_activation_failed",
            "`codex --version` returned non-UTF-8 output",
        )
    })?;
    let actual_version =
        parse_csa_codex_version(version_text, &active.binding.compat_id).map_err(|error| {
            ManagerError::new(
                "path_activation_failed",
                format!("`codex --version` verification failed: {error}"),
            )
        })?;
    let expected_version = active.binding.official.version.as_str();
    if actual_version != expected_version {
        return Err(ManagerError::new(
            "path_activation_failed",
            format!("`codex --version` returned {actual_version}, expected {expected_version}"),
        ));
    }
    Ok(UserPathReport {
        status: "verified",
        changed: update.changed,
        command_resolution,
    })
}

#[cfg(windows)]
fn prioritize_windows_machine_path(
    activation: &ActivationReport,
    root: &Path,
    powershell: &Path,
    runner: &dyn ProcessRunner,
) -> Result<WindowsPathUpdate> {
    let update = runner
        .run(
            &CommandSpec::captured(powershell)
                .args([
                    "-NoLogo",
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    WINDOWS_MACHINE_PATH_SCRIPT,
                ])
                .env("CSA_MACHINE_PATH_MODE", "install")
                .env("CSA_SHIM_SOURCE", activation.shim_path.as_os_str())
                .env("CSA_MANAGER_ROOT", root.as_os_str()),
        )
        .map_err(|error| {
            ManagerError::new(
                "path_elevation_failed",
                format!("could not request administrator permission: {error}"),
            )
        })?
        .require_success("install the elevated CSA Codex dispatcher")
        .map_err(|error| ManagerError::new("path_elevation_failed", error.to_string()))?;
    parse_windows_path_update(&update.stdout, "path_elevation_failed")
}

#[cfg(windows)]
fn parse_csa_codex_version(text: &str, compat_id: &str) -> Result<String> {
    let suffix = format!(" (CSA {compat_id})");
    let value = text.trim();
    let version = value.strip_suffix(&suffix).ok_or_else(|| {
        ManagerError::new(
            "invalid_csa_version",
            format!("expected a CSA version marker for {compat_id}, got {value:?}"),
        )
    })?;
    parse_codex_version(version)
}

#[cfg(windows)]
pub fn remove_windows_user_path(managed_bin: &Path, runner: &dyn ProcessRunner) -> Result<bool> {
    let root = managed_bin.parent().ok_or_else(|| {
        ManagerError::new("unsafe_manager_root", "managed bin has no manager root")
    })?;
    let powershell = windows_system_executable("System32/WindowsPowerShell/v1.0/powershell.exe")?;
    let user_update = runner
        .run(
            &CommandSpec::captured(&powershell)
                .args([
                    "-NoLogo",
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    WINDOWS_USER_PATH_SCRIPT,
                ])
                .env("CSA_MANAGED_BIN", managed_bin.as_os_str())
                .env("CSA_MANAGER_ROOT", root.as_os_str())
                .env("CSA_PATH_MODE", "remove"),
        )?
        .require_success("remove the CSA managed bin from the Windows user PATH")
        .map_err(|error| ManagerError::new("path_deactivation_failed", error.to_string()))?;
    let user_update = parse_windows_path_update(&user_update.stdout, "path_deactivation_failed")?;
    let machine_update = runner
        .run(
            &CommandSpec::captured(&powershell)
                .args([
                    "-NoLogo",
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    WINDOWS_MACHINE_PATH_SCRIPT,
                ])
                .env("CSA_MACHINE_PATH_MODE", "remove"),
        )
        .map_err(|error| {
            ManagerError::new(
                "path_deactivation_failed",
                format!("could not request administrator permission: {error}"),
            )
        })?
        .require_success("remove the elevated CSA Codex dispatcher")
        .map_err(|error| ManagerError::new("path_deactivation_failed", error.to_string()))?;
    let machine_update =
        parse_windows_path_update(&machine_update.stdout, "path_deactivation_failed")?;
    Ok(user_update.changed || machine_update.changed)
}

#[cfg(windows)]
fn parse_windows_path_update(bytes: &[u8], error_code: &'static str) -> Result<WindowsPathUpdate> {
    serde_json::from_slice(bytes).map_err(|error| {
        ManagerError::new(
            error_code,
            format!("Windows user PATH update returned invalid JSON: {error}"),
        )
    })
}

#[cfg(windows)]
fn windows_system_executable(relative: &str) -> Result<PathBuf> {
    let root = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| ManagerError::new("windows_system_root_missing", "SystemRoot is invalid"))?;
    let executable = root.join(relative).canonicalize().map_err(|error| {
        ManagerError::io(
            &format!("resolve Windows system executable {relative}"),
            error,
        )
    })?;
    if !executable.is_file() {
        return Err(ManagerError::new(
            "windows_system_tool_missing",
            format!(
                "Windows system executable is missing: {}",
                executable.display()
            ),
        ));
    }
    Ok(executable)
}

#[cfg(windows)]
const WINDOWS_USER_PATH_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$managed = $env:CSA_MANAGED_BIN
$managerRoot = $env:CSA_MANAGER_ROOT
$mode = $env:CSA_PATH_MODE
$key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey('Environment')
if ($null -eq $key) { throw 'cannot open HKCU\Environment' }
try {
  $current = [string]$key.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
  try { $kind = $key.GetValueKind('Path') }
  catch { $kind = [Microsoft.Win32.RegistryValueKind]::ExpandString }
  $entries = @($current -split ';' | Where-Object { $_ -and $_ -ine $managed })
  if ($mode -eq 'prepend') { $updated = (@($managed) + $entries) -join ';' }
  elseif ($mode -eq 'remove') { $updated = $entries -join ';' }
  else { throw 'invalid CSA_PATH_MODE' }
  $pathChanged = $updated -cne $current
  if ($pathChanged) {
    $key.SetValue('Path', $updated, $kind)
  }
  $rootName = 'DSLZL_CSA_MANAGER_ROOT'
  $savedRoot = [string]$key.GetValue($rootName, '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
  $rootChanged = $false
  if ($mode -eq 'prepend' -and $savedRoot -ine $managerRoot) {
    $key.SetValue($rootName, $managerRoot, [Microsoft.Win32.RegistryValueKind]::String)
    $rootChanged = $true
  }
  elseif ($mode -eq 'remove' -and $savedRoot -ieq $managerRoot) {
    $key.DeleteValue($rootName, $false)
    $rootChanged = $true
  }
  $changed = $pathChanged -or $rootChanged
  if ($changed) {
    try {
      $member = '[DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern IntPtr SendMessageTimeout(IntPtr hWnd, uint msg, UIntPtr wParam, string lParam, uint flags, uint timeout, out UIntPtr result);'
      $native = Add-Type -MemberDefinition $member -Name NativeMethods -Namespace CSA -PassThru
      $result = [UIntPtr]::Zero
      [void]$native::SendMessageTimeout([IntPtr]0xffff, 0x1a, [UIntPtr]::Zero, 'Environment', 2, 5000, [ref]$result)
    } catch {}
  }
  $saved = [string]$key.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
  $savedEntries = @($saved -split ';' | Where-Object { $_ })
  $managedMatches = @($savedEntries | Where-Object { $_ -ieq $managed })
  if ($mode -eq 'prepend' -and ($savedEntries.Count -eq 0 -or $savedEntries[0] -ine $managed)) {
    throw 'managed bin is not first in the saved user PATH'
  }
  if ($mode -eq 'remove' -and $managedMatches.Count -ne 0) {
    throw 'managed bin remains in the saved user PATH'
  }
  $machinePath = [Environment]::GetEnvironmentVariable('Path', [System.EnvironmentVariableTarget]::Machine)
  $userPath = [Environment]::ExpandEnvironmentVariables($saved)
  $effectivePath = (@([string]$machinePath, [string]$userPath) | Where-Object { $_ }) -join ';'
  $result = [ordered]@{ changed = [bool]$changed; effective_path = $effectivePath }
  [Console]::Out.Write(($result | ConvertTo-Json -Compress))
}
finally { $key.Dispose() }
"#;

#[cfg(windows)]
const WINDOWS_MACHINE_PATH_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$mode = $env:CSA_MACHINE_PATH_MODE
if ($mode -notin @('install', 'remove')) { throw 'invalid CSA_MACHINE_PATH_MODE' }
$programFiles = [Environment]::GetFolderPath([Environment+SpecialFolder]::ProgramFiles)
$machineBin = [IO.Path]::GetFullPath((Join-Path $programFiles 'DSLZL\CSA\bin'))
$destination = Join-Path $machineBin 'codex.exe'
$source = if ($mode -eq 'install') { [IO.Path]::GetFullPath($env:CSA_SHIM_SOURCE) } else { '' }
if ($mode -eq 'install' -and -not (Test-Path -LiteralPath $source -PathType Leaf)) {
  throw 'CSA shim source is missing'
}
$beforePath = [string][Environment]::GetEnvironmentVariable('Path', [EnvironmentVariableTarget]::Machine)
$beforeHash = if (Test-Path -LiteralPath $destination -PathType Leaf) {
  (Get-FileHash -LiteralPath $destination -Algorithm SHA256).Hash
} else { '' }
$registered = @($beforePath -split ';' | Where-Object { $_ -and $_ -ieq $machineBin }).Count -gt 0
$needsElevation = $mode -eq 'install' -or $registered -or (Test-Path -LiteralPath $destination)
if ($needsElevation) {
  $source64 = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($source))
  $childTemplate = @'
$ErrorActionPreference = 'Stop'
$mode = '__MODE__'
$source = [Text.Encoding]::Unicode.GetString([Convert]::FromBase64String('__SOURCE__'))
$programFiles = [Environment]::GetFolderPath([Environment+SpecialFolder]::ProgramFiles)
$machineBin = [IO.Path]::GetFullPath((Join-Path $programFiles 'DSLZL\CSA\bin'))
$destination = Join-Path $machineBin 'codex.exe'
$staged = Join-Path $machineBin '.csa.staging.exe'
$registryPath = 'SYSTEM\CurrentControlSet\Control\Session Manager\Environment'
$key = [Microsoft.Win32.Registry]::LocalMachine.OpenSubKey($registryPath, $true)
if ($null -eq $key) { throw 'cannot open the machine environment registry key' }
try {
  $current = [string]$key.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
  try { $kind = $key.GetValueKind('Path') }
  catch { $kind = [Microsoft.Win32.RegistryValueKind]::ExpandString }
  $entries = @($current -split ';' | Where-Object { $_ -and $_ -ine $machineBin })
  if ($mode -eq 'install') {
    if (-not (Test-Path -LiteralPath $source -PathType Leaf)) { throw 'CSA shim source is missing' }
    [void](New-Item -ItemType Directory -Path $machineBin -Force)
    Remove-Item -LiteralPath $staged -Force -ErrorAction SilentlyContinue
    Copy-Item -LiteralPath $source -Destination $staged
    if ((Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash -ne (Get-FileHash -LiteralPath $staged -Algorithm SHA256).Hash) {
      throw 'staged system dispatcher hash mismatch'
    }
    if (Test-Path -LiteralPath $destination -PathType Leaf) {
      if ((Get-FileHash -LiteralPath $destination -Algorithm SHA256).Hash -eq (Get-FileHash -LiteralPath $staged -Algorithm SHA256).Hash) {
        Remove-Item -LiteralPath $staged -Force
      } else {
        [IO.File]::Replace($staged, $destination, $null)
      }
    } else {
      Move-Item -LiteralPath $staged -Destination $destination
    }
    $updated = (@($machineBin) + $entries) -join ';'
  } else {
    $updated = $entries -join ';'
  }
  if ($updated -cne $current) { $key.SetValue('Path', $updated, $kind) }
}
finally { $key.Dispose() }
if ($mode -eq 'remove') {
  Remove-Item -LiteralPath $staged -Force -ErrorAction SilentlyContinue
  Remove-Item -LiteralPath $destination -Force -ErrorAction SilentlyContinue
  Remove-Item -LiteralPath $machineBin -ErrorAction SilentlyContinue
}
try {
  $member = '[DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern IntPtr SendMessageTimeout(IntPtr hWnd, uint msg, UIntPtr wParam, string lParam, uint flags, uint timeout, out UIntPtr result);'
  $native = Add-Type -MemberDefinition $member -Name NativeMethods -Namespace CSA -PassThru
  $result = [UIntPtr]::Zero
  [void]$native::SendMessageTimeout([IntPtr]0xffff, 0x1a, [UIntPtr]::Zero, 'Environment', 2, 5000, [ref]$result)
} catch {}
'@
  $child = $childTemplate.Replace('__MODE__', $mode).Replace('__SOURCE__', $source64)
  $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($child))
  try {
    $process = Start-Process -FilePath (Join-Path $PSHOME 'powershell.exe') -ArgumentList @('-NoLogo', '-NoProfile', '-NonInteractive', '-EncodedCommand', $encoded) -Verb RunAs -WindowStyle Hidden -Wait -PassThru
  }
  catch { throw "administrator permission was denied or unavailable: $($_.Exception.Message)" }
  if ($process.ExitCode -ne 0) { throw "elevated PATH helper failed with exit code $($process.ExitCode)" }
}
$afterPath = [string][Environment]::GetEnvironmentVariable('Path', [EnvironmentVariableTarget]::Machine)
$afterHash = if (Test-Path -LiteralPath $destination -PathType Leaf) {
  (Get-FileHash -LiteralPath $destination -Algorithm SHA256).Hash
} else { '' }
$machineEntries = @($afterPath -split ';' | Where-Object { $_ })
if ($mode -eq 'install' -and ($machineEntries.Count -eq 0 -or $machineEntries[0] -ine $machineBin)) {
  throw 'CSA system dispatcher is not first in the machine PATH'
}
if ($mode -eq 'install' -and -not (Test-Path -LiteralPath $destination -PathType Leaf)) {
  throw 'CSA system dispatcher was not installed'
}
if ($mode -eq 'remove' -and @($machineEntries | Where-Object { $_ -ieq $machineBin }).Count -ne 0) {
  throw 'CSA system dispatcher remains in the machine PATH'
}
if ($mode -eq 'remove' -and (Test-Path -LiteralPath $destination)) {
  throw 'CSA system dispatcher remains installed'
}
$userPath = [Environment]::ExpandEnvironmentVariables([string][Environment]::GetEnvironmentVariable('Path', [EnvironmentVariableTarget]::User))
$effectivePath = (@($afterPath, $userPath) | Where-Object { $_ }) -join ';'
$changed = $beforePath -cne $afterPath -or $beforeHash -cne $afterHash
$result = [ordered]@{ changed = [bool]$changed; effective_path = $effectivePath }
[Console]::Out.Write(($result | ConvertTo-Json -Compress))
"#;

pub fn forward_current_shim(args: Vec<OsString>, runner: &dyn ProcessRunner) -> Result<i32> {
    let current = std::env::current_exe()
        .map_err(|error| ManagerError::io("resolve current activation shim", error))?
        .canonicalize()
        .map_err(|error| ManagerError::io("canonicalize current activation shim", error))?;
    let bin = current
        .parent()
        .ok_or_else(|| ManagerError::new("unsafe_shim_path", "activation shim has no parent"))?;
    #[cfg(windows)]
    if windows_csa_system_bin()
        .ok()
        .and_then(|path| path.canonicalize().ok())
        .is_some_and(|system_bin| system_bin == bin)
    {
        let root = std::env::var_os(MANAGER_ROOT_ENV).map(PathBuf::from);
        let paths = ManagerPaths::resolve(root)?;
        let path_value = std::env::var_os("PATH");
        let filtered_path = path_value
            .as_deref()
            .map(|value| path_without(bin, value))
            .transpose()?;
        return forward_shim(
            &paths,
            args,
            filtered_path.as_deref(),
            &shim_path(&paths),
            runner,
        );
    }
    if bin.file_name() != Some(OsStr::new("bin")) {
        return Err(ManagerError::new(
            "unsafe_shim_path",
            "activation shim must be inside the manager bin directory",
        ));
    }
    let root = bin
        .parent()
        .ok_or_else(|| ManagerError::new("unsafe_shim_path", "manager bin has no parent"))?;
    let paths = ManagerPaths::resolve(Some(root.to_path_buf()))?;
    let path_value = std::env::var_os("PATH");
    forward_shim(&paths, args, path_value.as_deref(), &current, runner)
}

pub fn forward_shim(
    paths: &ManagerPaths,
    args: Vec<OsString>,
    path_value: Option<&OsStr>,
    current_shim: &Path,
    runner: &dyn ProcessRunner,
) -> Result<i32> {
    let selection = match PrepareLock::acquire(paths) {
        Ok(_lock) => select_shim_target(paths, path_value, current_shim, runner)?,
        Err(lock_error) => {
            let fallback_state = StateStore::new(paths).load().ok().flatten();
            let (target, official) = resolve_official_fallback(
                paths,
                path_value,
                current_shim,
                fallback_state.as_ref(),
                runner,
            )?;
            ShimSelection {
                mode: "official",
                target,
                official,
                compat_id: None,
                fallback_reason: Some(lock_error.to_string()),
            }
        }
    };
    if selection.mode == "patched"
        && matches!(args.as_slice(), [arg] if arg == "--version" || arg == "-V")
    {
        let compat_id = selection.compat_id.as_deref().ok_or_else(|| {
            ManagerError::new(
                "invalid_activation_state",
                "patched shim selection has no compatibility ID",
            )
        })?;
        println!("codex-cli {} (CSA {compat_id})", selection.official.version);
        return Ok(0);
    }
    let command = CommandSpec::captured(&selection.target)
        .args(args)
        .inherited();
    let command = if selection.mode == "patched" {
        patched_command(command, &selection.official)?
    } else {
        official_command(command, &selection.official)?
    };
    let result = runner.run(&command)?;
    Ok(result.code.unwrap_or(1))
}

pub fn select_shim_target(
    paths: &ManagerPaths,
    path_value: Option<&OsStr>,
    current_shim: &Path,
    runner: &dyn ProcessRunner,
) -> Result<ShimSelection> {
    let store = StateStore::new(paths);
    let prepared = store.recover().and_then(|()| store.load());
    let fallback_state = prepared
        .as_ref()
        .ok()
        .and_then(|state| state.as_ref())
        .cloned();
    let patched = (|| {
        let prepared = prepared?.ok_or_else(|| {
            ManagerError::new("not_prepared", "no verified prepared state exists")
        })?;
        let official = validate_prepared_state(&prepared, paths, runner)?;
        let active = read_active(&paths.active)?;
        if !active.binding.matches(&prepared) {
            return Err(ManagerError::new(
                "activation_state_mismatch",
                "active state does not bind the current prepared state",
            ));
        }
        let current = fingerprint(current_shim)?;
        if current.sha256 != active.shim_sha256 || current.size != active.shim_size {
            return Err(ManagerError::new(
                "shim_hash_mismatch",
                "running shim does not match active state",
            ));
        }
        Ok((prepared.artifact_path, official, prepared.compat_id))
    })();

    match patched {
        Ok((target, official, compat_id)) => Ok(ShimSelection {
            mode: "patched",
            target,
            official,
            compat_id: Some(compat_id),
            fallback_reason: None,
        }),
        Err(error) => {
            let (target, official) = resolve_official_fallback(
                paths,
                path_value,
                current_shim,
                fallback_state.as_ref(),
                runner,
            )?;
            Ok(ShimSelection {
                mode: "official",
                target,
                official,
                compat_id: None,
                fallback_reason: Some(error.to_string()),
            })
        }
    }
}

pub fn recover(paths: &ManagerPaths) -> Result<bool> {
    let mut changed = false;
    for path in [
        paths.root.join("active.json.next"),
        paths.bin.join(STAGED_SHIM_NAME),
        paths.bin.join(REMOVED_SHIM_NAME),
    ] {
        if owned_path_exists(&path)? {
            remove_owned_file(&path)?;
            changed = true;
        }
    }
    let active = owned_path_exists(&paths.active)?;
    let shim = owned_path_exists(&shim_path(paths))?;
    if active && !shim {
        remove_owned_file(&paths.active)?;
        changed = true;
    } else if shim && !active {
        withdraw_shim(paths)?;
        changed = true;
    }
    Ok(changed)
}

fn publish_active(paths: &ManagerPaths, state: &ActivationState) -> Result<()> {
    let next = paths.root.join("active.json.next");
    remove_owned_file(&next)?;
    let bytes = serde_json::to_vec_pretty(state)
        .map_err(|error| ManagerError::new("invalid_activation_state", error.to_string()))?;
    write_new_synced(&next, &bytes)?;
    fs::rename(&next, &paths.active)
        .map_err(|error| ManagerError::io("publish active state", error))?;
    sync_directory(&paths.root)
}

fn read_active(path: &Path) -> Result<ActivationState> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        ManagerError::io(&format!("inspect active state {}", path.display()), error)
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ManagerError::new(
            "invalid_activation_state",
            format!("active state is not a real file: {}", path.display()),
        ));
    }
    let bytes = fs::read(path).map_err(|error| {
        ManagerError::io(&format!("read active state {}", path.display()), error)
    })?;
    let state: ActivationState = serde_json::from_slice(&bytes).map_err(|error| {
        ManagerError::new(
            "invalid_activation_state",
            format!("active state JSON: {error}"),
        )
    })?;
    if !matches!(state.schema, 1 | 2) {
        return Err(ManagerError::new(
            "invalid_activation_state",
            format!("unsupported active state schema: {}", state.schema),
        ));
    }
    Ok(state)
}

fn reject_shim_source(
    source: &FileFingerprint,
    paths: &ManagerPaths,
    prepared: &PreparedState,
    official: &OfficialCodex,
) -> Result<()> {
    let conflicts = source.path.starts_with(&paths.root)
        || source.path == prepared.artifact_path
        || source.path == official.executable.path
        || official
            .native
            .as_ref()
            .is_some_and(|native| source.path == native.path)
        || official.runtime.as_ref().is_some_and(|runtime| {
            source.path.starts_with(&runtime.package_root)
                || source.path.starts_with(&runtime.managed_package_root)
        });
    if conflicts {
        return Err(ManagerError::new(
            "unsafe_shim_source",
            "shim source must be a distinct manager executable outside managed and Codex paths",
        ));
    }
    Ok(())
}

fn resolve_official_fallback(
    paths: &ManagerPaths,
    path_value: Option<&OsStr>,
    current_shim: &Path,
    prepared: Option<&PreparedState>,
    runner: &dyn ProcessRunner,
) -> Result<(PathBuf, OfficialCodex)> {
    if let Ok(launcher) = find_codex_launcher(path_value, std::slice::from_ref(&paths.root))
        && let Ok(official) = detect_official(
            runner,
            Some(&launcher),
            None,
            std::slice::from_ref(&paths.root),
        )
    {
        let target = official_target(&official);
        if safe_fallback(&target, paths, current_shim, prepared) {
            return Ok((target, official));
        }
    }
    if let Some(prepared) = prepared {
        let saved = &prepared.official;
        if let Ok(official) = detect_official(
            runner,
            Some(&saved.executable.path),
            saved.native.as_ref().map(|native| native.path.as_path()),
            std::slice::from_ref(&paths.root),
        ) {
            let target = official_target(&official);
            if safe_fallback(&target, paths, current_shim, Some(prepared)) {
                return Ok((target, official));
            }
        }
        if fingerprint(&saved.executable.path).is_ok_and(|current| current == saved.executable)
            && safe_fallback(&saved.executable.path, paths, current_shim, Some(prepared))
        {
            return Ok((saved.executable.path.clone(), saved.clone()));
        }
    }
    Err(ManagerError::new(
        "official_fallback_unavailable",
        "could not resolve a safe official Codex outside the managed activation tree",
    ))
}

fn official_target(official: &OfficialCodex) -> PathBuf {
    official
        .runtime
        .as_ref()
        .and(official.native.as_ref())
        .map_or_else(
            || official.executable.path.clone(),
            |native| native.path.clone(),
        )
}

fn safe_fallback(
    candidate: &Path,
    paths: &ManagerPaths,
    current_shim: &Path,
    prepared: Option<&PreparedState>,
) -> bool {
    !candidate.starts_with(&paths.root)
        && candidate != current_shim
        && prepared.is_none_or(|state| candidate != state.artifact_path)
}

fn deactivate_locked(paths: &ManagerPaths) -> Result<bool> {
    let changed = owned_path_exists(&shim_path(paths))? || owned_path_exists(&paths.active)?;
    withdraw_shim(paths)?;
    remove_owned_file(&paths.active)?;
    remove_owned_file(&paths.root.join("active.json.next"))?;
    sync_directory(&paths.root)?;
    Ok(changed)
}

fn withdraw_shim(paths: &ManagerPaths) -> Result<()> {
    let final_shim = shim_path(paths);
    if !owned_path_exists(&final_shim)? {
        return Ok(());
    }
    let removed = paths.bin.join(REMOVED_SHIM_NAME);
    remove_owned_file(&removed)?;
    let metadata = fs::symlink_metadata(&final_shim).map_err(|error| {
        ManagerError::io(&format!("inspect shim {}", final_shim.display()), error)
    })?;
    if metadata.is_dir() {
        return Err(ManagerError::new(
            "unsafe_shim_path",
            format!("managed shim path is a directory: {}", final_shim.display()),
        ));
    }
    fs::rename(&final_shim, &removed)
        .map_err(|error| ManagerError::io("withdraw activation shim", error))?;
    remove_owned_file(&removed)?;
    sync_directory(&paths.bin)
}

fn remove_owned_file(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => Err(ManagerError::new(
            "unsafe_managed_file",
            format!("managed file path is a directory: {}", path.display()),
        )),
        Ok(_) => fs::remove_file(path)
            .map_err(|error| ManagerError::io(&format!("remove {}", path.display()), error)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(ManagerError::io(
            &format!("inspect {}", path.display()),
            error,
        )),
    }
}

fn copy_synced(source: &Path, destination: &Path) -> Result<()> {
    let mut input = File::open(source)
        .map_err(|error| ManagerError::io(&format!("open {}", source.display()), error))?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|error| ManagerError::io(&format!("create {}", destination.display()), error))?;
    io::copy(&mut input, &mut output)
        .map_err(|error| ManagerError::io("copy activation shim", error))?;
    let permissions = fs::metadata(source)
        .map_err(|error| ManagerError::io(&format!("stat {}", source.display()), error))?
        .permissions();
    fs::set_permissions(destination, permissions)
        .map_err(|error| ManagerError::io("set activation shim permissions", error))?;
    output
        .sync_all()
        .map_err(|error| ManagerError::io("sync staged activation shim", error))
}

fn owned_path_exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(ManagerError::io(
            &format!("inspect {}", path.display()),
            error,
        )),
    }
}

fn managed_data_exists(paths: &ManagerPaths) -> Result<bool> {
    let shim = shim_path(paths);
    if [
        paths.state.as_path(),
        paths.active.as_path(),
        shim.as_path(),
    ]
    .iter()
    .any(|path| fs::symlink_metadata(path).is_ok())
    {
        return Ok(true);
    }
    for directory in [
        &paths.artifacts,
        &paths.manifests,
        &paths.downloads,
        &paths.sources,
        &paths.builds,
    ] {
        match fs::read_dir(directory) {
            Ok(entries) => {
                if entries.into_iter().next().is_some() {
                    return Ok(true);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(ManagerError::io(
                    &format!("inspect managed directory {}", directory.display()),
                    error,
                ));
            }
        }
    }
    Ok(false)
}

fn path_contains(directory: &Path, path_value: Option<&OsStr>) -> bool {
    let canonical = directory.canonicalize().ok();
    path_value.is_some_and(|value| {
        std::env::split_paths(value).any(|entry| {
            entry == directory
                || canonical.as_ref().is_some_and(|expected| {
                    entry.canonicalize().is_ok_and(|actual| &actual == expected)
                })
        })
    })
}

#[cfg(windows)]
fn path_without(directory: &Path, path_value: &OsStr) -> Result<OsString> {
    let canonical = directory.canonicalize().ok();
    std::env::join_paths(std::env::split_paths(path_value).filter(|entry| {
        !entry
            .as_os_str()
            .eq_ignore_ascii_case(directory.as_os_str())
            && !canonical.as_ref().is_some_and(|expected| {
                entry.canonicalize().is_ok_and(|actual| &actual == expected)
            })
    }))
    .map_err(|error| ManagerError::new("invalid_path", format!("rebuild PATH: {error}")))
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| ManagerError::io(&format!("sync directory {}", path.display()), error))
}

#[cfg(not(unix))]
fn sync_directory(_: &Path) -> Result<()> {
    Ok(())
}
