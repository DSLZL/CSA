#[cfg(windows)]
use crate::BUILD_TARGET;
use crate::error::{ManagerError, Result};
use crate::hash::sha256_file;
use crate::process::{CommandSpec, ProcessRunner};
use serde::{Deserialize, Serialize};
#[cfg(windows)]
use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileFingerprint {
    pub path: PathBuf,
    pub sha256: String,
    pub size: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PackageManager {
    Npm,
    Bun,
    Pnpm,
}

impl PackageManager {
    pub fn environment_key(self) -> &'static str {
        match self {
            Self::Npm => "CODEX_MANAGED_BY_NPM",
            Self::Bun => "CODEX_MANAGED_BY_BUN",
            Self::Pnpm => "CODEX_MANAGED_BY_PNPM",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OfficialRuntime {
    pub package_root: PathBuf,
    pub managed_package_root: PathBuf,
    pub package_manager: PackageManager,
    pub files: Vec<FileFingerprint>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OfficialCodex {
    pub executable: FileFingerprint,
    pub version: String,
    pub native: Option<FileFingerprint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<OfficialRuntime>,
}

#[cfg(windows)]
pub(crate) fn windows_csa_system_bin() -> Result<PathBuf> {
    let program_files = std::env::var_os("ProgramFiles")
        .map(PathBuf::from)
        .filter(|path| {
            path.is_absolute()
                && !path.components().any(|component| {
                    matches!(
                        component,
                        std::path::Component::CurDir | std::path::Component::ParentDir
                    )
                })
        })
        .ok_or_else(|| {
            ManagerError::new(
                "windows_program_files_missing",
                "ProgramFiles is unavailable or invalid",
            )
        })?;
    Ok(program_files.join("DSLZL").join("CSA").join("bin"))
}

pub fn detect_official(
    runner: &dyn ProcessRunner,
    explicit: Option<&Path>,
    explicit_native: Option<&Path>,
    excluded_roots: &[PathBuf],
) -> Result<OfficialCodex> {
    let mut effective_excluded_roots = excluded_roots.to_vec();
    #[cfg(windows)]
    if let Ok(system_bin) = windows_csa_system_bin() {
        effective_excluded_roots.push(system_bin);
    }
    let excluded_roots = effective_excluded_roots.as_slice();
    let executable_path = match explicit {
        Some(path) => canonical_launcher(path)?,
        None => find_codex_launcher(std::env::var_os("PATH").as_deref(), excluded_roots)?,
    };
    reject_excluded(&executable_path, excluded_roots)?;
    let executable = fingerprint_file(&executable_path)?;

    #[cfg(windows)]
    let launcher_version = platform_file_executable(&executable_path)
        .then(|| executable_version(runner, &executable_path))
        .transpose()?;
    #[cfg(not(windows))]
    let launcher_version = Some(executable_version(runner, &executable_path)?);

    #[cfg(windows)]
    let discovered = discover_windows_runtime(
        &executable_path,
        explicit_native,
        launcher_version.as_deref(),
        excluded_roots,
    )?;
    #[cfg(not(windows))]
    let discovered: Option<(FileFingerprint, OfficialRuntime, String)> = None;

    let (native, runtime, runtime_version) = match discovered {
        Some((native, runtime, version)) => (Some(native), Some(runtime), Some(version)),
        None => {
            #[cfg(windows)]
            if explicit_native.is_some() {
                return Err(ManagerError::new(
                    "official_runtime_incomplete",
                    "the explicit official native executable is not inside a complete Codex platform package",
                ));
            }
            let native = explicit_native
                .map(|path| {
                    let path = canonical_executable(path)?;
                    reject_excluded(&path, excluded_roots)?;
                    fingerprint(&path)
                })
                .transpose()?;
            (native, None, None)
        }
    };

    let native_version = native
        .as_ref()
        .map(|native| executable_version(runner, &native.path))
        .transpose()?;
    let version = launcher_version
        .as_ref()
        .or(runtime_version.as_ref())
        .or(native_version.as_ref())
        .cloned()
        .ok_or_else(|| {
            ManagerError::new(
                "official_runtime_incomplete",
                "could not locate a complete official Codex platform package",
            )
        })?;
    for (label, candidate) in [
        ("launcher", launcher_version.as_deref()),
        ("native binary", native_version.as_deref()),
        ("package marker", runtime_version.as_deref()),
    ] {
        if candidate.is_some_and(|candidate| candidate != version) {
            return Err(ManagerError::new(
                "official_version_mismatch",
                format!("official {label} does not match launcher version {version}"),
            ));
        }
    }

    Ok(OfficialCodex {
        executable,
        version,
        native,
        runtime,
    })
}

pub(crate) fn find_codex_launcher(
    path_value: Option<&OsStr>,
    excluded_roots: &[PathBuf],
) -> Result<PathBuf> {
    if let Ok(path) = find_executable("codex", path_value, excluded_roots) {
        return Ok(path);
    }
    #[cfg(windows)]
    {
        let path_value = path_value.ok_or_else(|| {
            ManagerError::new("official_not_found", "PATH is unavailable; pass --official")
        })?;
        let excluded: Vec<_> = excluded_roots
            .iter()
            .filter_map(|path| path.canonicalize().ok())
            .collect();
        for directory in std::env::split_paths(path_value) {
            for name in ["codex.cmd", "codex.bat", "codex.ps1"] {
                let candidate = directory.join(name);
                let Ok(canonical) = canonical_launcher(&candidate) else {
                    continue;
                };
                if !excluded.iter().any(|root| canonical.starts_with(root)) {
                    return Ok(canonical);
                }
            }
        }
    }
    Err(ManagerError::new(
        "official_not_found",
        "could not resolve codex to a safe launcher; pass an absolute path",
    ))
}

fn canonical_launcher(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        return Err(ManagerError::new(
            "unsafe_executable_path",
            format!("executable path must be absolute: {}", path.display()),
        ));
    }
    let canonical = path.canonicalize().map_err(|error| {
        ManagerError::io(
            &format!("canonicalize executable {}", path.display()),
            error,
        )
    })?;
    let metadata = fs::metadata(&canonical).map_err(|error| {
        ManagerError::io(&format!("stat executable {}", canonical.display()), error)
    })?;
    #[cfg(windows)]
    let supported = platform_executable(&canonical, &metadata)
        || canonical
            .extension()
            .and_then(OsStr::to_str)
            .is_some_and(|extension| {
                ["cmd", "bat", "ps1"]
                    .iter()
                    .any(|candidate| extension.eq_ignore_ascii_case(candidate))
            });
    #[cfg(not(windows))]
    let supported = platform_executable(&canonical, &metadata);
    if !metadata.is_file() || !supported {
        return Err(ManagerError::new(
            "unsafe_executable_path",
            format!("not a supported Codex launcher: {}", canonical.display()),
        ));
    }
    Ok(canonical)
}

#[cfg(windows)]
fn platform_file_executable(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("exe") || extension.eq_ignore_ascii_case("com")
        })
}

#[cfg(windows)]
fn discover_windows_runtime(
    launcher: &Path,
    explicit_native: Option<&Path>,
    expected_version: Option<&str>,
    excluded_roots: &[PathBuf],
) -> Result<Option<(FileFingerprint, OfficialRuntime, String)>> {
    let explicit_root = explicit_native
        .map(canonical_executable)
        .transpose()?
        .and_then(|native| {
            native
                .parent()
                .and_then(Path::parent)
                .map(Path::to_path_buf)
        });
    let meta_roots = official_meta_roots(launcher, explicit_root.as_deref());
    let mut candidates = Vec::new();
    let mut seen = BTreeSet::new();

    for meta_root in meta_roots {
        for package_root in platform_roots(&meta_root, explicit_root.as_deref()) {
            let (native, runtime, version) = match validate_windows_runtime(
                &package_root,
                &meta_root,
                expected_version,
                excluded_roots,
            ) {
                Ok(runtime) => runtime,
                Err(error) if error.code == "official_in_manager_root" => return Err(error),
                Err(_) => continue,
            };
            let key = (
                runtime.package_root.clone(),
                runtime.managed_package_root.clone(),
            );
            if seen.insert(key) {
                candidates.push((native, runtime, version));
            }
        }
    }

    match candidates.len() {
        0 => Ok(None),
        1 => Ok(candidates.pop()),
        count => Err(ManagerError::new(
            "official_runtime_ambiguous",
            format!("found {count} complete official Codex platform packages for one launcher"),
        )),
    }
}

#[cfg(windows)]
fn official_meta_roots(launcher: &Path, package_root: Option<&Path>) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let launcher_dir = launcher.parent().unwrap_or(launcher);
    for ancestor in launcher_dir.ancestors() {
        roots.push(ancestor.join("node_modules/@openai/codex"));
        roots.push(ancestor.join("install/global/node_modules/@openai/codex"));
    }
    let pnpm_global = launcher_dir.join("global");
    if let Ok(entries) = fs::read_dir(pnpm_global) {
        roots.extend(
            entries
                .flatten()
                .map(|entry| entry.path().join("node_modules/@openai/codex")),
        );
    }
    if let Some(package_root) = package_root {
        for ancestor in package_root.ancestors() {
            if ancestor.file_name() == Some(OsStr::new("node_modules")) {
                roots.push(ancestor.join("@openai/codex"));
            }
        }
    }
    dedupe_existing_directories(roots)
}

