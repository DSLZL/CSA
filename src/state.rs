use crate::detect::OfficialCodex;
use crate::error::{ManagerError, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::Component;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug)]
pub struct ManagerPaths {
    pub root: PathBuf,
    pub bin: PathBuf,
    pub artifacts: PathBuf,
    pub manifests: PathBuf,
    pub downloads: PathBuf,
    pub sources: PathBuf,
    pub builds: PathBuf,
    pub locks: PathBuf,
    pub state: PathBuf,
    pub active: PathBuf,
}

impl ManagerPaths {
    pub fn resolve(root: Option<PathBuf>) -> Result<Self> {
        let root = match root {
            Some(path) => path,
            None => ProjectDirs::from("org", "DSLZL", "csa")
                .ok_or_else(|| {
                    ManagerError::new(
                        "manager_root_unavailable",
                        "platform user-data directory is unavailable; pass --manager-root",
                    )
                })?
                .data_local_dir()
                .to_path_buf(),
        };
        if !root.is_absolute()
            || root
                .components()
                .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
        {
            return Err(ManagerError::new(
                "unsafe_manager_root",
                "manager root must be an absolute normalized path",
            ));
        }
        Ok(Self {
            bin: root.join("bin"),
            artifacts: root.join("artifacts"),
            manifests: root.join("manifests"),
            downloads: root.join("downloads"),
            sources: root.join("sources"),
            builds: root.join("builds"),
            locks: root.join("locks"),
            state: root.join("state.json"),
            active: root.join("active.json"),
            root,
        })
    }

    pub fn initialize(&self) -> Result<()> {
        ensure_directory(&self.root)?;
        for directory in [
            &self.bin,
            &self.artifacts,
            &self.manifests,
            &self.downloads,
            &self.sources,
            &self.builds,
            &self.locks,
        ] {
            ensure_directory(directory)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedState {
    pub schema: u32,
    pub compat_id: String,
    pub manifest_path: PathBuf,
    pub build_target: String,
    pub artifact_path: PathBuf,
    pub artifact_sha256: String,
    pub artifact_size: u64,
    pub official: OfficialCodex,
    pub prepared_at_unix_seconds: u64,
}

pub trait Clock: Send + Sync {
    fn unix_seconds(&self) -> Result<u64>;
}

#[derive(Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn unix_seconds(&self) -> Result<u64> {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .map_err(|error| ManagerError::new("clock_error", error.to_string()))
    }
}

pub struct PrepareLock {
    _file: File,
}

impl PrepareLock {
    pub fn acquire(paths: &ManagerPaths) -> Result<Self> {
        paths.initialize()?;
        let path = paths.locks.join("prepare.lock");
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|error| ManagerError::io(&format!("open lock {}", path.display()), error))?;
        file.try_lock().map_err(|error| match error {
            fs::TryLockError::WouldBlock => ManagerError::new(
                "prepare_locked",
                format!("another manager operation holds {}", path.display()),
            ),
            fs::TryLockError::Error(error) => {
                ManagerError::io(&format!("lock {}", path.display()), error)
            }
        })?;
        Ok(Self { _file: file })
    }
}

pub struct StateStore<'a> {
    paths: &'a ManagerPaths,
}

impl<'a> StateStore<'a> {
    pub fn new(paths: &'a ManagerPaths) -> Self {
        Self { paths }
    }

    pub fn recover(&self) -> Result<()> {
        let next = self.paths.root.join("state.json.next");
        let previous = self.paths.root.join("state.json.previous");
        if self.paths.state.exists() {
            read_state(&self.paths.state)?;
            remove_file_if_exists(&next)?;
            remove_file_if_exists(&previous)?;
            return Ok(());
        }
        if previous.exists() {
            read_state(&previous)?;
            fs::rename(&previous, &self.paths.state)
                .map_err(|error| ManagerError::io("restore previous manager state", error))?;
            remove_file_if_exists(&next)?;
        } else if next.exists() {
            read_state(&next)?;
            fs::rename(&next, &self.paths.state)
                .map_err(|error| ManagerError::io("promote staged manager state", error))?;
        }
        Ok(())
    }

    pub fn load(&self) -> Result<Option<PreparedState>> {
        if !self.paths.state.exists() {
            return Ok(None);
        }
        read_state(&self.paths.state).map(Some)
    }

