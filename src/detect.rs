use crate::error::{ManagerError, Result};
use crate::hash::sha256_file;
use crate::process::{CommandSpec, ProcessRunner};
use serde::{Deserialize, Serialize};
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OfficialCodex {
    pub executable: FileFingerprint,
    pub version: String,
    pub native: Option<FileFingerprint>,
}

pub fn detect_official(
    runner: &dyn ProcessRunner,
    explicit: Option<&Path>,
    explicit_native: Option<&Path>,
    excluded_roots: &[PathBuf],
) -> Result<OfficialCodex> {
    let executable_path = match explicit {
        Some(path) => canonical_executable(path)?,
        None => find_executable("codex", std::env::var_os("PATH").as_deref(), excluded_roots)?,
    };
    reject_excluded(&executable_path, excluded_roots)?;
    let executable = fingerprint(&executable_path)?;
    let version = executable_version(runner, &executable_path)?;

    let native = explicit_native
        .map(|path| {
            let path = canonical_executable(path)?;
            reject_excluded(&path, excluded_roots)?;
            Ok(path)
        })
        .transpose()?
        .map(|path| {
            let native_version = executable_version(runner, &path)?;
            if native_version != version {
                return Err(ManagerError::new(
                    "official_version_mismatch",
                    format!(
                        "launcher reports {version}, but native binary reports {native_version}"
                    ),
                ));
            }
            fingerprint(&path)
        })
        .transpose()?;

    Ok(OfficialCodex {
        executable,
        version,
        native,
    })
}

fn reject_excluded(path: &Path, excluded_roots: &[PathBuf]) -> Result<()> {
    if excluded_roots
        .iter()
        .filter_map(|root| root.canonicalize().ok())
        .any(|root| path.starts_with(root))
    {
        return Err(ManagerError::new(
            "official_in_manager_root",
            format!(
                "official executable is inside the managed tree: {}",
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
    use super::{find_executable, parse_codex_version};
    use std::fs;
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
}