#[cfg(windows)]
fn platform_roots(meta_root: &Path, explicit_root: Option<&Path>) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(root) = explicit_root {
        roots.push(root.to_path_buf());
    }
    roots.push(
        meta_root
            .join("node_modules/@openai/codex-win32-x64/vendor")
            .join(BUILD_TARGET),
    );
    roots.push(meta_root.join("vendor").join(BUILD_TARGET));
    if let Some(node_modules) = meta_root.parent().and_then(Path::parent) {
        roots.push(
            node_modules
                .join("@openai/codex-win32-x64/vendor")
                .join(BUILD_TARGET),
        );
    }
    dedupe_existing_directories(roots)
}

#[cfg(windows)]
fn dedupe_existing_directories(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = BTreeSet::new();
    paths
        .into_iter()
        .filter_map(|path| path.canonicalize().ok())
        .filter(|path| path.is_dir() && seen.insert(path.clone()))
        .collect()
}

#[cfg(windows)]
#[derive(Deserialize)]
struct PackageManifest {
    name: String,
    version: String,
}

#[cfg(windows)]
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PackageLayout {
    layout_version: u32,
    version: String,
    target: String,
    variant: String,
    entrypoint: String,
    resources_dir: String,
    path_dir: String,
}

#[cfg(windows)]
fn validate_windows_runtime(
    package_root: &Path,
    meta_root: &Path,
    expected_version: Option<&str>,
    excluded_roots: &[PathBuf],
) -> Result<(FileFingerprint, OfficialRuntime, String)> {
    let package_root = package_root.canonicalize().map_err(|error| {
        ManagerError::io(
            &format!(
                "canonicalize official package root {}",
                package_root.display()
            ),
            error,
        )
    })?;
    let managed_package_root = meta_root.canonicalize().map_err(|error| {
        ManagerError::io(
            &format!(
                "canonicalize official managed package {}",
                meta_root.display()
            ),
            error,
        )
    })?;
    reject_excluded(&package_root, excluded_roots)?;
    reject_excluded(&managed_package_root, excluded_roots)?;

    let package_json_path = managed_package_root.join("package.json");
    let package_json: PackageManifest = read_json_file(&package_json_path)?;
    if package_json.name != "@openai/codex" {
        return Err(ManagerError::new(
            "official_runtime_incomplete",
            "managed package is not @openai/codex",
        ));
    }

    let marker_path = package_root.join("codex-package.json");
    let marker: PackageLayout = read_json_file(&marker_path)?;
    if marker.layout_version != 1
        || marker.target != BUILD_TARGET
        || marker.variant != "codex"
        || marker.entrypoint != "bin/codex.exe"
        || marker.resources_dir != "codex-resources"
        || marker.path_dir != "codex-path"
        || marker.version != package_json.version
        || expected_version.is_some_and(|expected| marker.version != expected)
    {
        return Err(ManagerError::new(
            "official_runtime_incomplete",
            "official Codex package metadata does not match the selected runtime",
        ));
    }

    let native = fingerprint(&package_root.join("bin/codex.exe"))?;
    let mut files = vec![
        fingerprint_file(&marker_path)?,
        fingerprint_file(&package_json_path)?,
        fingerprint_file(&package_root.join("bin/codex-code-mode-host.exe"))?,
        fingerprint_file(&package_root.join("codex-resources/codex-command-runner.exe"))?,
        fingerprint_file(&package_root.join("codex-resources/codex-windows-sandbox-setup.exe"))?,
        fingerprint_file(&package_root.join("codex-path/rg.exe"))?,
    ];
    for file in &files {
        reject_excluded(&file.path, excluded_roots)?;
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));

    Ok((
        native,
        OfficialRuntime {
            package_root,
            managed_package_root: managed_package_root.clone(),
            package_manager: package_manager(&managed_package_root),
            files,
        },
        marker.version,
    ))
}

