use crate::detect::OfficialCodex;
use crate::error::{ManagerError, Result};
use crate::hash::sha256_os_str;
use crate::state::ManagerPaths;
use directories::BaseDirs;
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct IsolationRequest {
    pub codex_home: PathBuf,
    pub cwd: PathBuf,
    pub logs_dir: PathBuf,
    pub state_dir: PathBuf,
    pub npm_prefix: Option<PathBuf>,
    pub record_path: PathBuf,
}

#[derive(Clone, Debug, Serialize)]
pub struct IsolationPlan {
    pub codex_home: PathBuf,
    pub cwd: PathBuf,
    pub logs_dir: PathBuf,
    pub state_dir: PathBuf,
    pub npm_prefix: Option<PathBuf>,
    pub record_path: PathBuf,
    pub parent_path_sha256: String,
    pub path_prefix: Option<PathBuf>,
}

impl IsolationPlan {
    pub fn create(
        request: IsolationRequest,
        manager: &ManagerPaths,
        official: &OfficialCodex,
    ) -> Result<Self> {
        for (label, path) in [
            ("CODEX_HOME", &request.codex_home),
            ("cwd", &request.cwd),
            ("logs", &request.logs_dir),
            ("state", &request.state_dir),
            ("record", &request.record_path),
        ] {
            require_absolute(label, path)?;
        }
        if let Some(prefix) = &request.npm_prefix {
            require_absolute("npm prefix", prefix)?;
        }
        let mut requested = vec![
            ("CODEX_HOME", &request.codex_home),
            ("cwd", &request.cwd),
            ("logs", &request.logs_dir),
            ("state", &request.state_dir),
        ];
        if let Some(prefix) = &request.npm_prefix {
            requested.push(("npm prefix", prefix));
        }
        for (index, (left_name, left)) in requested.iter().enumerate() {
            reject_overlap(left_name, left, "manager root", &manager.root)?;
            for (right_name, right) in requested.iter().skip(index + 1) {
                reject_overlap(left_name, left, right_name, right)?;
            }
        }
        if request.record_path.starts_with(&manager.root) {
            return Err(ManagerError::new(
                "shared_isolation_path",
                "execution record must be outside the manager root",
            ));
        }
        if request.record_path.exists() {
            return Err(ManagerError::new(
                "record_exists",
                format!(
                    "refusing to overwrite execution record: {}",
                    request.record_path.display()
                ),
            ));
        }

        let cwd = existing_real_directory("cwd", &request.cwd)?;
        let codex_home = create_real_directory("CODEX_HOME", &request.codex_home)?;
        let logs_dir = create_real_directory("logs", &request.logs_dir)?;
        let state_dir = create_real_directory("state", &request.state_dir)?;
        let npm_prefix = request
            .npm_prefix
            .as_deref()
            .map(|path| create_real_directory("npm prefix", path))
            .transpose()?;
        let manager_root = manager.root.canonicalize().map_err(|error| {
            ManagerError::io(
                &format!("canonicalize manager root {}", manager.root.display()),
                error,
            )
        })?;

        let mut isolated = vec![
            ("CODEX_HOME", &codex_home),
            ("cwd", &cwd),
            ("logs", &logs_dir),
            ("state", &state_dir),
        ];
        if let Some(prefix) = &npm_prefix {
            isolated.push(("npm prefix", prefix));
        }
        for (index, (left_name, left)) in isolated.iter().enumerate() {
            reject_overlap(left_name, left, "manager root", &manager_root)?;
            reject_overlap(
                left_name,
                left,
                "official executable",
                &official.executable.path,
            )?;
            if let Some(native) = &official.native {
                reject_overlap(left_name, left, "official native executable", &native.path)?;
            }
            if let Some(runtime) = &official.runtime {
                reject_overlap(
                    left_name,
                    left,
                    "official platform package",
                    &runtime.package_root,
                )?;
                reject_overlap(
                    left_name,
                    left,
                    "official managed package",
                    &runtime.managed_package_root,
                )?;
            }
            for (right_name, right) in isolated.iter().skip(index + 1) {
                reject_overlap(left_name, left, right_name, right)?;
            }
        }

        if let Some(base) = BaseDirs::new() {
            let default_codex_home = base.home_dir().join(".codex");
            if paths_overlap(&codex_home, &default_codex_home) {
                return Err(ManagerError::new(
                    "shared_codex_home",
                    format!(
                        "isolated CODEX_HOME overlaps the default official home: {}",
                        default_codex_home.display()
                    ),
                ));
            }
        }

        let parent_path = std::env::var_os("PATH").unwrap_or_default();
        Ok(Self {
            codex_home,
            cwd,
            logs_dir,
            state_dir,
            npm_prefix,
            record_path: request.record_path,
            parent_path_sha256: sha256_os_str(&parent_path),
            path_prefix: None,
        })
    }
}

fn require_absolute(label: &str, path: &Path) -> Result<()> {
    if !path.is_absolute() {
        return Err(ManagerError::new(
            "unsafe_isolation_path",
            format!("{label} must be absolute: {}", path.display()),
        ));
    }
    Ok(())
}

fn create_real_directory(label: &str, path: &Path) -> Result<PathBuf> {
    if !path.exists() {
        fs::create_dir_all(path).map_err(|error| {
            ManagerError::io(&format!("create {label} {}", path.display()), error)
        })?;
    }
    existing_real_directory(label, path)
}

fn existing_real_directory(label: &str, path: &Path) -> Result<PathBuf> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| ManagerError::io(&format!("inspect {label} {}", path.display()), error))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ManagerError::new(
            "unsafe_isolation_path",
            format!("{label} must be a real directory: {}", path.display()),
        ));
    }
    path.canonicalize().map_err(|error| {
        ManagerError::io(&format!("canonicalize {label} {}", path.display()), error)
    })
}

fn reject_overlap(left_name: &str, left: &Path, right_name: &str, right: &Path) -> Result<()> {
    if paths_overlap(left, right) {
        return Err(ManagerError::new(
            "shared_isolation_path",
            format!(
                "{left_name} ({}) overlaps {right_name} ({})",
                left.display(),
                right.display()
            ),
        ));
    }
    Ok(())
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}