    pub fn save(&self, state: &PreparedState) -> Result<()> {
        if state.schema != 1 {
            return Err(ManagerError::new(
                "invalid_state",
                "only manager state schema 1 is supported",
            ));
        }
        self.recover()?;
        let next = self.paths.root.join("state.json.next");
        let previous = self.paths.root.join("state.json.previous");
        let bytes = serde_json::to_vec_pretty(state)
            .map_err(|error| ManagerError::new("invalid_state", error.to_string()))?;
        write_new_synced(&next, &bytes)?;

        if self.paths.state.exists() {
            fs::rename(&self.paths.state, &previous)
                .map_err(|error| ManagerError::io("rotate previous manager state", error))?;
        }
        if let Err(error) = fs::rename(&next, &self.paths.state) {
            if previous.exists() {
                let _ = fs::rename(&previous, &self.paths.state);
            }
            return Err(ManagerError::io("publish manager state", error));
        }
        remove_file_if_exists(&previous)?;
        Ok(())
    }
}

pub fn write_record(path: &Path, value: &impl Serialize) -> Result<()> {
    if !path.is_absolute() {
        return Err(ManagerError::new(
            "unsafe_record_path",
            "execution record path must be absolute",
        ));
    }
    if path.exists() {
        return Err(ManagerError::new(
            "record_exists",
            format!("refusing to overwrite execution record: {}", path.display()),
        ));
    }
    let parent = path
        .parent()
        .ok_or_else(|| ManagerError::new("unsafe_record_path", "record path has no parent"))?;
    ensure_directory(parent)?;
    let filename = path
        .file_name()
        .ok_or_else(|| ManagerError::new("unsafe_record_path", "record path has no filename"))?;
    let staged = parent.join(format!(
        ".{}.staging-{}",
        filename.to_string_lossy(),
        std::process::id()
    ));
    remove_file_if_exists(&staged)?;
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| ManagerError::new("record_serialize_error", error.to_string()))?;
    write_new_synced(&staged, &bytes)?;
    if let Err(error) = fs::rename(&staged, path) {
        let _ = fs::remove_file(&staged);
        return Err(ManagerError::io("publish execution record", error));
    }
    Ok(())
}

pub fn remove_managed_tree(root: &Path, target: &Path) -> Result<()> {
    if target == root || !target.starts_with(root) {
        return Err(ManagerError::new(
            "unsafe_cleanup_path",
            format!("refusing to remove unmanaged path: {}", target.display()),
        ));
    }
    let Ok(metadata) = fs::symlink_metadata(target) else {
        return Ok(());
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ManagerError::new(
            "unsafe_cleanup_path",
            format!(
                "managed cleanup target is not a real directory: {}",
                target.display()
            ),
        ));
    }
    fs::remove_dir_all(target)
        .map_err(|error| ManagerError::io(&format!("remove {}", target.display()), error))
}

pub fn ensure_managed_directory(root: &Path, target: &Path) -> Result<()> {
    if target == root || !target.starts_with(root) {
        return Err(ManagerError::new(
            "unsafe_managed_directory",
            format!("path escapes manager root: {}", target.display()),
        ));
    }
    ensure_directory(target)
}

fn ensure_directory(path: &Path) -> Result<()> {
    if !path.exists() {
        fs::create_dir_all(path).map_err(|error| {
            ManagerError::io(&format!("create directory {}", path.display()), error)
        })?;
    }
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        ManagerError::io(&format!("inspect directory {}", path.display()), error)
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ManagerError::new(
            "unsafe_managed_directory",
            format!("managed path is not a real directory: {}", path.display()),
        ));
    }
    Ok(())
}

fn read_state(path: &Path) -> Result<PreparedState> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| ManagerError::io(&format!("inspect state {}", path.display()), error))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ManagerError::new(
            "invalid_state",
            format!("state is not a real file: {}", path.display()),
        ));
    }
    let bytes = fs::read(path)
        .map_err(|error| ManagerError::io(&format!("read state {}", path.display()), error))?;
    let state: PreparedState = serde_json::from_slice(&bytes)
        .map_err(|error| ManagerError::new("invalid_state", format!("state JSON: {error}")))?;
    if state.schema != 1 {
        return Err(ManagerError::new(
            "invalid_state",
            format!("unsupported state schema: {}", state.schema),
        ));
    }
    Ok(state)
}

pub(crate) fn write_new_synced(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| ManagerError::io(&format!("create {}", path.display()), error))?;
    let result = file
        .write_all(bytes)
        .and_then(|()| file.write_all(b"\n"))
        .and_then(|()| file.sync_all());
    if let Err(error) = result {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(ManagerError::io(
            &format!("write {}", path.display()),
            error,
        ));
    }
    Ok(())
}

pub(crate) fn remove_file_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(ManagerError::io(
            &format!("remove {}", path.display()),
            error,
        )),
    }
}