#[cfg(windows)]
fn read_json_file<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let bytes = fs::read(path)
        .map_err(|error| ManagerError::io(&format!("read {}", path.display()), error))?;
    serde_json::from_slice(&bytes).map_err(|error| {
        ManagerError::new(
            "official_runtime_incomplete",
            format!("invalid {}: {error}", path.display()),
        )
    })
}

#[cfg(windows)]
fn package_manager(path: &Path) -> PackageManager {
    let components: Vec<_> = path
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect();
    if components
        .iter()
        .any(|component| component.eq_ignore_ascii_case(".bun"))
    {
        PackageManager::Bun
    } else if components.iter().any(|component| {
        component.eq_ignore_ascii_case("pnpm") || component.eq_ignore_ascii_case(".pnpm")
    }) {
        PackageManager::Pnpm
    } else {
        PackageManager::Npm
    }
}

fn reject_excluded(path: &Path, excluded_roots: &[PathBuf]) -> Result<()> {
    if excluded_roots
        .iter()
        .filter_map(|root| root.canonicalize().ok())
        .any(|root| path.starts_with(&root) || root.starts_with(path))
    {
        return Err(ManagerError::new(
            "official_in_manager_root",
            format!(
                "official path overlaps the managed tree: {}",
                path.display()
            ),
        ));
    }
    Ok(())
}

pub fn fingerprint(path: &Path) -> Result<FileFingerprint> {
    let path = canonical_executable(path)?;
    fingerprint_canonical_file(path)
}

pub fn fingerprint_file(path: &Path) -> Result<FileFingerprint> {
    if !path.is_absolute() {
        return Err(ManagerError::new(
            "unsafe_file_path",
            format!("file path must be absolute: {}", path.display()),
        ));
    }
    let path = path.canonicalize().map_err(|error| {
        ManagerError::io(&format!("canonicalize file {}", path.display()), error)
    })?;
    let metadata = fs::metadata(&path)
        .map_err(|error| ManagerError::io(&format!("stat file {}", path.display()), error))?;
    if !metadata.is_file() {
        return Err(ManagerError::new(
            "unsafe_file_path",
            format!("not a regular file: {}", path.display()),
        ));
    }
    fingerprint_canonical_file(path)
}

fn fingerprint_canonical_file(path: PathBuf) -> Result<FileFingerprint> {
    let (sha256, size) = sha256_file(&path)?;
    Ok(FileFingerprint { path, sha256, size })
}

pub fn find_executable(
    name: &str,
    path_value: Option<&OsStr>,
    excluded_roots: &[PathBuf],
) -> Result<PathBuf> {
    let path_value = path_value.ok_or_else(|| {
        ManagerError::new("official_not_found", "PATH is unavailable; pass --official")
    })?;
    let excluded: Vec<_> = excluded_roots
        .iter()
        .filter_map(|path| path.canonicalize().ok())
        .collect();
    for directory in std::env::split_paths(path_value) {
        for candidate_name in candidate_names(name) {
            let candidate = directory.join(candidate_name);
            let Ok(canonical) = canonical_executable(&candidate) else {
                continue;
            };
            if excluded.iter().any(|root| canonical.starts_with(root)) {
                continue;
            }
            return Ok(canonical);
        }
    }
    Err(ManagerError::new(
        "official_not_found",
        format!("could not resolve {name} to a safe executable; pass an absolute path"),
    ))
}

fn executable_version(runner: &dyn ProcessRunner, path: &Path) -> Result<String> {
    let result = runner
        .run(&CommandSpec::captured(path).arg("--version"))?
        .require_success("official Codex --version")?;
    let bytes = if result.stdout.is_empty() {
        &result.stderr
    } else {
        &result.stdout
    };
    let text = std::str::from_utf8(bytes).map_err(|_| {
        ManagerError::new("invalid_official_version", "version output is not UTF-8")
    })?;
    parse_codex_version(text)
}

pub fn parse_codex_version(text: &str) -> Result<String> {
    let value = text.trim();
    let version = value.strip_prefix("codex-cli ").ok_or_else(|| {
        ManagerError::new(
            "invalid_official_version",
            format!("expected 'codex-cli X.Y.Z', got {value:?}"),
        )
    })?;
    let parts: Vec<_> = version.split('.').collect();
    if parts.len() != 3
        || parts
            .iter()
            .any(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return Err(ManagerError::new(
            "invalid_official_version",
            format!("expected exact X.Y.Z version, got {version:?}"),
        ));
    }
    Ok(version.to_owned())
}

fn canonical_executable(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        return Err(ManagerError::new(
            "unsafe_executable_path",
            format!("executable path must be absolute: {}", path.display()),
        ));
    }
    let canonical = path.canonicalize().map_err(|error| {
        ManagerError::io(
            &format!("canonicalize executable {}", path.display()),
            error,
        )
    })?;
    let metadata = fs::metadata(&canonical).map_err(|error| {
        ManagerError::io(&format!("stat executable {}", canonical.display()), error)
    })?;
    if !metadata.is_file() || !platform_executable(&canonical, &metadata) {
        return Err(ManagerError::new(
            "unsafe_executable_path",
            format!("not an executable file: {}", canonical.display()),
        ));
    }
    Ok(canonical)
}

#[cfg(windows)]
fn candidate_names(name: &str) -> Vec<OsString> {
    if Path::new(name).extension().is_some() {
        vec![OsString::from(name)]
    } else {
        vec![
            OsString::from(format!("{name}.exe")),
            OsString::from(format!("{name}.com")),
        ]
    }
}

#[cfg(not(windows))]
fn candidate_names(name: &str) -> Vec<OsString> {
    vec![OsString::from(name)]
}

#[cfg(windows)]
fn platform_executable(path: &Path, _: &fs::Metadata) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("exe") || extension.eq_ignore_ascii_case("com")
        })
}

#[cfg(unix)]
fn platform_executable(_: &Path, metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    use super::{PackageManager, detect_official, find_codex_launcher};
    use super::{find_executable, parse_codex_version};
    #[cfg(windows)]
    use crate::BUILD_TARGET;
    #[cfg(windows)]
    use crate::error::Result;
    #[cfg(windows)]
    use crate::process::{CommandResult, CommandSpec, ProcessRunner};
    use std::fs;
    #[cfg(windows)]
    use std::path::Path;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn version_parser_is_exact() {
        assert_eq!(
            parse_codex_version("codex-cli 0.147.0\n").unwrap(),
            "0.147.0"
        );
        assert!(parse_codex_version("codex 0.147.0").is_err());
        assert!(parse_codex_version("codex-cli 0.147").is_err());
        assert!(parse_codex_version("codex-cli 0.147.0-beta").is_err());
    }

    #[test]
    fn path_resolution_is_absolute_and_honors_exclusions() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("csa-resolve-{}-{unique}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let executable = directory.join(if cfg!(windows) { "codex.exe" } else { "codex" });
        fs::write(&executable, b"fixture").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&executable).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&executable, permissions).unwrap();
        }
        let path = std::env::join_paths([&directory]).unwrap();
        assert_eq!(
            find_executable("codex", Some(&path), &[]).unwrap(),
            executable.canonicalize().unwrap()
        );
        assert!(find_executable("codex", Some(&path), std::slice::from_ref(&directory)).is_err());
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(windows)]
    fn write_windows_runtime(managed: &Path, package: &Path, target: &str) {
        fs::create_dir_all(managed).unwrap();
        fs::create_dir_all(package.join("bin")).unwrap();
        fs::create_dir_all(package.join("codex-resources")).unwrap();
        fs::create_dir_all(package.join("codex-path")).unwrap();
        fs::write(
            managed.join("package.json"),
            br#"{"name":"@openai/codex","version":"0.149.0"}"#,
        )
        .unwrap();
        fs::write(
            package.join("codex-package.json"),
            format!(
                r#"{{"layoutVersion":1,"version":"0.149.0","target":"{target}","variant":"codex","entrypoint":"bin/codex.exe","resourcesDir":"codex-resources","pathDir":"codex-path"}}"#
            ),
        )
        .unwrap();
        for relative in [
            "bin/codex.exe",
            "bin/codex-code-mode-host.exe",
            "codex-resources/codex-command-runner.exe",
            "codex-resources/codex-windows-sandbox-setup.exe",
            "codex-path/rg.exe",
        ] {
            fs::write(package.join(relative), relative.as_bytes()).unwrap();
        }
    }

    #[cfg(windows)]
    #[test]
    fn discovers_complete_npm_bun_and_pnpm_platform_packages() {
        struct VersionRunner;
        impl ProcessRunner for VersionRunner {
            fn run(&self, _: &CommandSpec) -> Result<CommandResult> {
                Ok(CommandResult::success("codex-cli 0.149.0\n"))
            }
        }

        for (name, expected) in [
            ("npm", PackageManager::Npm),
            ("bun", PackageManager::Bun),
            ("pnpm", PackageManager::Pnpm),
        ] {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir()
                .join(format!(
                    "csa-pnpm-looking-parent-{}-{unique}",
                    std::process::id()
                ))
                .join(name);
            let (launcher_dir, node_modules, launcher_name) = match name {
                "npm" => (root.join("npm"), root.join("npm/node_modules"), "codex.cmd"),
                "bun" => (
                    root.join(".bun/bin"),
                    root.join(".bun/install/global/node_modules"),
                    "codex.exe",
                ),
                "pnpm" => (
                    root.join("pnpm"),
                    root.join("pnpm/global/5/node_modules"),
                    "codex.cmd",
                ),
                _ => unreachable!(),
            };
            let managed = node_modules.join("@openai/codex");
            let package = node_modules
                .join("@openai/codex-win32-x64/vendor")
                .join(BUILD_TARGET);
            fs::create_dir_all(&launcher_dir).unwrap();
            fs::write(launcher_dir.join(launcher_name), b"launcher").unwrap();
            write_windows_runtime(&managed, &package, BUILD_TARGET);

            let path = std::env::join_paths([&launcher_dir]).unwrap();
            let launcher = find_codex_launcher(Some(&path), &[]).unwrap();
            let official = detect_official(&VersionRunner, Some(&launcher), None, &[]).unwrap();
            let runtime = official.runtime.unwrap();
            assert_eq!(runtime.package_manager, expected);
            assert_eq!(runtime.package_root, package.canonicalize().unwrap());
            assert_eq!(
                official.native.unwrap().path,
                package.join("bin/codex.exe").canonicalize().unwrap()
            );

            if name == "npm" {
                let second_modules = root.join("node_modules");
                let second_managed = second_modules.join("@openai/codex");
                let second_package = second_modules
                    .join("@openai/codex-win32-x64/vendor")
                    .join(BUILD_TARGET);
                write_windows_runtime(&second_managed, &second_package, BUILD_TARGET);
                let error =
                    detect_official(&VersionRunner, Some(&launcher), None, &[]).unwrap_err();
                assert_eq!(error.code, "official_runtime_ambiguous");
            }
            fs::remove_dir_all(root).unwrap();
        }
    }
}
