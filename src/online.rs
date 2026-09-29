use crate::BUILD_TARGET;
use crate::compat::{RuntimeArtifact, RuntimeManifest, validate_relative};
use crate::detect::{detect_official, parse_codex_version};
use crate::error::{ManagerError, Result};
use crate::hash::sha256_file;
use crate::manager::{InstallEvent, OnlineInstallOptions};
use crate::platform::{ensure_executable, runtime_artifact_target};
use crate::process::ProcessRunner;
use crate::state::{ManagerPaths, ensure_managed_directory, remove_managed_tree};
use directories::ProjectDirs;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use ureq::{Agent, ResponseExt};

const OPENAI_REPOSITORY: &str = "openai/codex";
const PRIMARY_COMPAT_REPOSITORY: &str = "DSLZL/CSA-codex";
const LEGACY_COMPAT_REPOSITORY: &str = "DSLZL/CSA";
const GH_PROXY_ROUTES: [(&str, &str); 7] = [
    ("https://gh-proxy.org/", "gh-proxy.org"),
    ("https://v4.gh-proxy.org/", "v4.gh-proxy.org"),
    ("https://v6.gh-proxy.org/", "v6.gh-proxy.org"),
    ("https://cdn.gh-proxy.org/", "cdn.gh-proxy.org"),
    ("https://axisnow.gh-proxy.org/", "axisnow.gh-proxy.org"),
    ("https://gh-proxy.com/", "gh-proxy.com"),
    ("https://ghfast.top/", "ghfast.top"),
];
const CLOUDFLARE_TRACE_URL: &str = "https://www.cloudflare.com/cdn-cgi/trace";
const ALIBABA_REGION_URL: &str = "https://ip.taobao.com/outGetIpInfo?ip=myip&accessKey=alibaba-inc";
const RELEASE_DESCRIPTOR: &str = "compatibility-release.json";
const RELEASE_CHECKSUMS: &str = "SHA256SUMS";
const INSTALL_CATALOG_ASSET: &str = "install-catalog-v1.json";
const INSTALL_CATALOG_BOOTSTRAP: &str =
    include_str!("../release/install-catalog-bootstrap-v1.json");
const MAX_REGION_TRACE_BYTES: u64 = 8 * 1024;
const MAX_GIT_REFS_BYTES: u64 = 16 * 1024 * 1024;
const MAX_RELEASE_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_INSTALL_CATALOG_BYTES: u64 = 1024 * 1024;
const MAX_REMOTE_CACHE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_ARTIFACT_BYTES: u64 = 1024 * 1024 * 1024;
const GH_PROXY_SAMPLE_BYTES: u64 = 256 * 1024;
const GH_PROXY_SAMPLE_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_COMPATIBILITY_TAGS: usize = 1_000;
const MAX_INSTALL_CATALOG_PROBES: usize = 16;

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallCandidate {
    pub repository: String,
    pub compat_id: String,
    pub codex_version: String,
    pub build_target: String,
    pub patch_revision: u64,
    pub recorded_on: String,
    pub recommended: bool,
    pub release_tag: String,
    pub release_commit: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PreparedCompatibilityMetadata {
    pub compat_id: String,
    pub codex_version: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct RemoteCompatibilityReport {
    pub status: &'static str,
    pub repository: Option<String>,
    pub artifact_target: String,
    pub compat_ids: Vec<String>,
    pub recommended_compat_id: Option<String>,
    pub prepared_compat_id: Option<String>,
    pub update_available: bool,
    pub latest_candidate: Option<InstallCandidate>,
    pub official_version_relation: Option<&'static str>,
    pub source: Option<&'static str>,
    pub checked_at_unix_seconds: Option<u64>,
}

/// Compares target candidates with the official version and selects the install recommendation.
fn remote_compatibility_report(
    candidates: &[InstallCandidate],
    official_version: &str,
    manager_target: &str,
    prepared: Option<&PreparedCompatibilityMetadata>,
) -> RemoteCompatibilityReport {
    let artifact_target = compatibility_artifact_target(manager_target);
    let matching = matching_install_candidates(candidates, official_version, manager_target);
    if !matching.is_empty() {
        let selection = select_automatic(&matching);
        let (status, recommended) = match selection {
            Ok(index) => ("match", Some(&matching[index])),
            Err(error) if error.code == "ambiguous_compatibility_revision" => {
                ("ambiguous_compatibility_revision", None)
            }
            Err(_) => ("none_for_version", None),
        };
        let ids = matching
            .iter()
            .map(|candidate| candidate.compat_id.clone())
            .collect();
        let recommended_id = recommended.map(|candidate| candidate.compat_id.clone());
        return RemoteCompatibilityReport {
            status,
            repository: matching
                .first()
                .map(|candidate| candidate.repository.clone()),
            artifact_target: artifact_target.to_owned(),
            compat_ids: ids,
            recommended_compat_id: recommended_id.clone(),
            prepared_compat_id: prepared.map(|prepared| prepared.compat_id.clone()),
            update_available: status == "match"
                && recommended.is_some_and(|recommended| {
                    prepared_update_available(prepared, official_version, &matching, recommended)
                }),
            latest_candidate: None,
            official_version_relation: None,
            source: None,
            checked_at_unix_seconds: None,
        };
    }
    let latest = candidates
        .iter()
        .filter(|candidate| candidate.build_target == artifact_target)
        .max_by(|left, right| compare_catalog_candidates(left, right))
        .cloned();
    let relation = latest.as_ref().and_then(|candidate| {
        match (
            version_key(official_version).ok()?,
            version_key(&candidate.codex_version).ok()?,
        ) {
            (official, latest) if official < latest => Some("older"),
            (official, latest) if official > latest => Some("newer"),
            _ => Some("same"),
        }
    });
    RemoteCompatibilityReport {
        status: "none_for_version",
        repository: candidates
            .first()
            .map(|candidate| candidate.repository.clone()),
        artifact_target: artifact_target.to_owned(),
        compat_ids: Vec::new(),
        recommended_compat_id: None,
        prepared_compat_id: prepared.map(|prepared| prepared.compat_id.clone()),
        update_available: false,
        latest_candidate: latest,
        official_version_relation: relation,
        source: None,
        checked_at_unix_seconds: None,
    }
}

/// Builds an unreachable report while preserving any known prepared metadata.
fn unreachable_remote_report(
    manager_target: &str,
    prepared: Option<&PreparedCompatibilityMetadata>,
) -> RemoteCompatibilityReport {
    RemoteCompatibilityReport {
        status: "unreachable",
        repository: None,
        artifact_target: compatibility_artifact_target(manager_target).to_owned(),
        compat_ids: Vec::new(),
        recommended_compat_id: None,
        prepared_compat_id: prepared.map(|prepared| prepared.compat_id.clone()),
        update_available: false,
        latest_candidate: None,
        official_version_relation: None,
        source: None,
        checked_at_unix_seconds: None,
    }
}

/// Reports an update only when the prepared release is known to be older.
fn prepared_update_available(
    prepared: Option<&PreparedCompatibilityMetadata>,
    official_version: &str,
    matching: &[InstallCandidate],
    recommended: &InstallCandidate,
) -> bool {
    let Some(prepared) = prepared else {
        return false;
    };
    if prepared.compat_id == recommended.compat_id {
        return false;
    }
    let Some(prepared_version) = prepared.codex_version.as_deref() else {
        return false;
    };
    if prepared_version != official_version {
        return true;
    }
    matching
        .iter()
        .find(|candidate| candidate.compat_id == prepared.compat_id)
        .is_some_and(|candidate| candidate.patch_revision < recommended.patch_revision)
}

const REMOTE_DIAGNOSTIC_TIMEOUT: Duration = Duration::from_secs(5);
const REMOTE_REGION_PROBE_TIMEOUT: Duration = Duration::from_millis(1_500);
const REMOTE_CACHE_TTL: Duration = Duration::from_secs(60 * 60);

trait RemoteMetadataSource {
    /// Fetches repository refs from an injectable metadata source before the deadline.
    fn repository_refs(
        &mut self,
        repository: &'static str,
        deadline: Instant,
    ) -> Result<Option<BTreeMap<String, String>>>;
    /// Fetches catalog bytes from an injectable metadata source before the deadline.
    fn catalog(
        &mut self,
        repository: &'static str,
        tag: &str,
        deadline: Instant,
    ) -> Result<Option<Vec<u8>>>;
}

struct GitHubRemoteMetadata {
    route: Option<GitHubRoute>,
    client: Option<(&'static str, GitHubClient)>,
}

impl GitHubRemoteMetadata {
    /// Creates a remote metadata source with no client or route selected yet.
    fn new() -> Self {
        Self {
            route: None,
            client: None,
        }
    }

    /// Reuses a repository client and selects a responsive proxy when needed.
    fn client(&mut self, repository: &'static str, deadline: Instant) -> Result<&GitHubClient> {
        if self.client.as_ref().map(|(current, _)| *current) != Some(repository) {
            if let Some((_, client)) = self.client.take() {
                self.route = Some(client.route.get());
            }
            let route = match self.route {
                Some(route) => route,
                None => select_detected_remote_route(
                    detect_github_route_until(deadline)?.unwrap_or(GitHubRoute::Direct),
                    repository,
                    deadline,
                    select_proxy_index_until,
                )?,
            };
            self.route = Some(route);
            self.client = Some((
                repository,
                GitHubClient::with_deadline(repository, route, deadline),
            ));
        }
        Ok(&self.client.as_ref().expect("client was initialized").1)
    }
}

impl RemoteMetadataSource for GitHubRemoteMetadata {
    /// Fetches refs through the selected GitHub client.
    fn repository_refs(
        &mut self,
        repository: &'static str,
        deadline: Instant,
    ) -> Result<Option<BTreeMap<String, String>>> {
        self.client(repository, deadline)?
            .repository_refs_with_deadline(deadline)
    }

    /// Fetches catalog bytes through the selected GitHub client.
    fn catalog(
        &mut self,
        repository: &'static str,
        tag: &str,
        deadline: Instant,
    ) -> Result<Option<Vec<u8>>> {
        self.client(repository, deadline)?
            .read_catalog_bytes_with_deadline(tag, deadline)
    }
}

/// Keeps direct routing or selects a live proxy for a detected proxy route.
fn select_detected_remote_route(
    detected: GitHubRoute,
    repository: &'static str,
    deadline: Instant,
    select_proxy: fn(&'static str, Instant) -> Result<usize>,
) -> Result<GitHubRoute> {
    match detected {
        GitHubRoute::Proxy(_) => Ok(GitHubRoute::Proxy(select_proxy(repository, deadline)?)),
        GitHubRoute::Direct => Ok(GitHubRoute::Direct),
    }
}

/// Checks remote compatibility without downloading release executables.
pub(crate) fn diagnose_remote_compatibility(
    official_version: &str,
    manager_target: &str,
    prepared: Option<&PreparedCompatibilityMetadata>,
    refresh: bool,
) -> RemoteCompatibilityReport {
    let mut source = GitHubRemoteMetadata::new();
    let cache_dir = ProjectDirs::from("org", "DSLZL", "csa")
        .map(|directories| directories.cache_dir().join("remote-doctor"));
    diagnose_remote_with_source(
        official_version,
        manager_target,
        prepared,
        refresh,
        &mut source,
        cache_dir.as_deref(),
    )
}

/// Runs remote discovery with the default five-second deadline.
fn diagnose_remote_with_source(
    official_version: &str,
    manager_target: &str,
    prepared: Option<&PreparedCompatibilityMetadata>,
    refresh: bool,
    source: &mut dyn RemoteMetadataSource,
    cache_dir: Option<&Path>,
) -> RemoteCompatibilityReport {
    diagnose_remote_with_deadline(
        official_version,
        manager_target,
        prepared,
        refresh,
        source,
        cache_dir,
        Instant::now() + REMOTE_DIAGNOSTIC_TIMEOUT,
    )
}

/// Runs remote discovery with a caller-provided deadline.
fn diagnose_remote_with_deadline(
    official_version: &str,
    manager_target: &str,
    prepared: Option<&PreparedCompatibilityMetadata>,
    refresh: bool,
    source: &mut dyn RemoteMetadataSource,
    cache_dir: Option<&Path>,
    deadline: Instant,
) -> RemoteCompatibilityReport {
    diagnose_remote_with_deadline_at(
        official_version,
        manager_target,
        prepared,
        refresh,
        source,
        cache_dir,
        RemoteDiagnosticTiming {
            deadline,
            now: SystemTime::now(),
        },
    )
}

#[derive(Clone, Copy)]
struct RemoteDiagnosticTiming {
    deadline: Instant,
    now: SystemTime,
}

/// Uses cached candidates when valid, otherwise fetches and reports remote candidates.
fn diagnose_remote_with_deadline_at(
    official_version: &str,
    manager_target: &str,
    prepared: Option<&PreparedCompatibilityMetadata>,
    refresh: bool,
    source: &mut dyn RemoteMetadataSource,
    cache_dir: Option<&Path>,
    timing: RemoteDiagnosticTiming,
) -> RemoteCompatibilityReport {
    let cache_path = cache_dir.map(|directory| {
        directory.join(format!(
            "csa-doctor-{}.json",
            cache_key(manager_target, official_version)
        ))
    });
    if !refresh
        && let Some(candidates) = cache_path
            .as_deref()
            .and_then(|path| read_remote_cache_at(path, manager_target, timing.now))
    {
        let mut report =
            remote_compatibility_report(&candidates, official_version, manager_target, prepared);
        report.source = Some("cache");
        report.checked_at_unix_seconds = cache_path
            .as_deref()
            .and_then(|path| fs::metadata(path).ok())
            .and_then(|metadata| metadata.modified().ok())
            .and_then(system_time_unix_seconds);
        return report;
    }
    match fetch_remote_candidates(source, official_version, manager_target, timing.deadline) {
        Ok(Some((repository, candidates))) => {
            if let Some(cache_path) = cache_path.as_deref() {
                write_remote_cache(cache_path, &candidates);
            }
            let mut report = remote_compatibility_report(
                &candidates,
                official_version,
                manager_target,
                prepared,
            );
            report.repository = Some(repository.to_owned());
            report.source = Some("network");
            report.checked_at_unix_seconds = system_time_unix_seconds(SystemTime::now());
            report
        }
        _ => unreachable_remote_report(manager_target, prepared),
    }
}

/// Converts a system time to Unix seconds when representable.
fn system_time_unix_seconds(time: SystemTime) -> Option<u64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .map(|age| age.as_secs())
}

/// Finds a matching repository catalog or retains the best target catalog for no-match reports.
fn fetch_remote_candidates(
    source: &mut dyn RemoteMetadataSource,
    official_version: &str,
    manager_target: &str,
    deadline: Instant,
) -> Result<Option<(&'static str, Vec<InstallCandidate>)>> {
    let mut latest_catalog: Option<(&'static str, Vec<InstallCandidate>)> = None;
    for repository in [PRIMARY_COMPAT_REPOSITORY, LEGACY_COMPAT_REPOSITORY] {
        remaining_deadline(deadline)?;
        let refs = match source.repository_refs(repository, deadline) {
            Ok(Some(refs)) => refs,
            Ok(None) => continue,
            Err(error) => return Err(error),
        };
        remaining_deadline(deadline)?;
        let tags = compatibility_tags(&refs)?;
        if tags.is_empty() {
            continue;
        }
        let mut catalog = None;
        for tag in tags.into_iter().take(MAX_INSTALL_CATALOG_PROBES) {
            remaining_deadline(deadline)?;
            match source.catalog(repository, &tag, deadline)? {
                Some(bytes) => {
                    remaining_deadline(deadline)?;
                    if bytes.len() as u64 > MAX_INSTALL_CATALOG_BYTES {
                        return Err(invalid_install_catalog(
                            "install catalog exceeds the supported size",
                        ));
                    }
                    let value: InstallCatalog =
                        serde_json::from_slice(&bytes).map_err(|error| {
                            invalid_install_catalog(format!(
                                "install catalog is invalid JSON: {error}"
                            ))
                        })?;
                    validate_install_catalog(&value, repository, &refs, Some(&tag))?;
                    catalog = Some(value);
                    break;
                }
                None => continue,
            }
        }
        let catalog = match catalog {
            Some(catalog) => catalog,
            None if repository == LEGACY_COMPAT_REPOSITORY => {
                let catalog: InstallCatalog = serde_json::from_str(INSTALL_CATALOG_BOOTSTRAP)
                    .map_err(|error| {
                        invalid_install_catalog(format!(
                            "bundled install catalog is invalid JSON: {error}"
                        ))
                    })?;
                validate_install_catalog(&catalog, repository, &refs, None)?;
                catalog
            }
            None => {
                return Err(invalid_install_catalog(
                    "authoritative repository has no readable valid install catalog",
                ));
            }
        };
        let all = catalog_candidates_for_target(&catalog, manager_target, repository);
        let has_install_match =
            !matching_install_candidates(&all, official_version, manager_target).is_empty();
        if has_install_match {
            return Ok(Some((repository, all)));
        }
        let replace_latest = match latest_catalog.as_ref() {
            None => true,
            Some((_, current)) => {
                match (
                    highest_catalog_candidate(&all),
                    highest_catalog_candidate(current),
                ) {
                    (Some(next), Some(previous)) => {
                        compare_catalog_revisions(next, previous).is_gt()
                    }
                    (Some(_), None) => true,
                    _ => false,
                }
            }
        };
        if replace_latest {
            latest_catalog = Some((repository, all));
        }
    }
    Ok(latest_catalog)
}

/// Returns the highest official-version and patch-revision candidate.
fn highest_catalog_candidate(candidates: &[InstallCandidate]) -> Option<&InstallCandidate> {
    candidates
        .iter()
        .max_by(|left, right| compare_catalog_candidates(left, right))
}

/// Orders catalog candidates by version, revision, then stable compatibility ID.
fn compare_catalog_candidates(
    left: &InstallCandidate,
    right: &InstallCandidate,
) -> std::cmp::Ordering {
    compare_catalog_revisions(left, right).then_with(|| right.compat_id.cmp(&left.compat_id))
}

/// Compares catalog candidates by official version and numeric patch revision.
fn compare_catalog_revisions(
    left: &InstallCandidate,
    right: &InstallCandidate,
) -> std::cmp::Ordering {
    version_key(&left.codex_version)
        .ok()
        .cmp(&version_key(&right.codex_version).ok())
        .then_with(|| left.patch_revision.cmp(&right.patch_revision))
}

/// Returns time remaining or a diagnostic timeout when the deadline has passed.
fn remaining_deadline(deadline: Instant) -> Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|value| !value.is_zero())
        .ok_or_else(|| {
            ManagerError::new(
                "remote_diagnostic_timeout",
                "remote diagnostic deadline exceeded",
            )
        })
}

/// Writes normalized candidates to a private cache using an atomic replacement.
fn write_remote_cache(path: &Path, candidates: &[InstallCandidate]) {
    if candidates.is_empty() {
        return;
    }
    let Ok(bytes) = serde_json::to_vec(candidates) else {
        return;
    };
    if bytes.len() as u64 > MAX_REMOTE_CACHE_BYTES
        || path
            .parent()
            .is_none_or(|directory| ensure_private_cache_directory(directory).is_err())
    {
        return;
    }
    let temporary = path.with_extension(format!("{}.{}.tmp", std::process::id(), cache_nonce()));
    let Ok(mut file) = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
    else {
        return;
    };
    let result = file.write_all(&bytes).and_then(|()| file.sync_all());
    drop(file);
    if result.is_ok() {
        let replacement: io::Result<()> = {
            #[cfg(windows)]
            {
                fs::remove_file(path).or_else(|error| {
                    if error.kind() == io::ErrorKind::NotFound {
                        Ok(())
                    } else {
                        Err(error)
                    }
                })
            }
            #[cfg(not(windows))]
            {
                Ok(())
            }
        };
        if replacement.is_ok() && fs::rename(&temporary, path).is_ok() {
            return;
        }
    }
    {
        let _ = fs::remove_file(&temporary);
    }
}

/// Creates a process-unique suffix for temporary cache files.
fn cache_nonce() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        ^ u128::from(std::process::id())
}

/// Validates cached candidates against repository, target, identity, and date rules.
fn valid_cached_candidates(candidates: &[InstallCandidate], manager_target: &str) -> bool {
    let target = compatibility_artifact_target(manager_target);
    let Some(repository) = candidates
        .first()
        .map(|candidate| candidate.repository.as_str())
    else {
        return false;
    };
    if repository != PRIMARY_COMPAT_REPOSITORY && repository != LEGACY_COMPAT_REPOSITORY {
        return false;
    }
    let mut compat_ids = BTreeSet::new();
    let mut release_tags = BTreeSet::new();
    candidates.iter().all(|candidate| {
        let key = version_key(&candidate.codex_version);
        (candidate.repository == repository
            && candidate.build_target == target
            && key.is_ok_and(|key| {
                format!("{}.{}.{}", key.0, key.1, key.2) == candidate.codex_version
            })
            && candidate
                .compat_id
                .starts_with(&format!("rust-v{}-", candidate.codex_version))
            && patch_revision(&candidate.compat_id).ok() == Some(candidate.patch_revision)
            && validate_asset_name(&candidate.compat_id).is_ok()
            && candidate.release_tag == format!("compat-{}", candidate.compat_id)
            && validate_asset_name(&candidate.release_tag).is_ok()
            && validate_sha(&candidate.release_commit).is_ok()
            && valid_recorded_on(&candidate.recorded_on)
            && !candidate.recommended)
            && compat_ids.insert(&candidate.compat_id)
            && release_tags.insert(&candidate.release_tag)
    })
}

/// Reads a fresh, valid remote cache for the requested target.
#[cfg(test)]
fn read_remote_cache(path: &Path, manager_target: &str) -> Option<Vec<InstallCandidate>> {
    read_remote_cache_at(path, manager_target, SystemTime::now())
}

/// Reads and validates a remote cache at a supplied reference time.
fn read_remote_cache_at(
    path: &Path,
    manager_target: &str,
    now: SystemTime,
) -> Option<Vec<InstallCandidate>> {
    let directory = path.parent()?;
    if !cache_directory_is_private(directory) {
        return None;
    }
    let metadata = fs::symlink_metadata(path).ok()?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_REMOTE_CACHE_BYTES {
        return None;
    }
    let age = now.duration_since(metadata.modified().ok()?).ok()?;
    if age >= REMOTE_CACHE_TTL {
        return None;
    }
    let file = File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_REMOTE_CACHE_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_REMOTE_CACHE_BYTES {
        return None;
    }
    let candidates: Vec<InstallCandidate> = serde_json::from_slice(&bytes).ok()?;
    valid_cached_candidates(&candidates, manager_target).then_some(candidates)
}

/// Creates the cache directory with user-only permissions where supported.
fn ensure_private_cache_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;

        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder.create(path)?;
    }
    #[cfg(not(unix))]
    fs::create_dir_all(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "remote cache directory is not a private directory",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "remote cache directory is not private to the current user",
            ));
        }
    }
    Ok(())
}

/// Checks whether a cache directory has restrictive permissions.
fn cache_directory_is_private(path: &Path) -> bool {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return false;
    };
    if !metadata.file_type().is_dir() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o077 == 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Builds a cache identity from both artifact target and official version.
fn cache_key(target: &str, official_version: &str) -> String {
    target
        .bytes()
        .chain(std::iter::once(0))
        .chain(official_version.bytes())
        .fold(0xcbf29ce484222325u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        })
        .to_string()
}

pub type InstallSelector<'a> = dyn FnMut(&[InstallCandidate]) -> Result<String> + 'a;

struct SelectedCompatibility {
    repository: String,
    compat_id: String,
    release_tag: String,
    release_commit: String,
    catalog_entry: Option<InstallCandidate>,
}

pub struct OnlineBundle {
    pub(crate) manager_root: Option<PathBuf>,
    pub(crate) official: PathBuf,
    pub(crate) official_native: Option<PathBuf>,
    pub(crate) runtime: RuntimeManifest,
    pub(crate) artifact: PathBuf,
    _staging: StagingGuard,
}

struct StagingGuard {
    manager_root: PathBuf,
    path: PathBuf,
}

impl Drop for StagingGuard {
    fn drop(&mut self) {
        let _ = remove_managed_tree(&self.manager_root, &self.path);
    }
}

pub fn resolve_online_install(
    options: &OnlineInstallOptions,
    runner: &dyn ProcessRunner,
) -> Result<OnlineBundle> {
    resolve_online_install_inner(options, runner, &mut |_| {}, None)
}

pub fn resolve_online_install_with_progress(
    options: &OnlineInstallOptions,
    runner: &dyn ProcessRunner,
    progress: &mut dyn FnMut(InstallEvent),
) -> Result<OnlineBundle> {
    resolve_online_install_inner(options, runner, progress, None)
}

pub fn resolve_online_install_with_selector(
    options: &OnlineInstallOptions,
    runner: &dyn ProcessRunner,
    progress: &mut dyn FnMut(InstallEvent),
    selector: &mut InstallSelector<'_>,
) -> Result<OnlineBundle> {
    resolve_online_install_inner(options, runner, progress, Some(selector))
}

fn resolve_online_install_inner(
    options: &OnlineInstallOptions,
    runner: &dyn ProcessRunner,
    progress: &mut dyn FnMut(InstallEvent),
    selector: Option<&mut InstallSelector<'_>>,
) -> Result<OnlineBundle> {
    let paths = ManagerPaths::resolve(options.manager_root.clone())?;
    progress(InstallEvent::DetectingOfficial);
    let official = detect_official(
        runner,
        options.official.as_deref(),
        options.official_native.as_deref(),
        std::slice::from_ref(&paths.root),
    )?;
    progress(InstallEvent::DiscoveringCompatibility);
    let github_route = detect_github_route().unwrap_or(GitHubRoute::Direct);
    paths.initialize()?;
    let staging_path = paths
        .downloads
        .join(format!(".install-{}", std::process::id()));
    remove_managed_tree(&paths.root, &staging_path)?;
    ensure_managed_directory(&paths.root, &staging_path)?;
    let staging = StagingGuard {
        manager_root: paths.root.clone(),
        path: staging_path.clone(),
    };

    let (client, selected) = if let Some(requested) = options.compat.as_deref() {
        validate_asset_name(requested)?;
        let release_tag = format!("compat-{requested}");
        resolve_ordered_authority(|repository| {
            let client = GitHubClient::new(repository, github_route);
            let Some(refs) = client.repository_refs()? else {
                return Ok(None);
            };
            let Some(release_commit) = tag_commit_if_present(&refs, &release_tag)? else {
                return Ok(None);
            };
            Ok(Some((
                client,
                SelectedCompatibility {
                    repository: repository.to_owned(),
                    compat_id: requested.to_owned(),
                    release_tag: release_tag.clone(),
                    release_commit,
                    catalog_entry: None,
                },
            )))
        })?
        .ok_or_else(|| {
            ManagerError::new(
                "compatibility_not_found",
                format!("no formal compatibility release has ID {requested}"),
            )
        })?
    } else {
        let (client, mut candidates) = resolve_ordered_authority(|repository| {
            let client = GitHubClient::new(repository, github_route);
            let Some(refs) = client.repository_refs()? else {
                return Ok(None);
            };
            let candidates = discover_install_candidates(
                &client,
                &paths.root,
                &staging_path,
                &refs,
                &official.version,
            )?;
            if candidates.is_empty() {
                Ok(None)
            } else {
                Ok(Some((client, candidates)))
            }
        })?
        .ok_or_else(|| {
            ManagerError::new(
                "no_installable_compatibility_releases",
                "no formal compatibility release matches this Manager target and official Codex version",
            )
        })?;
        let recommended = select_automatic(&candidates)?;
        candidates[recommended].recommended = true;
        let selected_id = if let Some(select) = selector {
            progress(InstallEvent::SelectingCompatibility);
            select(&candidates)?
        } else {
            candidates[recommended].compat_id.clone()
        };
        let candidate = take_selected_candidate(candidates, &selected_id)?;
        (
            client,
            SelectedCompatibility {
                repository: candidate.repository.clone(),
                compat_id: candidate.compat_id.clone(),
                release_tag: candidate.release_tag.clone(),
                release_commit: candidate.release_commit.clone(),
                catalog_entry: Some(candidate),
            },
        )
    };
    let SelectedCompatibility {
        repository,
        compat_id,
        release_tag,
        release_commit,
        catalog_entry,
    } = selected;
    progress(InstallEvent::SelectedCompatibility {
        compat_id: compat_id.clone(),
    });
    let artifact_target = compatibility_artifact_target(BUILD_TARGET);
    progress(InstallEvent::DownloadingReleaseMetadata);
    let selected_path = staging_path.join("selected");
    ensure_managed_directory(&paths.root, &selected_path)?;
    let (descriptor, checksums) = download_release_metadata(&client, &release_tag, &selected_path)?
        .ok_or_else(|| {
            ManagerError::new(
                "compatibility_release_missing",
                format!("selected compatibility release disappeared: {release_tag}"),
            )
        })?;
    validate_catalog_descriptor(
        &descriptor,
        &repository,
        &release_tag,
        &release_commit,
        &compat_id,
    )?;
    if catalog_entry.is_some_and(|expected| {
        descriptor.upstream.version != expected.codex_version
            || !descriptor_supports_target(&descriptor, &expected.build_target)
    }) {
        return Err(ManagerError::new(
            "compatibility_release_changed",
            "selected compatibility metadata changed during installation",
        ));
    }

    let upstream_version = stable_release_version(&descriptor.upstream.tag)?;
    descriptor_artifact(&descriptor, artifact_target)?;
    if official.version != upstream_version {
        return Err(ManagerError::new(
            "unsupported_official_version",
            format!(
                "selected compatibility requires Codex {upstream_version}, official Codex is {}",
                official.version
            ),
        ));
    }

    let release_artifact = descriptor_artifact(&descriptor, artifact_target)?;
    validate_declared_asset(release_artifact, &checksums)?;
    let runtime = RuntimeManifest::new(
        compat_id,
        upstream_version,
        artifact_target.to_owned(),
        RuntimeArtifact {
            filename: release_artifact.path.clone(),
            sha256: release_artifact.sha256.clone(),
            size: release_artifact.size,
        },
    )?;
    let artifact_path = selected_path.join("artifact").join(&release_artifact.path);
    ensure_managed_directory(
        &paths.root,
        artifact_path.parent().expect("artifact path has a parent"),
    )?;
    progress(InstallEvent::ConnectingArtifact);
    client.rank_proxy_routes_for_asset(
        &release_tag,
        &release_artifact.asset,
        release_artifact.size,
    );
    if !client.download_asset_with_progress(
        &release_tag,
        &release_artifact.asset,
        &artifact_path,
        (
            Some(release_artifact.size),
            Some(&release_artifact.sha256),
            MAX_ARTIFACT_BYTES,
        ),
        Some(progress),
    )? {
        return Err(ManagerError::new(
            "invalid_compatibility_release",
            format!(
                "declared release asset is missing: {}",
                release_artifact.asset
            ),
        ));
    }
    ensure_executable(&artifact_path)?;

    Ok(OnlineBundle {
        manager_root: options.manager_root.clone(),
        official: official.executable.path,
        official_native: official.native.map(|native| native.path),
        runtime,
        artifact: artifact_path,
        _staging: staging,
    })
}

fn resolve_ordered_authority<T>(
    mut lookup: impl FnMut(&'static str) -> Result<Option<T>>,
) -> Result<Option<T>> {
    for repository in [PRIMARY_COMPAT_REPOSITORY, LEGACY_COMPAT_REPOSITORY] {
        if let Some(value) = lookup(repository)? {
            return Ok(Some(value));
        }
    }
    Ok(None)
}

fn tag_commit_if_present(
    refs: &BTreeMap<String, String>,
    release_tag: &str,
) -> Result<Option<String>> {
    let reference = format!("refs/tags/{release_tag}");
    if !refs.contains_key(&reference) && !refs.contains_key(&format!("{reference}^{{}}")) {
        return Ok(None);
    }
    peel_tag_from_refs(refs, release_tag).map(Some)
}

fn discover_install_candidates(
    client: &GitHubClient,
    manager_root: &Path,
    staging_path: &Path,
    refs: &BTreeMap<String, String>,
    official_version: &str,
) -> Result<Vec<InstallCandidate>> {
    let catalog_path = staging_path.join(
        if client
            .repository
            .eq_ignore_ascii_case(PRIMARY_COMPAT_REPOSITORY)
        {
            "primary-catalog"
        } else {
            "legacy-catalog"
        },
    );
    ensure_managed_directory(manager_root, &catalog_path)?;
    let Some(catalog) = load_install_catalog(client, &catalog_path, refs)? else {
        return Ok(Vec::new());
    };
    Ok(install_candidates(
        catalog,
        official_version,
        BUILD_TARGET,
        client.repository,
    ))
}

/// Filters catalog entries by exact official version and target using install rules.
fn install_candidates(
    catalog: InstallCatalog,
    official_version: &str,
    manager_target: &str,
    repository: &str,
) -> Vec<InstallCandidate> {
    let candidates = catalog_candidates_for_target(&catalog, manager_target, repository);
    matching_install_candidates(&candidates, official_version, manager_target)
}

/// Converts catalog entries for the requested artifact target into candidates.
fn catalog_candidates_for_target(
    catalog: &InstallCatalog,
    manager_target: &str,
    repository: &str,
) -> Vec<InstallCandidate> {
    let artifact_target = compatibility_artifact_target(manager_target);
    catalog
        .entries
        .iter()
        .filter(|entry| entry.supports_target(artifact_target))
        .map(|entry| InstallCandidate {
            repository: repository.to_owned(),
            compat_id: entry.compat_id.clone(),
            codex_version: entry.codex_version.clone(),
            build_target: artifact_target.to_owned(),
            patch_revision: entry.patch_revision,
            recorded_on: entry.recorded_on.clone(),
            recommended: false,
            release_tag: entry.release_tag.clone(),
            release_commit: entry.release_commit.clone(),
        })
        .collect()
}

/// Returns only candidates matching both official version and artifact target.
fn matching_install_candidates(
    candidates: &[InstallCandidate],
    official_version: &str,
    manager_target: &str,
) -> Vec<InstallCandidate> {
    let artifact_target = compatibility_artifact_target(manager_target);
    candidates
        .iter()
        .filter(|candidate| {
            candidate.codex_version == official_version && candidate.build_target == artifact_target
        })
        .cloned()
        .collect()
}

fn compatibility_artifact_target(manager_target: &str) -> &str {
    runtime_artifact_target(manager_target)
}

fn take_selected_candidate(
    candidates: Vec<InstallCandidate>,
    selected_id: &str,
) -> Result<InstallCandidate> {
    candidates
        .into_iter()
        .find(|candidate| candidate.compat_id == selected_id)
        .ok_or_else(|| {
            ManagerError::new(
                "invalid_install_selection",
                "compatibility selector returned an unknown compatibility ID",
            )
        })
}

fn load_install_catalog(
    client: &GitHubClient,
    catalog_path: &Path,
    refs: &BTreeMap<String, String>,
) -> Result<Option<InstallCatalog>> {
    let release_tags = compatibility_tags(refs)?;
    if release_tags.is_empty() {
        return Ok(None);
    }
    // ponytail: probe only the newest 16 tags; switch to one stable catalog URL if release history outgrows it.
    for (index, release_tag) in release_tags
        .into_iter()
        .take(MAX_INSTALL_CATALOG_PROBES)
        .enumerate()
    {
        let destination = catalog_path.join(format!("remote-{index}.json"));
        if client.download_asset(
            &release_tag,
            INSTALL_CATALOG_ASSET,
            &destination,
            None,
            None,
            MAX_INSTALL_CATALOG_BYTES,
        )? {
            let catalog = read_install_catalog(&destination)?;
            validate_install_catalog(&catalog, client.repository, refs, Some(&release_tag))?;
            return Ok(Some(catalog));
        }
    }
    if !client
        .repository
        .eq_ignore_ascii_case(LEGACY_COMPAT_REPOSITORY)
    {
        return Err(invalid_install_catalog(
            "primary compatibility tags do not publish an install catalog",
        ));
    }
    let catalog: InstallCatalog =
        serde_json::from_str(INSTALL_CATALOG_BOOTSTRAP).map_err(|error| {
            ManagerError::new(
                "invalid_install_catalog",
                format!("invalid bundled install catalog: {error}"),
            )
        })?;
    validate_install_catalog(&catalog, LEGACY_COMPAT_REPOSITORY, refs, None)?;
    Ok(Some(catalog))
}

fn download_release_metadata(
    client: &GitHubClient,
    release_tag: &str,
    destination: &Path,
) -> Result<Option<(CompatibilityRelease, BTreeMap<String, String>)>> {
    let checksums_path = destination.join(RELEASE_CHECKSUMS);
    if !client.download_asset(
        release_tag,
        RELEASE_CHECKSUMS,
        &checksums_path,
        None,
        None,
        MAX_RELEASE_FILE_BYTES,
    )? {
        return Ok(None);
    }
    let checksums =
        parse_checksums(&fs::read(&checksums_path).map_err(|error| {
            ManagerError::io("read downloaded compatibility checksums", error)
        })?)?;
    let descriptor_sha256 = checksums.get(RELEASE_DESCRIPTOR).ok_or_else(|| {
        ManagerError::new(
            "invalid_compatibility_release",
            "compatibility release is missing its provenance descriptor",
        )
    })?;
    let descriptor_path = destination.join(RELEASE_DESCRIPTOR);
    if !client.download_asset(
        release_tag,
        RELEASE_DESCRIPTOR,
        &descriptor_path,
        None,
        Some(descriptor_sha256),
        MAX_RELEASE_FILE_BYTES,
    )? {
        return Err(ManagerError::new(
            "invalid_compatibility_release",
            "compatibility release is missing its provenance descriptor",
        ));
    }
    let descriptor: CompatibilityRelease = read_json_file(&descriptor_path)?;
    let declared_assets = descriptor_assets(&descriptor)?;
    let expected_checksums: BTreeSet<_> = declared_assets
        .keys()
        .cloned()
        .chain([RELEASE_DESCRIPTOR.to_owned()])
        .collect();
    if checksums.keys().cloned().collect::<BTreeSet<_>>() != expected_checksums {
        return Err(ManagerError::new(
            "invalid_compatibility_release",
            "SHA256SUMS differs from the reviewed compatibility descriptor",
        ));
    }
    for file in declared_assets.values() {
        validate_declared_asset(file, &checksums)?;
    }
    Ok(Some((descriptor, checksums)))
}

fn select_automatic(catalog: &[InstallCandidate]) -> Result<usize> {
    let greatest = catalog
        .iter()
        .map(|entry| entry.patch_revision)
        .max()
        .ok_or_else(|| {
            ManagerError::new(
                "no_installable_compatibility_releases",
                "no formal compatibility release matches this Manager target and official Codex version",
            )
        })?;
    let mut matches = catalog
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.patch_revision == greatest);
    let selected = matches
        .next()
        .expect("greatest revision came from catalog")
        .0;
    if matches.next().is_some() {
        return Err(ManagerError::new(
            "ambiguous_compatibility_revision",
            format!("multiple installable compatibility releases use patch revision p{greatest}"),
        ));
    }
    Ok(selected)
}

fn patch_revision(compat_id: &str) -> Result<u64> {
    let revision = compat_id
        .rsplit_once("-p")
        .map(|(_, revision)| revision)
        .filter(|revision| {
            !revision.is_empty() && revision.bytes().all(|byte| byte.is_ascii_digit())
        })
        .and_then(|revision| revision.parse().ok())
        .ok_or_else(|| {
            ManagerError::new(
                "invalid_compatibility_release",
                "compatibility ID must end with numeric -pN",
            )
        })?;
    Ok(revision)
}

fn version_key(version: &str) -> Result<(u64, u64, u64)> {
    let mut parts = version.split('.');
    let parse = |value: Option<&str>| {
        value
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or_else(|| {
                ManagerError::new(
                    "invalid_compatibility_release",
                    "Codex version must use numeric X.Y.Z",
                )
            })
    };
    let key = (
        parse(parts.next())?,
        parse(parts.next())?,
        parse(parts.next())?,
    );
    if parts.next().is_some() {
        return Err(ManagerError::new(
            "invalid_compatibility_release",
            "Codex version must use numeric X.Y.Z",
        ));
    }
    Ok(key)
}

fn validate_install_catalog(
    catalog: &InstallCatalog,
    expected_repository: &str,
    refs: &BTreeMap<String, String>,
    containing_release_tag: Option<&str>,
) -> Result<()> {
    if !matches!(catalog.schema, 1 | 2)
        || !catalog.repository.eq_ignore_ascii_case(expected_repository)
        || catalog.entries.is_empty()
        || catalog.entries.len() > MAX_COMPATIBILITY_TAGS
    {
        return Err(invalid_install_catalog(
            "install catalog schema, repository, or entry count is invalid",
        ));
    }
    validate_asset_name(&catalog.source_release_tag)
        .map_err(|_| invalid_install_catalog("install catalog source tag is invalid"))?;
    validate_sha(&catalog.source_commit)
        .map_err(|_| invalid_install_catalog("install catalog source commit is invalid"))?;
    if containing_release_tag.is_some_and(|tag| tag != catalog.source_release_tag) {
        return Err(invalid_install_catalog(
            "install catalog source tag differs from the containing release",
        ));
    }
    if catalog_ref_commit(refs, &catalog.source_release_tag)? != catalog.source_commit {
        return Err(invalid_install_catalog(
            "install catalog source commit differs from its Git tag",
        ));
    }

    let mut compat_ids = BTreeSet::new();
    let mut release_tags = BTreeSet::new();
    let mut order = Vec::with_capacity(catalog.entries.len());
    let mut source_matches = 0;
    for entry in &catalog.entries {
        validate_asset_name(&entry.compat_id)
            .map_err(|_| invalid_install_catalog("install catalog compatibility ID is invalid"))?;
        validate_asset_name(&entry.release_tag)
            .map_err(|_| invalid_install_catalog("install catalog release tag is invalid"))?;
        let targets: Vec<&str> = match catalog.schema {
            1 if entry.build_target.is_some() && entry.build_targets.is_empty() => {
                vec![
                    entry
                        .build_target
                        .as_deref()
                        .expect("schema 1 target checked"),
                ]
            }
            2 if entry.build_target.is_none() && !entry.build_targets.is_empty() => {
                entry.build_targets.iter().map(String::as_str).collect()
            }
            _ => {
                return Err(invalid_install_catalog(
                    "install catalog target fields do not match its schema",
                ));
            }
        };
        if targets.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(invalid_install_catalog(
                "install catalog build targets must be unique and sorted",
            ));
        }
        for target in targets {
            validate_asset_name(target)
                .map_err(|_| invalid_install_catalog("install catalog build target is invalid"))?;
        }
        validate_sha(&entry.release_commit)
            .map_err(|_| invalid_install_catalog("install catalog release commit is invalid"))?;
        let key = version_key(&entry.codex_version)
            .map_err(|_| invalid_install_catalog("install catalog Codex version is invalid"))?;
        if format!("{}.{}.{}", key.0, key.1, key.2) != entry.codex_version
            || patch_revision(&entry.compat_id)
                .map_err(|_| invalid_install_catalog("install catalog patch revision is invalid"))?
                != entry.patch_revision
            || entry.release_tag != format!("compat-{}", entry.compat_id)
            || !valid_recorded_on(&entry.recorded_on)
        {
            return Err(invalid_install_catalog(
                "install catalog entry identity, revision, or date is invalid",
            ));
        }
        if !compat_ids.insert(&entry.compat_id) || !release_tags.insert(&entry.release_tag) {
            return Err(invalid_install_catalog(
                "install catalog repeats a compatibility ID or release tag",
            ));
        }
        if catalog_ref_commit(refs, &entry.release_tag)? != entry.release_commit {
            return Err(invalid_install_catalog(
                "install catalog release commit differs from its Git tag",
            ));
        }
        if entry.release_tag == catalog.source_release_tag {
            source_matches += 1;
            if entry.release_commit != catalog.source_commit {
                return Err(invalid_install_catalog(
                    "install catalog source entry differs from the source commit",
                ));
            }
        }
        order.push((key, entry.patch_revision, entry.compat_id.as_str()));
    }
    let mut sorted = order.clone();
    sorted.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| right.1.cmp(&left.1))
            .then_with(|| left.2.cmp(right.2))
    });
    if source_matches != 1 || order != sorted {
        return Err(invalid_install_catalog(
            "install catalog source entry or newest-first ordering is invalid",
        ));
    }
    Ok(())
}

fn catalog_ref_commit(refs: &BTreeMap<String, String>, tag: &str) -> Result<String> {
    peel_tag_from_refs(refs, tag)
        .map_err(|_| invalid_install_catalog("install catalog references a missing Git tag"))
}

fn invalid_install_catalog(message: impl Into<String>) -> ManagerError {
    ManagerError::new("invalid_install_catalog", message)
}

fn valid_recorded_on(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes
            .iter()
            .enumerate()
            .any(|(index, byte)| index != 4 && index != 7 && !byte.is_ascii_digit())
    {
        return false;
    }
    let parse = |start: usize, end: usize| value[start..end].parse::<u32>().ok();
    let (Some(year), Some(month), Some(day)) = (parse(0, 4), parse(5, 7), parse(8, 10)) else {
        return false;
    };
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    year != 0 && day != 0 && day <= days
}

struct ProgressReader<'a, R> {
    inner: R,
    downloaded_bytes: u64,
    total_bytes: u64,
    progress: &'a mut dyn FnMut(InstallEvent),
}

impl<R: Read> Read for ProgressReader<'_, R> {
    /// Copies bytes while reporting download progress when a callback is configured.
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buffer)?;
        if read != 0 {
            self.downloaded_bytes += read as u64;
            (self.progress)(InstallEvent::ArtifactProgress {
                downloaded_bytes: self.downloaded_bytes,
                total_bytes: self.total_bytes,
            });
        }
        Ok(read)
    }
}

struct GitHubClient {
    repository: &'static str,
    agent: Agent,
    route: Cell<GitHubRoute>,
    proxy_order: RefCell<Vec<usize>>,
    deadline: Option<Instant>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GitHubRoute {
    Direct,
    Proxy(usize),
}

enum GitHubRequestError {
    DeadlineExceeded,
    Request(ureq::Error),
}

impl GitHubClient {
    fn new(repository: &'static str, detected_route: GitHubRoute) -> Self {
        let route = match detected_route {
            GitHubRoute::Direct => GitHubRoute::Direct,
            GitHubRoute::Proxy(_) => GitHubRoute::Proxy(select_proxy_index(repository)),
        };
        Self::with_route(repository, route)
    }

    /// Creates a GitHub client using install's normal routing and timeout behavior.
    fn with_route(repository: &'static str, route: GitHubRoute) -> Self {
        let config = Agent::config_builder()
            .https_only(true)
            .max_redirects(5)
            .timeout_global(Some(Duration::from_secs(15 * 60)))
            .timeout_connect(Some(Duration::from_secs(15)))
            .timeout_recv_response(Some(Duration::from_secs(30)))
            .build();
        let mut proxy_order: Vec<_> = (0..GH_PROXY_ROUTES.len()).collect();
        if let GitHubRoute::Proxy(index) = route {
            proxy_order.rotate_left(index);
        }
        Self {
            repository,
            agent: Agent::new_with_config(config),
            route: Cell::new(route),
            proxy_order: RefCell::new(proxy_order),
            deadline: None,
        }
    }

    /// Creates a GitHub client whose requests share the supplied diagnostic deadline.
    fn with_deadline(repository: &'static str, route: GitHubRoute, deadline: Instant) -> Self {
        let mut client = Self::with_route(repository, route);
        client.deadline = Some(deadline);
        client
    }

    /// Reads public Git refs with the diagnostic request deadline.
    fn repository_refs_with_deadline(
        &self,
        deadline: Instant,
    ) -> Result<Option<BTreeMap<String, String>>> {
        let refs = self.repository_refs()?;
        remaining_deadline(deadline)?;
        Ok(refs)
    }

    /// Reads a tagged install catalog with the diagnostic request deadline.
    fn read_catalog_bytes_with_deadline(
        &self,
        release_tag: &str,
        deadline: Instant,
    ) -> Result<Option<Vec<u8>>> {
        remaining_deadline(deadline)?;
        validate_asset_name(release_tag)?;
        let url = release_asset_url(self.repository, release_tag, INSTALL_CATALOG_ASSET);
        let Some(mut response) = self.get_response(
            &url,
            "application/octet-stream",
            false,
            &[
                "github.com",
                "objects.githubusercontent.com",
                "release-assets.githubusercontent.com",
            ],
            "read compatibility install catalog",
            false,
        )?
        else {
            return Ok(None);
        };
        let bytes = response
            .body_mut()
            .with_config()
            .limit(MAX_INSTALL_CATALOG_BYTES + 1)
            .read_to_vec()
            .map_err(|error| network_error("read compatibility install catalog", error))?;
        if bytes.len() as u64 > MAX_INSTALL_CATALOG_BYTES {
            return Err(invalid_install_catalog(
                "install catalog exceeds the supported size",
            ));
        }
        remaining_deadline(deadline)?;
        Ok(Some(bytes))
    }

    fn repository_refs(&self) -> Result<Option<BTreeMap<String, String>>> {
        let url = git_refs_url(self.repository);
        let Some(mut response) = self.get_response(
            &url,
            "application/x-git-upload-pack-advertisement",
            true,
            &["github.com"],
            "query GitHub repository refs",
            true,
        )?
        else {
            return Ok(None);
        };
        let bytes = response
            .body_mut()
            .with_config()
            .limit(MAX_GIT_REFS_BYTES + 1)
            .read_to_vec()
            .map_err(|error| network_error("read GitHub repository refs", error))?;
        if bytes.len() as u64 > MAX_GIT_REFS_BYTES {
            return Err(ManagerError::new(
                "github_response_too_large",
                "GitHub repository refs exceed the supported size",
            ));
        }
        parse_git_refs(&bytes).map(Some)
    }

    fn rank_proxy_routes_for_asset(&self, release_tag: &str, asset_name: &str, expected_size: u64) {
        if !matches!(self.route.get(), GitHubRoute::Proxy(_)) {
            return;
        }
        let active = self.proxy_order.borrow().clone();
        if active.len() <= 1 {
            return;
        }
        let direct_url = release_asset_url(self.repository, release_tag, asset_name);
        let samples = sample_proxy_routes(&active, &direct_url, expected_size);
        let ranked = rank_proxy_indices(&active, samples);
        if let Some(first) = ranked.first().copied() {
            self.route.set(GitHubRoute::Proxy(first));
            *self.proxy_order.borrow_mut() = ranked;
        }
    }

    fn download_asset(
        &self,
        release_tag: &str,
        asset_name: &str,
        destination: &Path,
        expected_size: Option<u64>,
        expected_sha256: Option<&str>,
        max_size: u64,
    ) -> Result<bool> {
        self.download_asset_with_progress(
            release_tag,
            asset_name,
            destination,
            (expected_size, expected_sha256, max_size),
            None,
        )
    }

    fn download_asset_with_progress(
        &self,
        release_tag: &str,
        asset_name: &str,
        destination: &Path,
        expected: (Option<u64>, Option<&str>, u64),
        mut progress: Option<&mut dyn FnMut(InstallEvent)>,
    ) -> Result<bool> {
        let (expected_size, expected_sha256, max_size) = expected;
        validate_asset_name(release_tag)?;
        validate_asset_name(asset_name)?;
        if expected_size.is_some_and(|size| size == 0 || size > max_size) {
            return Err(ManagerError::new(
                "invalid_release_asset_size",
                format!("release asset has an invalid size: {asset_name}"),
            ));
        }
        if let Some(expected) = expected_sha256 {
            validate_sha256(expected)?;
        }
        let url = release_asset_url(self.repository, release_tag, asset_name);
        loop {
            let Some(mut response) = self.get_response(
                &url,
                "application/octet-stream",
                false,
                &[
                    "github.com",
                    "objects.githubusercontent.com",
                    "release-assets.githubusercontent.com",
                ],
                "download compatibility release asset",
                asset_name != INSTALL_CATALOG_ASSET,
            )?
            else {
                return Ok(false);
            };
            let route = self.route.get();
            if response.body().content_length().is_some_and(|length| {
                length == 0 || length > max_size || expected_size.is_some_and(|size| size != length)
            }) {
                let error = ManagerError::new(
                    "release_asset_size_mismatch",
                    format!("Content-Length differs for release asset: {asset_name}"),
                );
                if self.switch_after_failed_download(route, &mut progress) {
                    continue;
                }
                return Err(error);
            }
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(destination)
                .map_err(|error| {
                    ManagerError::io(
                        &format!("create download destination {}", destination.display()),
                        error,
                    )
                })?;
            let copy_result = {
                let mut reader = response
                    .body_mut()
                    .with_config()
                    .limit(expected_size.unwrap_or(max_size) + 1)
                    .reader();
                if let Some(progress) = progress.as_deref_mut() {
                    io::copy(
                        &mut ProgressReader {
                            inner: reader,
                            downloaded_bytes: 0,
                            total_bytes: expected_size.unwrap_or(max_size),
                            progress,
                        },
                        &mut output,
                    )
                } else {
                    io::copy(&mut reader, &mut output)
                }
            };
            let copied = match copy_result.and_then(|size| output.flush().map(|()| size)) {
                Ok(size) => size,
                Err(error) => {
                    drop(output);
                    let _ = fs::remove_file(destination);
                    let error = ManagerError::io("stream release asset", error);
                    if self.switch_after_failed_download(route, &mut progress) {
                        continue;
                    }
                    return Err(error);
                }
            };
            if let Err(error) = output.sync_all() {
                drop(output);
                let _ = fs::remove_file(destination);
                return Err(ManagerError::io("sync release asset", error));
            }
            drop(output);
            if copied == 0 || copied > max_size || expected_size.is_some_and(|size| copied != size)
            {
                let _ = fs::remove_file(destination);
                let error = ManagerError::new(
                    "release_asset_size_mismatch",
                    format!("release asset {asset_name} has an unexpected size: {copied} bytes"),
                );
                if self.switch_after_failed_download(route, &mut progress) {
                    continue;
                }
                return Err(error);
            }
            if let Some(progress) = progress.as_deref_mut() {
                progress(InstallEvent::VerifyingArtifact);
            }
            let (actual, size) = match sha256_file(destination) {
                Ok(fingerprint) => fingerprint,
                Err(error) => {
                    let _ = fs::remove_file(destination);
                    return Err(error);
                }
            };
            if size != copied || expected_sha256.is_some_and(|expected| actual != expected) {
                let _ = fs::remove_file(destination);
                let error = ManagerError::new(
                    "release_asset_hash_mismatch",
                    format!("release asset failed SHA-256 verification: {asset_name}"),
                );
                if self.switch_after_failed_download(route, &mut progress) {
                    continue;
                }
                return Err(error);
            }
            return Ok(true);
        }
    }

    fn switch_after_failed_download(
        &self,
        failed_route: GitHubRoute,
        progress: &mut Option<&mut dyn FnMut(InstallEvent)>,
    ) -> bool {
        let GitHubRoute::Proxy(failed) = failed_route else {
            return false;
        };
        let next = {
            let mut order = self.proxy_order.borrow_mut();
            if order.len() <= 1 || !order.contains(&failed) {
                return false;
            }
            order.retain(|index| *index != failed);
            order.first().copied()
        };
        let Some(next) = next else {
            return false;
        };
        self.route.set(GitHubRoute::Proxy(next));
        if let Some(progress) = progress.as_deref_mut() {
            progress(InstallEvent::ConnectingArtifact);
        }
        true
    }

    /// Tries the configured GitHub route and applies the direct-to-proxy fallback policy.
    fn get_response(
        &self,
        direct_url: &str,
        accept: &str,
        git_protocol: bool,
        direct_hosts: &[&str],
        context: &str,
        retry_proxy_404: bool,
    ) -> Result<Option<ureq::http::Response<ureq::Body>>> {
        let route = self.route.get();
        if let GitHubRoute::Proxy(index) = route {
            return self.request_from_proxy_pool(
                index,
                direct_url,
                accept,
                git_protocol,
                context,
                retry_proxy_404,
            );
        }
        match self.request_on_route(GitHubRoute::Direct, direct_url, accept, git_protocol) {
            Ok(response) => {
                require_route_host(&response, GitHubRoute::Direct, direct_hosts)?;
                Ok(Some(response))
            }
            Err(GitHubRequestError::DeadlineExceeded) => Err(remote_diagnostic_timeout()),
            Err(GitHubRequestError::Request(ureq::Error::StatusCode(404))) => Ok(None),
            Err(GitHubRequestError::Request(error)) if should_try_proxy(&error) => {
                let direct_error = error.to_string();
                let index = match self.deadline {
                    Some(deadline) => select_proxy_index_until(self.repository, deadline)?,
                    None => select_proxy_index(self.repository),
                };
                self.route.set(GitHubRoute::Proxy(index));
                self.request_from_proxy_pool(
                    index,
                    direct_url,
                    accept,
                    git_protocol,
                    context,
                    retry_proxy_404,
                )
                .map_err(|proxy_error| {
                    ManagerError::new(
                        "network_error",
                        format!(
                            "{context}: GitHub direct failed ({direct_error}); proxy pool failed ({proxy_error})"
                        ),
                    )
                })
            }
            Err(GitHubRequestError::Request(error)) => Err(network_error(context, error)),
        }
    }

    /// Tries proxy routes in order and returns the first valid response.
    fn request_from_proxy_pool(
        &self,
        first: usize,
        direct_url: &str,
        accept: &str,
        git_protocol: bool,
        context: &str,
        retry_404: bool,
    ) -> Result<Option<ureq::http::Response<ureq::Body>>> {
        let mut failures = Vec::new();
        let mut not_found = 0;
        let order = proxy_indices_from(&self.proxy_order.borrow(), first);
        for index in order.iter().copied() {
            let route = GitHubRoute::Proxy(index);
            match self.request_on_route(route, direct_url, accept, git_protocol) {
                Ok(response) => {
                    require_route_host(&response, route, &[])?;
                    self.route.set(route);
                    return Ok(Some(response));
                }
                Err(GitHubRequestError::DeadlineExceeded) => {
                    return Err(remote_diagnostic_timeout());
                }
                Err(GitHubRequestError::Request(ureq::Error::StatusCode(404))) if !retry_404 => {
                    self.route.set(route);
                    return Ok(None);
                }
                Err(GitHubRequestError::Request(ureq::Error::StatusCode(404))) => {
                    not_found += 1;
                    failures.push(format!("{}: 404", GH_PROXY_ROUTES[index].1));
                }
                Err(GitHubRequestError::Request(error)) => {
                    failures.push(format!("{}: {error}", GH_PROXY_ROUTES[index].1));
                }
            }
        }
        if !order.is_empty() && not_found == order.len() {
            return Ok(None);
        }
        Err(ManagerError::new(
            "network_error",
            format!(
                "{context}: all proxy nodes failed ({})",
                failures.join("; ")
            ),
        ))
    }

    /// Sends one request through a selected route while enforcing its remaining time.
    fn request_on_route(
        &self,
        route: GitHubRoute,
        direct_url: &str,
        accept: &str,
        git_protocol: bool,
    ) -> std::result::Result<ureq::http::Response<ureq::Body>, GitHubRequestError> {
        let url = routed_url(route, direct_url);
        if let Some(deadline) = self.deadline {
            let timeout =
                remaining_deadline(deadline).map_err(|_| GitHubRequestError::DeadlineExceeded)?;
            let config = Agent::config_builder()
                .https_only(true)
                .max_redirects(5)
                .timeout_global(Some(timeout))
                .timeout_connect(Some(timeout))
                .timeout_recv_response(Some(timeout))
                .timeout_recv_body(Some(timeout))
                .build();
            let agent = Agent::new_with_config(config);
            let result = Self::request_with_agent(&agent, &url, accept, git_protocol)
                .map_err(GitHubRequestError::Request);
            if Instant::now() >= deadline {
                Err(GitHubRequestError::DeadlineExceeded)
            } else {
                result
            }
        } else {
            Self::request_with_agent(&self.agent, &url, accept, git_protocol)
                .map_err(GitHubRequestError::Request)
        }
    }

    /// Builds and sends an HTTP request using the configured agent.
    fn request_with_agent(
        agent: &Agent,
        url: &str,
        accept: &str,
        git_protocol: bool,
    ) -> std::result::Result<ureq::http::Response<ureq::Body>, ureq::Error> {
        let mut request = agent
            .get(url)
            .header("Accept", accept)
            .header("User-Agent", concat!("csa/", env!("CARGO_PKG_VERSION")));
        if git_protocol {
            request = request.header("Git-Protocol", "version=1");
        }
        request.call()
    }
}

/// Creates the stable timeout error used by remote diagnostics.
fn remote_diagnostic_timeout() -> ManagerError {
    ManagerError::new(
        "remote_diagnostic_timeout",
        "remote diagnostic deadline exceeded",
    )
}

fn detect_github_route() -> Option<GitHubRoute> {
    let probes = std::thread::scope(|scope| {
        let cloudflare = scope.spawn(detect_cloudflare_country);
        let alibaba = scope.spawn(detect_alibaba_country);
        [
            cloudflare.join().ok().flatten(),
            alibaba.join().ok().flatten(),
        ]
    });
    route_from_region_probes(probes)
}

/// Detects the region with a short probe deadline and defaults to direct routing.
fn detect_github_route_until(deadline: Instant) -> Result<Option<GitHubRoute>> {
    let probe_deadline = remote_region_probe_deadline(Instant::now(), deadline);
    let probes = std::thread::scope(|scope| {
        let cloudflare = scope.spawn(|| detect_cloudflare_country_until(probe_deadline));
        let alibaba = scope.spawn(|| detect_alibaba_country_until(probe_deadline));
        [
            cloudflare.join().ok().flatten(),
            alibaba.join().ok().flatten(),
        ]
    });
    remaining_deadline(deadline)?;
    Ok(Some(remote_route_from_region_probes(probes)))
}

/// Caps the region-probe deadline at both its short limit and the overall deadline.
fn remote_region_probe_deadline(start: Instant, overall_deadline: Instant) -> Instant {
    start
        .checked_add(REMOTE_REGION_PROBE_TIMEOUT)
        .map_or(overall_deadline, |limit| limit.min(overall_deadline))
}

fn detect_cloudflare_country() -> Option<bool> {
    let bytes = read_region_response(CLOUDFLARE_TRACE_URL, "text/plain", &["www.cloudflare.com"])?;
    country_from_cloudflare_trace(&bytes)
}

/// Reads Cloudflare's country result before the supplied deadline.
fn detect_cloudflare_country_until(deadline: Instant) -> Option<bool> {
    let timeout = remaining_deadline(deadline).ok()?;
    let bytes = read_region_response_with_timeout(
        CLOUDFLARE_TRACE_URL,
        "text/plain",
        &["www.cloudflare.com"],
        timeout,
    )?;
    country_from_cloudflare_trace(&bytes)
}

fn detect_alibaba_country() -> Option<bool> {
    let bytes = read_region_response(ALIBABA_REGION_URL, "application/json", &["ip.taobao.com"])?;
    country_from_alibaba_region(&bytes)
}

/// Reads Alibaba's country result before the supplied deadline.
fn detect_alibaba_country_until(deadline: Instant) -> Option<bool> {
    let timeout = remaining_deadline(deadline).ok()?;
    let bytes = read_region_response_with_timeout(
        ALIBABA_REGION_URL,
        "application/json",
        &["ip.taobao.com"],
        timeout,
    )?;
    country_from_alibaba_region(&bytes)
}

/// Reads a region response using the standard non-diagnostic timeout.
fn read_region_response(url: &str, accept: &str, allowed_hosts: &[&str]) -> Option<Vec<u8>> {
    read_region_response_with_timeout(url, accept, allowed_hosts, Duration::from_secs(5))
}

/// Fetches a bounded region response and validates its final host.
fn read_region_response_with_timeout(
    url: &str,
    accept: &str,
    allowed_hosts: &[&str],
    timeout: Duration,
) -> Option<Vec<u8>> {
    let config = Agent::config_builder()
        .https_only(true)
        .max_redirects(0)
        .timeout_global(Some(timeout.min(Duration::from_secs(5))))
        .timeout_connect(Some(timeout.min(Duration::from_secs(3))))
        .timeout_recv_response(Some(timeout.min(Duration::from_secs(3))))
        .timeout_recv_body(Some(timeout.min(Duration::from_secs(3))))
        .build();
    let agent = Agent::new_with_config(config);
    let mut response = agent
        .get(url)
        .header("Accept", accept)
        .header("User-Agent", concat!("csa/", env!("CARGO_PKG_VERSION")))
        .call()
        .ok()?;
    require_response_host(&response, allowed_hosts).ok()?;
    let bytes = response
        .body_mut()
        .with_config()
        .limit(MAX_REGION_TRACE_BYTES + 1)
        .read_to_vec()
        .ok()?;
    if bytes.len() as u64 > MAX_REGION_TRACE_BYTES {
        return None;
    }
    Some(bytes)
}

fn country_from_cloudflare_trace(bytes: &[u8]) -> Option<bool> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut country = None;
    for line in text.lines() {
        let Some(value) = line.strip_prefix("loc=") else {
            continue;
        };
        if country.replace(value).is_some() {
            return None;
        }
    }
    country_code_is_cn(country?)
}

#[derive(Deserialize)]
struct AlibabaRegionResponse {
    code: u8,
    data: Option<AlibabaRegionData>,
}

#[derive(Deserialize)]
struct AlibabaRegionData {
    country_id: String,
}

fn country_from_alibaba_region(bytes: &[u8]) -> Option<bool> {
    let response: AlibabaRegionResponse = serde_json::from_slice(bytes).ok()?;
    if response.code != 0 {
        return None;
    }
    country_code_is_cn(&response.data?.country_id)
}

fn country_code_is_cn(country: &str) -> Option<bool> {
    if country.len() != 2 || !country.bytes().all(|byte| byte.is_ascii_uppercase()) {
        return None;
    }
    Some(country == "CN")
}

fn route_from_region_probes(probes: [Option<bool>; 2]) -> Option<GitHubRoute> {
    if probes.contains(&Some(true)) {
        Some(GitHubRoute::Proxy(0))
    } else if probes.iter().any(Option::is_some) {
        Some(GitHubRoute::Direct)
    } else {
        None
    }
}

/// Uses the detected route, or direct routing when both region probes fail.
fn remote_route_from_region_probes(probes: [Option<bool>; 2]) -> GitHubRoute {
    route_from_region_probes(probes).unwrap_or(GitHubRoute::Direct)
}

fn routed_url(route: GitHubRoute, direct_url: &str) -> String {
    match route {
        GitHubRoute::Direct => direct_url.to_owned(),
        GitHubRoute::Proxy(index) => format!("{}{direct_url}", GH_PROXY_ROUTES[index].0),
    }
}

fn proxy_indices_from(order: &[usize], first: usize) -> Vec<usize> {
    let Some(position) = order.iter().position(|index| *index == first) else {
        return order.to_vec();
    };
    order[position..]
        .iter()
        .chain(&order[..position])
        .copied()
        .collect()
}

fn sample_proxy_routes(
    active: &[usize],
    direct_url: &str,
    expected_size: u64,
) -> Vec<(usize, Duration)> {
    std::thread::scope(|scope| {
        let handles: Vec<_> = active
            .iter()
            .copied()
            .map(|index| {
                scope.spawn(move || {
                    sample_proxy_route(index, direct_url, expected_size)
                        .map(|elapsed| (index, elapsed))
                })
            })
            .collect();
        handles
            .into_iter()
            .filter_map(|handle| handle.join().ok().flatten())
            .collect()
    })
}

fn sample_proxy_route(index: usize, direct_url: &str, expected_size: u64) -> Option<Duration> {
    let sample_size = expected_size.min(GH_PROXY_SAMPLE_BYTES);
    if sample_size == 0 {
        return None;
    }
    let config = Agent::config_builder()
        .https_only(true)
        .max_redirects(5)
        .timeout_global(Some(GH_PROXY_SAMPLE_TIMEOUT))
        .timeout_connect(Some(Duration::from_secs(2)))
        .timeout_recv_response(Some(Duration::from_secs(2)))
        .timeout_recv_body(Some(Duration::from_secs(2)))
        .build();
    let agent = Agent::new_with_config(config);
    let started = Instant::now();
    let mut response = agent
        .get(&routed_url(GitHubRoute::Proxy(index), direct_url))
        .header("Accept", "application/octet-stream")
        .header("Range", &format!("bytes=0-{}", sample_size - 1))
        .header("User-Agent", concat!("csa/", env!("CARGO_PKG_VERSION")))
        .call()
        .ok()?;
    if response.status().as_u16() != 206
        || require_route_host(&response, GitHubRoute::Proxy(index), &[]).is_err()
        || !response
            .headers()
            .get("Content-Range")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| content_range_matches(value, sample_size, expected_size))
        || response
            .body()
            .content_length()
            .is_some_and(|length| length != sample_size)
    {
        return None;
    }
    let mut reader = response
        .body_mut()
        .with_config()
        .limit(sample_size + 1)
        .reader();
    let copied = io::copy(&mut reader, &mut io::sink()).ok()?;
    (copied == sample_size).then(|| started.elapsed())
}

fn content_range_matches(value: &str, sample_size: u64, expected_size: u64) -> bool {
    let Some((range, total)) = value
        .strip_prefix("bytes ")
        .and_then(|value| value.split_once('/'))
    else {
        return false;
    };
    let Some((start, end)) = range.split_once('-') else {
        return false;
    };
    start.parse::<u64>() == Ok(0)
        && end.parse::<u64>().ok().and_then(|end| end.checked_add(1)) == Some(sample_size)
        && total.parse::<u64>() == Ok(expected_size)
}

fn rank_proxy_indices(active: &[usize], mut samples: Vec<(usize, Duration)>) -> Vec<usize> {
    samples.retain(|(index, _)| active.contains(index));
    samples.sort_by(|left, right| left.1.cmp(&right.1).then_with(|| left.0.cmp(&right.0)));
    let mut seen = BTreeSet::new();
    let mut ranked = Vec::with_capacity(active.len());
    for (index, _) in samples {
        if seen.insert(index) {
            ranked.push(index);
        }
    }
    for index in active.iter().copied() {
        if seen.insert(index) {
            ranked.push(index);
        }
    }
    ranked
}

fn select_proxy_index(repository: &'static str) -> usize {
    let (sender, receiver) = mpsc::channel();
    for index in 0..GH_PROXY_ROUTES.len() {
        let sender = sender.clone();
        std::thread::spawn(move || {
            if proxy_responds(index, repository) {
                let _ = sender.send(index);
            }
        });
    }
    drop(sender);
    receiver.recv_timeout(Duration::from_secs(4)).unwrap_or(0)
}

/// Selects a responsive proxy before the diagnostic deadline.
fn select_proxy_index_until(repository: &'static str, deadline: Instant) -> Result<usize> {
    select_proxy_index_until_with(repository, deadline, proxy_responds_until)
}

/// Probes proxy routes concurrently and returns a responsive route or an error.
fn select_proxy_index_until_with(
    repository: &'static str,
    deadline: Instant,
    probe: fn(usize, &'static str, Instant) -> bool,
) -> Result<usize> {
    let (sender, receiver) = mpsc::channel();
    for index in 0..GH_PROXY_ROUTES.len() {
        let sender = sender.clone();
        std::thread::spawn(move || {
            if probe(index, repository, deadline) {
                let _ = sender.send(index);
            }
        });
    }
    drop(sender);
    let timeout = remaining_deadline(deadline)?.min(Duration::from_secs(4));
    let selected = match receiver.recv_timeout(timeout) {
        Ok(selected) => selected,
        Err(mpsc::RecvTimeoutError::Timeout) if Instant::now() >= deadline => {
            return Err(remote_diagnostic_timeout());
        }
        Err(_) => {
            return Err(ManagerError::new(
                "network_error",
                "no GitHub proxy route responded to the probe",
            ));
        }
    };
    remaining_deadline(deadline)?;
    Ok(selected)
}

/// Checks proxy responsiveness with the normal probe timeout.
fn proxy_responds(index: usize, repository: &str) -> bool {
    proxy_responds_with_timeout(index, repository, Duration::from_secs(4))
}

/// Checks proxy responsiveness using only the remaining diagnostic time.
fn proxy_responds_until(index: usize, repository: &'static str, deadline: Instant) -> bool {
    remaining_deadline(deadline)
        .ok()
        .is_some_and(|timeout| proxy_responds_with_timeout(index, repository, timeout))
}

/// Probes a proxy with bounded connect, response, and total timeouts.
fn proxy_responds_with_timeout(index: usize, repository: &str, timeout: Duration) -> bool {
    let config = Agent::config_builder()
        .https_only(true)
        .max_redirects(2)
        .timeout_global(Some(timeout.min(Duration::from_secs(4))))
        .timeout_connect(Some(timeout.min(Duration::from_secs(3))))
        .timeout_recv_response(Some(timeout.min(Duration::from_secs(3))))
        .build();
    let agent = Agent::new_with_config(config);
    let response = agent
        .get(&routed_url(
            GitHubRoute::Proxy(index),
            &git_refs_url(repository),
        ))
        .header("Accept", "application/x-git-upload-pack-advertisement")
        .header("Git-Protocol", "version=1")
        .header("User-Agent", concat!("csa/", env!("CARGO_PKG_VERSION")))
        .call();
    response
        .as_ref()
        .is_ok_and(|response| require_route_host(response, GitHubRoute::Proxy(index), &[]).is_ok())
}

fn git_refs_url(repository: &str) -> String {
    format!("https://github.com/{repository}.git/info/refs?service=git-upload-pack")
}

fn release_asset_url(repository: &str, release_tag: &str, asset_name: &str) -> String {
    format!("https://github.com/{repository}/releases/download/{release_tag}/{asset_name}")
}

fn should_try_proxy(error: &ureq::Error) -> bool {
    matches!(
        error,
        ureq::Error::StatusCode(403 | 408 | 429 | 500..=599)
            | ureq::Error::Protocol(_)
            | ureq::Error::Io(_)
            | ureq::Error::Timeout(_)
            | ureq::Error::HostNotFound
            | ureq::Error::ConnectionFailed
            | ureq::Error::ConnectProxyFailed(_)
    )
}

fn require_route_host(
    response: &ureq::http::Response<ureq::Body>,
    route: GitHubRoute,
    direct_hosts: &[&str],
) -> Result<()> {
    match route {
        GitHubRoute::Direct => require_response_host(response, direct_hosts),
        GitHubRoute::Proxy(index) => require_response_host(response, &[GH_PROXY_ROUTES[index].1]),
    }
}

fn compatibility_tags(refs: &BTreeMap<String, String>) -> Result<Vec<String>> {
    let mut tags = Vec::new();
    for reference in refs.keys() {
        let Some(tag) = reference.strip_prefix("refs/tags/compat-") else {
            continue;
        };
        if tag.ends_with("^{}") {
            continue;
        }
        validate_asset_name(tag)?;
        let version = tag
            .strip_prefix("rust-v")
            .and_then(|value| value.split_once('-').map(|(version, _)| version))
            .ok_or_else(|| {
                invalid_install_catalog("compatibility tag has no numeric Codex version")
            })?;
        tags.push((
            format!("compat-{tag}"),
            version_key(version).map_err(|_| {
                invalid_install_catalog("compatibility tag has an invalid Codex version")
            })?,
            patch_revision(tag).map_err(|_| {
                invalid_install_catalog("compatibility tag has an invalid patch revision")
            })?,
        ));
    }
    if tags.len() > MAX_COMPATIBILITY_TAGS {
        return Err(ManagerError::new(
            "compatibility_catalog_too_large",
            "compatibility catalog reached the 1,000-tag safety limit",
        ));
    }
    tags.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then_with(|| right.2.cmp(&left.2))
            .then_with(|| left.0.cmp(&right.0))
    });
    Ok(tags.into_iter().map(|(tag, _, _)| tag).collect())
}

fn peel_tag_from_refs(refs: &BTreeMap<String, String>, tag: &str) -> Result<String> {
    validate_asset_name(tag)?;
    let reference = format!("refs/tags/{tag}");
    let commit = refs
        .get(&format!("{reference}^{{}}"))
        .or_else(|| refs.get(&reference))
        .ok_or_else(|| {
            ManagerError::new(
                "invalid_release_tag",
                format!("GitHub tag does not exist: {tag}"),
            )
        })?
        .clone();
    validate_sha(&commit)?;
    Ok(commit)
}

fn parse_git_refs(bytes: &[u8]) -> Result<BTreeMap<String, String>> {
    let mut refs = BTreeMap::new();
    let mut offset = 0;
    while offset < bytes.len() {
        if bytes.len() - offset < 4 {
            return Err(invalid_git_refs());
        }
        let length = std::str::from_utf8(&bytes[offset..offset + 4])
            .ok()
            .and_then(|value| usize::from_str_radix(value, 16).ok())
            .ok_or_else(invalid_git_refs)?;
        offset += 4;
        if length <= 2 {
            continue;
        }
        if length < 4 || offset + length - 4 > bytes.len() {
            return Err(invalid_git_refs());
        }
        let payload = &bytes[offset..offset + length - 4];
        offset += length - 4;
        let payload = payload
            .strip_suffix(b"\n")
            .unwrap_or(payload)
            .split(|byte| *byte == 0)
            .next()
            .unwrap_or_default();
        if payload.is_empty() || payload.starts_with(b"# service=") || payload == b"version 1" {
            continue;
        }
        let Some(separator) = payload.iter().position(|byte| *byte == b' ') else {
            return Err(invalid_git_refs());
        };
        let sha = std::str::from_utf8(&payload[..separator]).map_err(|_| invalid_git_refs())?;
        let reference =
            std::str::from_utf8(&payload[separator + 1..]).map_err(|_| invalid_git_refs())?;
        validate_sha(sha)?;
        if !reference.starts_with("refs/") {
            continue;
        }
        if reference.is_empty()
            || reference
                .bytes()
                .any(|byte| byte.is_ascii_whitespace() || byte == 0)
            || refs.insert(reference.to_owned(), sha.to_owned()).is_some()
        {
            return Err(invalid_git_refs());
        }
    }
    if refs.is_empty() {
        return Err(invalid_git_refs());
    }
    Ok(refs)
}

fn invalid_git_refs() -> ManagerError {
    ManagerError::new(
        "invalid_github_response",
        "GitHub returned an invalid Git ref advertisement",
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallCatalog {
    schema: u32,
    repository: String,
    source_release_tag: String,
    source_commit: String,
    entries: Vec<InstallCatalogEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallCatalogEntry {
    compat_id: String,
    release_tag: String,
    release_commit: String,
    codex_version: String,
    #[serde(default)]
    build_target: Option<String>,
    #[serde(default)]
    build_targets: Vec<String>,
    patch_revision: u64,
    recorded_on: String,
}

impl InstallCatalogEntry {
    fn supports_target(&self, target: &str) -> bool {
        self.build_target.as_deref() == Some(target)
            || self
                .build_targets
                .iter()
                .any(|candidate| candidate == target)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CompatibilityRelease {
    schema: u32,
    repository: String,
    release_tag: String,
    source_commit: String,
    compat_id: String,
    upstream: UpstreamRelease,
    #[serde(default)]
    build_target: Option<String>,
    payload: Vec<ReleaseFile>,
    #[serde(default)]
    artifact: Option<ReleaseFile>,
    #[serde(default)]
    artifacts: BTreeMap<String, ReleaseFile>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpstreamRelease {
    repository: String,
    version: String,
    tag: String,
    commit: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseFile {
    path: String,
    asset: String,
    size: u64,
    sha256: String,
}

fn stable_release_version(tag: &str) -> Result<String> {
    let version = tag.strip_prefix("rust-v").ok_or_else(|| {
        ManagerError::new(
            "invalid_upstream_release",
            "official stable tag must use rust-vX.Y.Z",
        )
    })?;
    parse_codex_version(&format!("codex-cli {version}")).map_err(|_| {
        ManagerError::new(
            "invalid_upstream_release",
            "official tag is not rust-vX.Y.Z",
        )
    })
}

fn parse_checksums(bytes: &[u8]) -> Result<BTreeMap<String, String>> {
    let text = std::str::from_utf8(bytes).map_err(|_| {
        ManagerError::new(
            "invalid_release_checksums",
            "SHA256SUMS must be valid UTF-8",
        )
    })?;
    let mut checksums = BTreeMap::new();
    for line in text.lines() {
        let (digest, name) = line.split_once("  ").ok_or_else(|| {
            ManagerError::new(
                "invalid_release_checksums",
                "SHA256SUMS entries must use '<sha256>  <asset>'",
            )
        })?;
        let digest = digest.to_ascii_lowercase();
        validate_sha256(&digest)?;
        validate_asset_name(name)?;
        if checksums.insert(name.to_owned(), digest).is_some() {
            return Err(ManagerError::new(
                "invalid_release_checksums",
                format!("duplicate checksum entry: {name}"),
            ));
        }
    }
    if checksums.is_empty() {
        return Err(ManagerError::new(
            "invalid_release_checksums",
            "SHA256SUMS must not be empty",
        ));
    }
    Ok(checksums)
}

fn read_json_file<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = fs::read(path)
        .map_err(|error| ManagerError::io(&format!("read JSON {}", path.display()), error))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| ManagerError::new("invalid_compatibility_release", error.to_string()))
}

fn read_install_catalog(path: &Path) -> Result<InstallCatalog> {
    let bytes = fs::read(path)
        .map_err(|error| ManagerError::io(&format!("read JSON {}", path.display()), error))?;
    serde_json::from_slice(&bytes).map_err(|error| {
        invalid_install_catalog(format!("install catalog is invalid JSON: {error}"))
    })
}

fn descriptor_supports_target(descriptor: &CompatibilityRelease, target: &str) -> bool {
    match descriptor.schema {
        1 => descriptor.build_target.as_deref() == Some(target) && descriptor.artifact.is_some(),
        2 => descriptor.artifacts.contains_key(target),
        _ => false,
    }
}

fn descriptor_artifact<'a>(
    descriptor: &'a CompatibilityRelease,
    target: &str,
) -> Result<&'a ReleaseFile> {
    match descriptor.schema {
        1 if descriptor.build_target.as_deref() == Some(target)
            && descriptor.artifacts.is_empty() =>
        {
            descriptor.artifact.as_ref()
        }
        2 if descriptor.build_target.is_none() && descriptor.artifact.is_none() => {
            descriptor.artifacts.get(target)
        }
        _ => None,
    }
    .ok_or_else(|| {
        ManagerError::new(
            "invalid_compatibility_release",
            format!("compatibility release has no exact artifact for {target}"),
        )
    })
}

fn validate_catalog_descriptor(
    descriptor: &CompatibilityRelease,
    expected_repository: &str,
    release_tag: &str,
    release_commit: &str,
    compat_id: &str,
) -> Result<()> {
    validate_asset_name(compat_id)?;
    match descriptor.schema {
        1 if descriptor.build_target.is_some()
            && descriptor.artifact.is_some()
            && descriptor.artifacts.is_empty() =>
        {
            validate_asset_name(
                descriptor
                    .build_target
                    .as_deref()
                    .expect("schema 1 target checked"),
            )?;
        }
        2 if descriptor.build_target.is_none()
            && descriptor.artifact.is_none()
            && !descriptor.artifacts.is_empty() =>
        {
            for target in descriptor.artifacts.keys() {
                validate_asset_name(target)?;
            }
        }
        _ => {
            return Err(ManagerError::new(
                "invalid_compatibility_release",
                "compatibility descriptor target fields do not match its schema",
            ));
        }
    }
    validate_sha(release_commit)?;
    validate_sha(&descriptor.source_commit)?;
    validate_sha(&descriptor.upstream.commit)?;
    let parsed_version = parse_codex_version(&format!("codex-cli {}", descriptor.upstream.version))
        .map_err(|_| {
            ManagerError::new(
                "invalid_compatibility_release",
                "descriptor upstream version must use numeric X.Y.Z",
            )
        })?;
    version_key(&parsed_version)?;
    if !descriptor
        .repository
        .eq_ignore_ascii_case(expected_repository)
        || descriptor.release_tag != release_tag
        || descriptor.source_commit != release_commit
        || descriptor.compat_id != compat_id
        || descriptor.upstream.repository != OPENAI_REPOSITORY
        || descriptor.upstream.version != parsed_version
        || descriptor.upstream.tag != format!("rust-v{parsed_version}")
    {
        return Err(ManagerError::new(
            "invalid_compatibility_release",
            "compatibility descriptor differs from its formal release identity",
        ));
    }
    Ok(())
}

fn descriptor_assets(descriptor: &CompatibilityRelease) -> Result<BTreeMap<String, &ReleaseFile>> {
    let mut assets = BTreeMap::new();
    let mut paths = BTreeSet::new();
    for file in &descriptor.payload {
        validate_relative(&file.path, false)?;
        validate_asset_name(&file.asset)?;
        validate_sha256(&file.sha256)?;
        if file.size == 0 || file.size > MAX_RELEASE_FILE_BYTES {
            return Err(ManagerError::new(
                "invalid_compatibility_release",
                format!("declared file size is invalid: {}", file.path),
            ));
        }
        if assets.insert(file.asset.clone(), file).is_some() {
            return Err(ManagerError::new(
                "invalid_compatibility_release",
                format!("descriptor repeats release asset: {}", file.asset),
            ));
        }
        if !paths.insert(file.path.clone()) {
            return Err(ManagerError::new(
                "invalid_compatibility_release",
                format!("descriptor repeats file path: {}", file.path),
            ));
        }
    }
    let artifacts: Vec<&ReleaseFile> = match descriptor.schema {
        1 => descriptor.artifact.iter().collect(),
        2 => descriptor.artifacts.values().collect(),
        _ => Vec::new(),
    };
    for file in artifacts {
        validate_relative(&file.path, false)?;
        validate_asset_name(&file.asset)?;
        validate_sha256(&file.sha256)?;
        if file.size == 0 || file.size > MAX_ARTIFACT_BYTES {
            return Err(ManagerError::new(
                "invalid_compatibility_release",
                format!("declared file size is invalid: {}", file.path),
            ));
        }
        if assets.insert(file.asset.clone(), file).is_some() {
            return Err(ManagerError::new(
                "invalid_compatibility_release",
                format!("descriptor repeats release asset: {}", file.asset),
            ));
        }
    }
    Ok(assets)
}

fn validate_declared_asset(file: &ReleaseFile, checksums: &BTreeMap<String, String>) -> Result<()> {
    if checksums.get(&file.asset) != Some(&file.sha256) {
        return Err(ManagerError::new(
            "invalid_compatibility_release",
            format!("release checksum differs for asset: {}", file.asset),
        ));
    }
    Ok(())
}

fn require_response_host(
    response: &ureq::http::Response<ureq::Body>,
    allowed_hosts: &[&str],
) -> Result<()> {
    require_uri_host(response.get_uri(), allowed_hosts)
}

fn require_uri_host(uri: &ureq::http::Uri, allowed_hosts: &[&str]) -> Result<()> {
    let allowed = uri.scheme_str() == Some("https")
        && uri.host().is_some_and(|host| allowed_hosts.contains(&host));
    if !allowed {
        return Err(ManagerError::new(
            "unsafe_download_redirect",
            format!("download redirected outside approved GitHub hosts: {uri}"),
        ));
    }
    Ok(())
}

fn validate_asset_name(value: &str) -> Result<()> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    {
        return Err(ManagerError::new(
            "invalid_release_asset_name",
            format!("invalid flat release asset name: {value}"),
        ));
    }
    Ok(())
}

fn validate_sha(value: &str) -> Result<()> {
    if value.len() != 40
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ManagerError::new(
            "invalid_release_commit",
            "release commit must be a lowercase 40-hex SHA",
        ));
    }
    Ok(())
}

fn validate_sha256(value: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ManagerError::new(
            "invalid_release_digest",
            "release digest must be a lowercase SHA-256",
        ));
    }
    Ok(())
}

/// Maps a GitHub request failure to a stable manager error code.
fn network_error(context: &str, error: impl std::fmt::Display) -> ManagerError {
    ManagerError::new("network_error", format!("{context}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    const WINDOWS_TARGET: &str = "x86_64-pc-windows-msvc";
    static TEMP_ID: AtomicU64 = AtomicU64::new(0);

    #[derive(Default)]
    struct FixtureSource {
        refs: BTreeMap<&'static str, Option<BTreeMap<String, String>>>,
        catalogs: BTreeMap<(&'static str, String), Option<Vec<u8>>>,
        refs_requests: Vec<&'static str>,
        catalog_requests: Vec<(&'static str, String)>,
        delay: Option<Duration>,
    }

    impl RemoteMetadataSource for FixtureSource {
        /// Returns fixture refs for a repository and records the request.
        fn repository_refs(
            &mut self,
            repository: &'static str,
            _deadline: Instant,
        ) -> Result<Option<BTreeMap<String, String>>> {
            self.refs_requests.push(repository);
            if let Some(delay) = self.delay {
                std::thread::sleep(delay);
            }
            Ok(self.refs.get(repository).cloned().flatten())
        }

        /// Returns fixture catalog bytes for a tag and records the request.
        fn catalog(
            &mut self,
            repository: &'static str,
            tag: &str,
            _deadline: Instant,
        ) -> Result<Option<Vec<u8>>> {
            self.catalog_requests.push((repository, tag.to_owned()));
            if let Some(delay) = self.delay {
                std::thread::sleep(delay);
            }
            Ok(self
                .catalogs
                .get(&(repository, tag.to_owned()))
                .cloned()
                .flatten())
        }
    }

    struct TestTempDir(PathBuf);

    impl TestTempDir {
        /// Creates an empty fixture metadata source.
        fn new() -> Self {
            let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("csa-online-test-{}-{id}", std::process::id()));
            ensure_private_cache_directory(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestTempDir {
        /// Removes the temporary test directory when its fixture is dropped.
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Builds a valid install candidate fixture from a version, ID, and target.
    fn candidate(version: &str, compat_id: &str, target: &str) -> InstallCandidate {
        let revision = patch_revision(compat_id).unwrap();
        InstallCandidate {
            repository: LEGACY_COMPAT_REPOSITORY.to_owned(),
            compat_id: compat_id.to_owned(),
            codex_version: version.to_owned(),
            build_target: target.to_owned(),
            patch_revision: revision,
            recorded_on: "2026-09-26".to_owned(),
            recommended: false,
            release_tag: format!("compat-{compat_id}"),
            release_commit: "a".repeat(40),
        }
    }

    /// Adds a catalog and matching refs to a fixture metadata source.
    fn add_catalog(
        source: &mut FixtureSource,
        repository: &'static str,
        target: &str,
        mut entries: Vec<(&str, &str, u64)>,
    ) -> BTreeMap<String, String> {
        entries.sort_by(|left, right| {
            version_key(right.0)
                .unwrap()
                .cmp(&version_key(left.0).unwrap())
                .then_with(|| right.2.cmp(&left.2))
                .then_with(|| left.1.cmp(right.1))
        });
        let catalog_entries: Vec<_> = entries
            .iter()
            .enumerate()
            .map(
                |(index, (version, compat_id, revision))| InstallCatalogEntry {
                    compat_id: (*compat_id).to_owned(),
                    release_tag: format!("compat-{compat_id}"),
                    release_commit: format!("{:040x}", index + 1),
                    codex_version: (*version).to_owned(),
                    build_target: Some(target.to_owned()),
                    build_targets: Vec::new(),
                    patch_revision: *revision,
                    recorded_on: "2026-09-26".to_owned(),
                },
            )
            .collect();
        let first = catalog_entries.first().unwrap();
        let refs: BTreeMap<_, _> = catalog_entries
            .iter()
            .map(|entry| {
                (
                    format!("refs/tags/{}", entry.release_tag),
                    entry.release_commit.clone(),
                )
            })
            .collect();
        let catalog = serde_json::json!({
            "schema": 1,
            "repository": repository,
            "source_release_tag": first.release_tag,
            "source_commit": first.release_commit,
            "entries": catalog_entries.iter().map(|entry| serde_json::json!({
                "compat_id": entry.compat_id,
                "release_tag": entry.release_tag,
                "release_commit": entry.release_commit,
                "codex_version": entry.codex_version,
                "build_target": entry.build_target,
                "patch_revision": entry.patch_revision,
                "recorded_on": entry.recorded_on,
            })).collect::<Vec<_>>(),
        });
        source.refs.insert(repository, Some(refs.clone()));
        source.catalogs.insert(
            (repository, first.release_tag.clone()),
            Some(serde_json::to_vec(&catalog).unwrap()),
        );
        refs
    }

    /// Formats a compatibility ID with the requested numeric revision.
    fn release_id(version: &str, revision: u64, variant: &str) -> String {
        format!("rust-v{version}-{variant}-p{revision}")
    }

    /// Builds prepared compatibility metadata for remote-report tests.
    fn prepared_metadata(
        compat_id: &str,
        codex_version: Option<&str>,
    ) -> PreparedCompatibilityMetadata {
        PreparedCompatibilityMetadata {
            compat_id: compat_id.to_owned(),
            codex_version: codex_version.map(str::to_owned),
        }
    }

    /// Checks that report selection follows install rules and retains the latest candidate.
    #[test]
    fn remote_report_uses_install_selection_and_preserves_latest_candidate() {
        let target = "x86_64-unknown-linux-musl";
        let matching = vec![
            candidate("0.150.1", &release_id("0.150.1", 14, "native-join"), target),
            candidate("0.150.1", &release_id("0.150.1", 15, "native-join"), target),
        ];
        let report = remote_compatibility_report(
            &matching,
            "0.150.1",
            "x86_64-unknown-linux-gnu",
            Some(&prepared_metadata(
                "rust-v0.150.1-native-join-p14",
                Some("0.150.1"),
            )),
        );
        assert_eq!(report.status, "match");
        assert_eq!(report.artifact_target, target);
        assert_eq!(
            serde_json::to_value(&report).unwrap()["artifact_target"],
            target
        );
        assert_eq!(
            report.recommended_compat_id.as_deref(),
            Some("rust-v0.150.1-native-join-p15")
        );
        assert!(report.update_available);

        let latest = vec![
            candidate("0.149.0", &release_id("0.149.0", 99, "native-join"), target),
            candidate("0.152.0", &release_id("0.152.0", 3, "native-join"), target),
            candidate("0.152.0", &release_id("0.152.0", 8, "native-join"), target),
        ];
        let report =
            remote_compatibility_report(&latest, "0.150.1", "x86_64-unknown-linux-gnu", None);
        assert_eq!(report.status, "none_for_version");
        assert_eq!(report.official_version_relation, Some("older"));
        let latest = report.latest_candidate.unwrap();
        assert_eq!(latest.codex_version, "0.152.0");
        assert_eq!(latest.compat_id, "rust-v0.152.0-native-join-p8");
        assert_eq!(latest.patch_revision, 8);
    }

    /// Covers installed, unprepared, outdated, unknown-revision, and version-mismatch cases.
    #[test]
    fn remote_report_covers_current_match_and_both_version_relations() {
        let matching = vec![
            candidate("0.150.1", "rust-v0.150.1-native-join-p14", WINDOWS_TARGET),
            candidate("0.150.1", "rust-v0.150.1-native-join-p15", WINDOWS_TARGET),
        ];
        let unprepared = remote_compatibility_report(&matching, "0.150.1", WINDOWS_TARGET, None);
        assert_eq!(unprepared.status, "match");
        assert_eq!(
            unprepared.recommended_compat_id.as_deref(),
            Some("rust-v0.150.1-native-join-p15")
        );
        assert!(!unprepared.update_available);

        let current = remote_compatibility_report(
            &matching,
            "0.150.1",
            WINDOWS_TARGET,
            Some(&prepared_metadata(
                "rust-v0.150.1-native-join-p15",
                Some("0.150.1"),
            )),
        );
        assert_eq!(current.status, "match");
        assert!(!current.update_available);

        let newer_available = remote_compatibility_report(
            &matching,
            "0.150.1",
            WINDOWS_TARGET,
            Some(&prepared_metadata(
                "rust-v0.150.1-native-join-p14",
                Some("0.150.1"),
            )),
        );
        assert_eq!(newer_available.status, "match");
        assert!(newer_available.update_available);

        let unknown_prepared_revision = remote_compatibility_report(
            &matching,
            "0.150.1",
            WINDOWS_TARGET,
            Some(&prepared_metadata("custom-local-build", Some("0.150.1"))),
        );
        assert!(!unknown_prepared_revision.update_available);

        let different_prepared_official = remote_compatibility_report(
            &matching,
            "0.150.1",
            WINDOWS_TARGET,
            Some(&prepared_metadata(
                "rust-v0.149.0-native-join-p20",
                Some("0.149.0"),
            )),
        );
        assert!(different_prepared_official.update_available);

        let latest = vec![candidate(
            "0.152.0",
            "rust-v0.152.0-native-join-p8",
            WINDOWS_TARGET,
        )];
        let official_is_older =
            remote_compatibility_report(&latest, "0.150.1", WINDOWS_TARGET, None);
        assert_eq!(official_is_older.status, "none_for_version");
        assert_eq!(official_is_older.official_version_relation, Some("older"));
        assert_eq!(
            official_is_older
                .latest_candidate
                .as_ref()
                .map(|candidate| candidate.compat_id.as_str()),
            Some("rust-v0.152.0-native-join-p8")
        );

        let official_is_newer =
            remote_compatibility_report(&latest, "0.153.0", WINDOWS_TARGET, None);
        assert_eq!(official_is_newer.status, "none_for_version");
        assert_eq!(official_is_newer.official_version_relation, Some("newer"));
    }

    /// Checks that doctor and install select the same candidates and recommendation.
    #[test]
    fn doctor_and_install_share_catalog_target_and_version_selection() {
        let manager_target = "x86_64-unknown-linux-gnu";
        let artifact_target = "x86_64-unknown-linux-musl";
        let repository = LEGACY_COMPAT_REPOSITORY;
        let catalog = InstallCatalog {
            schema: 1,
            repository: repository.to_owned(),
            source_release_tag: "compat-rust-v0.150.1-native-join-p15".to_owned(),
            source_commit: "a".repeat(40),
            entries: vec![
                InstallCatalogEntry {
                    compat_id: "rust-v0.150.1-native-join-p14".to_owned(),
                    release_tag: "compat-rust-v0.150.1-native-join-p14".to_owned(),
                    release_commit: "b".repeat(40),
                    codex_version: "0.150.1".to_owned(),
                    build_target: Some(artifact_target.to_owned()),
                    build_targets: Vec::new(),
                    patch_revision: 14,
                    recorded_on: "2026-09-26".to_owned(),
                },
                InstallCatalogEntry {
                    compat_id: "rust-v0.150.1-native-join-p15".to_owned(),
                    release_tag: "compat-rust-v0.150.1-native-join-p15".to_owned(),
                    release_commit: "c".repeat(40),
                    codex_version: "0.150.1".to_owned(),
                    build_target: Some(artifact_target.to_owned()),
                    build_targets: Vec::new(),
                    patch_revision: 15,
                    recorded_on: "2026-09-26".to_owned(),
                },
                InstallCatalogEntry {
                    compat_id: "rust-v0.150.1-native-join-p20".to_owned(),
                    release_tag: "compat-rust-v0.150.1-native-join-p20".to_owned(),
                    release_commit: "d".repeat(40),
                    codex_version: "0.150.1".to_owned(),
                    build_target: Some(WINDOWS_TARGET.to_owned()),
                    build_targets: Vec::new(),
                    patch_revision: 20,
                    recorded_on: "2026-09-26".to_owned(),
                },
                InstallCatalogEntry {
                    compat_id: "rust-v0.151.0-native-join-p99".to_owned(),
                    release_tag: "compat-rust-v0.151.0-native-join-p99".to_owned(),
                    release_commit: "e".repeat(40),
                    codex_version: "0.151.0".to_owned(),
                    build_target: Some(artifact_target.to_owned()),
                    build_targets: Vec::new(),
                    patch_revision: 99,
                    recorded_on: "2026-09-26".to_owned(),
                },
            ],
        };
        let all_target_candidates =
            catalog_candidates_for_target(&catalog, manager_target, repository);
        let report =
            remote_compatibility_report(&all_target_candidates, "0.150.1", manager_target, None);
        let install_candidates = install_candidates(catalog, "0.150.1", manager_target, repository);
        assert_eq!(
            report.compat_ids,
            install_candidates
                .iter()
                .map(|candidate| candidate.compat_id.clone())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            report.recommended_compat_id.as_deref(),
            Some(
                install_candidates[select_automatic(&install_candidates).unwrap()]
                    .compat_id
                    .as_str()
            )
        );
    }

    /// Checks that equal maximum revisions report install's ambiguity status.
    #[test]
    fn remote_revision_ties_are_reported_as_install_ambiguity() {
        let candidates = vec![
            candidate("0.150.1", "rust-v0.150.1-native-join-p15", WINDOWS_TARGET),
            candidate("0.150.1", "rust-v0.150.1-orbit-p15", WINDOWS_TARGET),
        ];
        assert_eq!(
            select_automatic(&candidates).unwrap_err().code,
            "ambiguous_compatibility_revision"
        );
        let report = remote_compatibility_report(&candidates, "0.150.1", WINDOWS_TARGET, None);
        assert_eq!(report.status, "ambiguous_compatibility_revision");
        assert_eq!(report.recommended_compat_id, None);
    }

    /// Checks discovery falls back when the primary catalog lacks the official version.
    #[test]
    fn remote_discovery_falls_back_after_a_catalog_without_the_official_version() {
        let mut source = FixtureSource::default();
        add_catalog(
            &mut source,
            PRIMARY_COMPAT_REPOSITORY,
            WINDOWS_TARGET,
            vec![("0.151.0", "rust-v0.151.0-native-join-p3", 3)],
        );
        add_catalog(
            &mut source,
            LEGACY_COMPAT_REPOSITORY,
            WINDOWS_TARGET,
            vec![("0.150.1", "rust-v0.150.1-native-join-p14", 14)],
        );
        let result = fetch_remote_candidates(
            &mut source,
            "0.150.1",
            WINDOWS_TARGET,
            Instant::now() + REMOTE_DIAGNOSTIC_TIMEOUT,
        )
        .unwrap()
        .unwrap();
        assert_eq!(result.0, LEGACY_COMPAT_REPOSITORY);
        assert_eq!(result.1[0].compat_id, "rust-v0.150.1-native-join-p14");
        assert_eq!(
            source.refs_requests,
            [PRIMARY_COMPAT_REPOSITORY, LEGACY_COMPAT_REPOSITORY]
        );
    }

    /// Checks no-match reports retain the highest candidate across repositories.
    #[test]
    fn remote_no_match_keeps_the_highest_catalog_across_repository_fallbacks() {
        let official = "0.160.0";
        let mut older_legacy = FixtureSource::default();
        add_catalog(
            &mut older_legacy,
            PRIMARY_COMPAT_REPOSITORY,
            WINDOWS_TARGET,
            vec![("0.152.0", "rust-v0.152.0-native-join-p12", 12)],
        );
        add_catalog(
            &mut older_legacy,
            LEGACY_COMPAT_REPOSITORY,
            WINDOWS_TARGET,
            vec![("0.149.0", "rust-v0.149.0-native-join-p99", 99)],
        );
        let primary_latest = fetch_remote_candidates(
            &mut older_legacy,
            official,
            WINDOWS_TARGET,
            Instant::now() + REMOTE_DIAGNOSTIC_TIMEOUT,
        )
        .unwrap()
        .unwrap();
        assert_eq!(primary_latest.0, PRIMARY_COMPAT_REPOSITORY);
        let report = remote_compatibility_report(&primary_latest.1, official, WINDOWS_TARGET, None);
        assert_eq!(report.status, "none_for_version");
        assert_eq!(
            report
                .latest_candidate
                .as_ref()
                .map(|candidate| candidate.compat_id.as_str()),
            Some("rust-v0.152.0-native-join-p12")
        );

        let mut empty_legacy_target = FixtureSource::default();
        add_catalog(
            &mut empty_legacy_target,
            PRIMARY_COMPAT_REPOSITORY,
            WINDOWS_TARGET,
            vec![("0.152.0", "rust-v0.152.0-native-join-p12", 12)],
        );
        add_catalog(
            &mut empty_legacy_target,
            LEGACY_COMPAT_REPOSITORY,
            "aarch64-pc-windows-msvc",
            vec![("0.149.0", "rust-v0.149.0-native-join-p99", 99)],
        );
        let primary_latest = fetch_remote_candidates(
            &mut empty_legacy_target,
            official,
            WINDOWS_TARGET,
            Instant::now() + REMOTE_DIAGNOSTIC_TIMEOUT,
        )
        .unwrap()
        .unwrap();
        assert_eq!(primary_latest.0, PRIMARY_COMPAT_REPOSITORY);
        let report = remote_compatibility_report(&primary_latest.1, official, WINDOWS_TARGET, None);
        assert_eq!(
            report
                .latest_candidate
                .as_ref()
                .map(|candidate| candidate.compat_id.as_str()),
            Some("rust-v0.152.0-native-join-p12")
        );
    }

    /// Checks legacy discovery can use the bundled bootstrap catalog.
    #[test]
    fn remote_legacy_discovery_uses_the_install_bootstrap_catalog() {
        let catalog: InstallCatalog = serde_json::from_str(INSTALL_CATALOG_BOOTSTRAP).unwrap();
        let refs = catalog
            .entries
            .iter()
            .map(|entry| {
                (
                    format!("refs/tags/{}", entry.release_tag),
                    entry.release_commit.clone(),
                )
            })
            .collect();
        let mut source = FixtureSource::default();
        source.refs.insert(LEGACY_COMPAT_REPOSITORY, Some(refs));
        let result = fetch_remote_candidates(
            &mut source,
            "0.150.1",
            WINDOWS_TARGET,
            Instant::now() + REMOTE_DIAGNOSTIC_TIMEOUT,
        )
        .unwrap()
        .unwrap();
        assert_eq!(result.0, LEGACY_COMPAT_REPOSITORY);
        assert!(result.1.iter().any(|candidate| {
            candidate.codex_version == "0.150.1" && candidate.build_target == WINDOWS_TARGET
        }));
        assert!(!source.catalog_requests.is_empty());
    }

    /// Checks timeouts and invalid catalog data become unreachable reports.
    #[test]
    fn remote_timeout_and_invalid_catalogs_fail_as_unreachable() {
        let mut slow_source = FixtureSource {
            delay: Some(Duration::from_millis(20)),
            ..FixtureSource::default()
        };
        let deadline = Instant::now() + Duration::from_millis(5);
        let report = diagnose_remote_with_deadline(
            "0.150.1",
            WINDOWS_TARGET,
            Some(&prepared_metadata(
                "rust-v0.150.1-native-join-p14",
                Some("0.150.1"),
            )),
            true,
            &mut slow_source,
            None,
            deadline,
        );
        assert_eq!(report.status, "unreachable");
        assert_eq!(
            report.prepared_compat_id.as_deref(),
            Some("rust-v0.150.1-native-join-p14")
        );

        let mut invalid_source = FixtureSource::default();
        let tag = "compat-rust-v0.150.1-native-join-p14";
        invalid_source.refs.insert(
            PRIMARY_COMPAT_REPOSITORY,
            Some(BTreeMap::from([(
                format!("refs/tags/{tag}"),
                "a".repeat(40),
            )])),
        );
        invalid_source.catalogs.insert(
            (PRIMARY_COMPAT_REPOSITORY, tag.to_owned()),
            Some(b"{invalid json".to_vec()),
        );
        let report = diagnose_remote_with_source(
            "0.150.1",
            WINDOWS_TARGET,
            None,
            true,
            &mut invalid_source,
            None,
        );
        assert_eq!(report.status, "unreachable");
    }

    /// Covers cache hits, expiry, refresh, replacement, and version-specific identity.
    #[test]
    fn remote_cache_is_private_validated_refreshable_and_replaced() {
        let directory = TestTempDir::new();
        let mut original = FixtureSource::default();
        add_catalog(
            &mut original,
            LEGACY_COMPAT_REPOSITORY,
            WINDOWS_TARGET,
            vec![
                ("0.150.1", "rust-v0.150.1-native-join-p14", 14),
                ("0.150.1", "rust-v0.150.1-native-join-p15", 15),
            ],
        );
        let report = diagnose_remote_with_source(
            "0.150.1",
            WINDOWS_TARGET,
            Some(&prepared_metadata(
                "rust-v0.150.1-native-join-p14",
                Some("0.150.1"),
            )),
            false,
            &mut original,
            Some(&directory.0),
        );
        assert_eq!(
            report.recommended_compat_id.as_deref(),
            Some("rust-v0.150.1-native-join-p15")
        );
        assert!(report.update_available);
        assert_eq!(report.source, Some("network"));
        assert!(report.checked_at_unix_seconds.is_some());
        let serialized = serde_json::to_value(&report).unwrap();
        assert_eq!(serialized["source"], "network");
        assert_eq!(
            serialized["checked_at_unix_seconds"].as_u64(),
            report.checked_at_unix_seconds
        );

        let mut cache_only = FixtureSource::default();
        let cached = diagnose_remote_with_source(
            "0.150.1",
            WINDOWS_TARGET,
            None,
            false,
            &mut cache_only,
            Some(&directory.0),
        );
        assert_eq!(
            cached.recommended_compat_id.as_deref(),
            Some("rust-v0.150.1-native-join-p15")
        );
        assert_eq!(cached.source, Some("cache"));
        assert!(cached.checked_at_unix_seconds.is_some());
        assert_eq!(serde_json::to_value(&cached).unwrap()["source"], "cache");
        assert!(cache_only.refs_requests.is_empty());

        assert_ne!(
            cache_key(WINDOWS_TARGET, "0.150.1"),
            cache_key(WINDOWS_TARGET, "0.151.0")
        );
        let mut changed_official_source = FixtureSource::default();
        add_catalog(
            &mut changed_official_source,
            PRIMARY_COMPAT_REPOSITORY,
            WINDOWS_TARGET,
            vec![("0.151.0", "rust-v0.151.0-native-join-p3", 3)],
        );
        let changed_official = diagnose_remote_with_source(
            "0.151.0",
            WINDOWS_TARGET,
            None,
            false,
            &mut changed_official_source,
            Some(&directory.0),
        );
        assert_eq!(changed_official.status, "match");
        assert_eq!(
            changed_official.recommended_compat_id.as_deref(),
            Some("rust-v0.151.0-native-join-p3")
        );
        assert!(!changed_official_source.refs_requests.is_empty());

        let cache_path = directory.0.join(format!(
            "csa-doctor-{}.json",
            cache_key(WINDOWS_TARGET, "0.150.1")
        ));
        let cache_modified = fs::metadata(&cache_path).unwrap().modified().unwrap();
        let mut expired_cache_source = FixtureSource::default();
        let expired = diagnose_remote_with_deadline_at(
            "0.150.1",
            WINDOWS_TARGET,
            None,
            false,
            &mut expired_cache_source,
            Some(&directory.0),
            RemoteDiagnosticTiming {
                deadline: Instant::now() + Duration::from_secs(1),
                now: cache_modified + REMOTE_CACHE_TTL + Duration::from_secs(1),
            },
        );
        assert_eq!(expired.status, "unreachable");
        assert!(!expired_cache_source.refs_requests.is_empty());

        let mut refreshed = FixtureSource::default();
        add_catalog(
            &mut refreshed,
            LEGACY_COMPAT_REPOSITORY,
            WINDOWS_TARGET,
            vec![("0.150.1", "rust-v0.150.1-native-join-p16", 16)],
        );
        let fresh = diagnose_remote_with_source(
            "0.150.1",
            WINDOWS_TARGET,
            None,
            true,
            &mut refreshed,
            Some(&directory.0),
        );
        assert_eq!(
            fresh.recommended_compat_id.as_deref(),
            Some("rust-v0.150.1-native-join-p16")
        );
        assert!(!refreshed.refs_requests.is_empty());

        let path = directory.0.join(format!(
            "csa-doctor-{}.json",
            cache_key(WINDOWS_TARGET, "0.150.1")
        ));
        assert_eq!(
            read_remote_cache(&path, WINDOWS_TARGET)
                .unwrap()
                .first()
                .unwrap()
                .compat_id,
            "rust-v0.150.1-native-join-p16"
        );
    }

    /// Checks malformed and oversized cache entries are ignored.
    #[test]
    fn malformed_or_oversized_remote_cache_is_ignored() {
        let directory = TestTempDir::new();
        assert!(cache_directory_is_private(&directory.0));
        assert!(!valid_cached_candidates(&[], WINDOWS_TARGET));
        let valid = candidate("0.150.1", "rust-v0.150.1-native-join-p14", WINDOWS_TARGET);
        assert!(valid_cached_candidates(
            std::slice::from_ref(&valid),
            WINDOWS_TARGET
        ));
        let mut invalid_date = valid.clone();
        invalid_date.recorded_on = "2025-02-29".to_owned();
        assert!(!valid_cached_candidates(
            std::slice::from_ref(&invalid_date),
            WINDOWS_TARGET
        ));
        assert!(!valid_cached_candidates(
            &[valid.clone(), valid],
            WINDOWS_TARGET
        ));
        let path = directory.0.join(format!(
            "csa-doctor-{}.json",
            cache_key(WINDOWS_TARGET, "0.150.1")
        ));
        let mut invalid = candidate("0.150.1", "rust-v0.150.1-native-join-p14", WINDOWS_TARGET);
        invalid.release_commit = "not-a-commit".to_owned();
        fs::write(&path, serde_json::to_vec(&[invalid]).unwrap()).unwrap();
        assert!(read_remote_cache(&path, WINDOWS_TARGET).is_none());
        let mut network_after_corrupt_cache = FixtureSource::default();
        let report = diagnose_remote_with_source(
            "0.150.1",
            WINDOWS_TARGET,
            None,
            false,
            &mut network_after_corrupt_cache,
            Some(&directory.0),
        );
        assert_eq!(report.status, "unreachable");
        assert!(!network_after_corrupt_cache.refs_requests.is_empty());

        fs::write(&path, vec![b' '; (MAX_REMOTE_CACHE_BYTES + 1) as usize]).unwrap();
        assert!(read_remote_cache(&path, WINDOWS_TARGET).is_none());
    }

    #[test]
    fn only_exact_formal_rust_release_tags_are_stable() {
        assert_eq!(stable_release_version("rust-v0.147.0").unwrap(), "0.147.0");
        for tag in ["rust-v0.148.0-rc.1", "rust-v0.148", "v0.148.0"] {
            assert!(stable_release_version(tag).is_err());
        }
    }

    #[test]
    fn compatibility_catalog_uses_the_greatest_numeric_patch_revision() {
        let entry = |compat_id: &str| InstallCandidate {
            repository: LEGACY_COMPAT_REPOSITORY.to_owned(),
            compat_id: compat_id.to_owned(),
            codex_version: "0.10.0".to_owned(),
            build_target: BUILD_TARGET.to_owned(),
            patch_revision: patch_revision(compat_id).unwrap(),
            recorded_on: "2026-08-29".to_owned(),
            recommended: false,
            release_tag: format!("compat-{compat_id}"),
            release_commit: "a".repeat(40),
        };
        let catalog = vec![
            entry("rust-v0.10.0-native-join-p9"),
            entry("rust-v0.10.0-native-join-p10"),
        ];
        assert_eq!(
            catalog[select_automatic(&catalog).unwrap()].compat_id,
            "rust-v0.10.0-native-join-p10"
        );
        assert_eq!(
            select_automatic(&[]).unwrap_err().code,
            "no_installable_compatibility_releases"
        );
        let tie = [
            entry("rust-v0.10.0-native-join-p10"),
            entry("rust-v0.10.0-orbit-p10"),
        ];
        assert_eq!(
            select_automatic(&tie).unwrap_err().code,
            "ambiguous_compatibility_revision"
        );
        for malformed in [
            "rust-v0.10.0-native-join",
            "rust-v0.10.0-native-join-p",
            "rust-v0.10.0-native-join-px",
        ] {
            assert!(patch_revision(malformed).is_err());
        }
    }

    #[test]
    fn ordered_authority_prefers_primary_and_falls_back_only_on_absence() {
        let mut visited = Vec::new();
        let selected = resolve_ordered_authority(|repository| {
            visited.push(repository);
            Ok(Some((repository, "duplicate-compat-id")))
        })
        .unwrap()
        .unwrap();
        assert_eq!(selected.0, PRIMARY_COMPAT_REPOSITORY);
        assert_eq!(visited, [PRIMARY_COMPAT_REPOSITORY]);

        visited.clear();
        let selected = resolve_ordered_authority(|repository| {
            visited.push(repository);
            Ok((repository == LEGACY_COMPAT_REPOSITORY).then_some(repository))
        })
        .unwrap()
        .unwrap();
        assert_eq!(selected, LEGACY_COMPAT_REPOSITORY);
        assert_eq!(
            visited,
            [PRIMARY_COMPAT_REPOSITORY, LEGACY_COMPAT_REPOSITORY]
        );

        visited.clear();
        let error = resolve_ordered_authority::<&str>(|repository| {
            visited.push(repository);
            Err(invalid_install_catalog("invalid primary catalog"))
        })
        .unwrap_err();
        assert_eq!(error.code, "invalid_install_catalog");
        assert_eq!(visited, [PRIMARY_COMPAT_REPOSITORY]);
    }

    #[test]
    fn install_catalog_is_strict_and_bound_to_git_refs() {
        let source_tag = "compat-rust-v0.10.0-native-join-p10";
        let source_commit = "a".repeat(40);
        let mut refs = BTreeMap::new();
        refs.insert(format!("refs/tags/{source_tag}"), source_commit.clone());
        let mut catalog = InstallCatalog {
            schema: 1,
            repository: "DSLZL/CSA".to_owned(),
            source_release_tag: source_tag.to_owned(),
            source_commit: source_commit.clone(),
            entries: vec![InstallCatalogEntry {
                compat_id: "rust-v0.10.0-native-join-p10".to_owned(),
                release_tag: source_tag.to_owned(),
                release_commit: source_commit,
                codex_version: "0.10.0".to_owned(),
                build_target: Some(BUILD_TARGET.to_owned()),
                build_targets: Vec::new(),
                patch_revision: 10,
                recorded_on: "2026-08-29".to_owned(),
            }],
        };
        validate_install_catalog(&catalog, LEGACY_COMPAT_REPOSITORY, &refs, Some(source_tag))
            .unwrap();
        assert!(
            validate_install_catalog(&catalog, PRIMARY_COMPAT_REPOSITORY, &refs, Some(source_tag),)
                .is_err()
        );
        catalog.entries[0].release_commit = "b".repeat(40);
        assert!(
            validate_install_catalog(&catalog, LEGACY_COMPAT_REPOSITORY, &refs, Some(source_tag),)
                .is_err()
        );
        assert!(valid_recorded_on("2024-02-29"));
        assert!(!valid_recorded_on("2025-02-29"));
        assert!(!valid_recorded_on("0000-01-01"));
    }

    #[test]
    fn schema_two_catalog_maps_linux_gnu_manager_to_musl_artifact() {
        let source_tag = "compat-rust-v0.10.0-native-join-p10";
        let source_commit = "a".repeat(40);
        let catalog: InstallCatalog = serde_json::from_value(serde_json::json!({
            "schema": 2,
            "repository": "DSLZL/CSA",
            "source_release_tag": source_tag,
            "source_commit": source_commit,
            "entries": [{
                "compat_id": "rust-v0.10.0-native-join-p10",
                "release_tag": source_tag,
                "release_commit": source_commit,
                "codex_version": "0.10.0",
                "build_targets": ["x86_64-unknown-linux-musl"],
                "patch_revision": 10,
                "recorded_on": "2026-08-29"
            }]
        }))
        .unwrap();
        let refs = BTreeMap::from([(format!("refs/tags/{source_tag}"), source_commit.to_owned())]);

        validate_install_catalog(&catalog, LEGACY_COMPAT_REPOSITORY, &refs, Some(source_tag))
            .unwrap();
        let candidates = install_candidates(
            catalog,
            "0.10.0",
            "x86_64-unknown-linux-gnu",
            LEGACY_COMPAT_REPOSITORY,
        );
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].build_target, "x86_64-unknown-linux-musl");
        assert_eq!(
            compatibility_artifact_target("aarch64-unknown-linux-gnu"),
            "aarch64-unknown-linux-musl"
        );
        assert_eq!(
            compatibility_artifact_target("x86_64-pc-windows-msvc"),
            "x86_64-pc-windows-msvc"
        );
    }

    #[test]
    fn bundled_install_catalog_is_valid_against_its_reviewed_refs() {
        let catalog: InstallCatalog =
            serde_json::from_str(super::INSTALL_CATALOG_BOOTSTRAP).unwrap();
        let mut refs = BTreeMap::new();
        for entry in &catalog.entries {
            refs.insert(
                format!("refs/tags/{}", entry.release_tag),
                entry.release_commit.clone(),
            );
        }
        validate_install_catalog(&catalog, LEGACY_COMPAT_REPOSITORY, &refs, None).unwrap();
    }

    #[test]
    fn display_catalog_filters_one_hundred_candidates_and_revalidates_selection() {
        let entries: Vec<_> = (1..=100)
            .rev()
            .map(|revision| {
                let compat_id = format!("rust-v0.10.0-native-join-p{revision}");
                InstallCatalogEntry {
                    release_tag: format!("compat-{compat_id}"),
                    release_commit: format!("{revision:040x}"),
                    compat_id,
                    codex_version: "0.10.0".to_owned(),
                    build_target: Some(compatibility_artifact_target(BUILD_TARGET).to_owned()),
                    build_targets: Vec::new(),
                    patch_revision: revision,
                    recorded_on: "2026-08-29".to_owned(),
                }
            })
            .collect();
        let source = &entries[0];
        let mut refs = BTreeMap::new();
        for entry in &entries {
            refs.insert(
                format!("refs/tags/{}", entry.release_tag),
                entry.release_commit.clone(),
            );
        }
        let catalog = InstallCatalog {
            schema: 1,
            repository: LEGACY_COMPAT_REPOSITORY.to_owned(),
            source_release_tag: source.release_tag.clone(),
            source_commit: source.release_commit.clone(),
            entries,
        };
        validate_install_catalog(&catalog, LEGACY_COMPAT_REPOSITORY, &refs, None).unwrap();
        let candidates =
            install_candidates(catalog, "0.10.0", BUILD_TARGET, LEGACY_COMPAT_REPOSITORY);
        assert_eq!(candidates.len(), 100);
        assert_eq!(
            candidates[select_automatic(&candidates).unwrap()].patch_revision,
            100
        );
        assert_eq!(
            take_selected_candidate(candidates.clone(), "rust-v0.10.0-native-join-p42")
                .unwrap()
                .patch_revision,
            42
        );
        assert_eq!(
            take_selected_candidate(candidates, "missing")
                .unwrap_err()
                .code,
            "invalid_install_selection"
        );
    }

    #[test]
    fn progress_reader_reports_cumulative_artifact_bytes() {
        let mut events = Vec::new();
        let copied = {
            let mut progress = |event| events.push(event);
            let mut reader = ProgressReader {
                inner: Cursor::new(b"patched"),
                downloaded_bytes: 0,
                total_bytes: 7,
                progress: &mut progress,
            };
            io::copy(&mut reader, &mut Vec::new()).unwrap()
        };
        assert_eq!(copied, 7);
        assert_eq!(
            events.last(),
            Some(&InstallEvent::ArtifactProgress {
                downloaded_bytes: 7,
                total_bytes: 7,
            })
        );
    }

    #[test]
    fn checksum_manifest_is_strict_and_complete_lines_only() {
        let digest = "a".repeat(64);
        let checksums = parse_checksums(format!("{digest}  asset.bin\n").as_bytes()).unwrap();
        assert_eq!(checksums["asset.bin"], digest);
        assert!(parse_checksums(format!("{digest} asset.bin\n").as_bytes()).is_err());
        assert!(parse_checksums(format!("{digest}  ../asset.bin\n").as_bytes()).is_err());
    }

    #[test]
    fn release_metadata_must_match_exactly() {
        let sha = "a".repeat(64);
        let commit = "b".repeat(40);
        let csa_commit = "c".repeat(40);
        let file = ReleaseFile {
            path: "manifest.toml".to_owned(),
            asset: "payload--manifest.toml".to_owned(),
            size: 3,
            sha256: sha.clone(),
        };
        let artifact = ReleaseFile {
            path: "codex.exe".to_owned(),
            asset: "payload--codex.exe".to_owned(),
            size: 3,
            sha256: sha.clone(),
        };
        let mut descriptor = CompatibilityRelease {
            schema: 1,
            repository: LEGACY_COMPAT_REPOSITORY.to_owned(),
            release_tag: "compat-rust-v1.2.3-native-join-p1".to_owned(),
            source_commit: csa_commit.clone(),
            compat_id: "rust-v1.2.3-native-join-p1".to_owned(),
            upstream: UpstreamRelease {
                repository: OPENAI_REPOSITORY.to_owned(),
                version: "1.2.3".to_owned(),
                tag: "rust-v1.2.3".to_owned(),
                commit: commit.clone(),
            },
            build_target: Some(BUILD_TARGET.to_owned()),
            payload: vec![file],
            artifact: Some(artifact),
            artifacts: BTreeMap::new(),
        };
        assert!(
            validate_catalog_descriptor(
                &descriptor,
                LEGACY_COMPAT_REPOSITORY,
                "compat-rust-v1.2.3-native-join-p1",
                &csa_commit,
                "rust-v1.2.3-native-join-p1",
            )
            .is_ok()
        );
        assert!(descriptor_artifact(&descriptor, BUILD_TARGET).is_ok());
        descriptor.repository = PRIMARY_COMPAT_REPOSITORY.to_owned();
        assert!(
            validate_catalog_descriptor(
                &descriptor,
                LEGACY_COMPAT_REPOSITORY,
                "compat-rust-v1.2.3-native-join-p1",
                &csa_commit,
                "rust-v1.2.3-native-join-p1",
            )
            .is_err()
        );
        descriptor.repository = LEGACY_COMPAT_REPOSITORY.to_owned();
        descriptor.upstream.tag = "rust-v9.9.9".to_owned();
        assert!(
            validate_catalog_descriptor(
                &descriptor,
                LEGACY_COMPAT_REPOSITORY,
                "compat-rust-v1.2.3-native-join-p1",
                &csa_commit,
                "rust-v1.2.3-native-join-p1",
            )
            .is_err()
        );

        assert!(descriptor_assets(&descriptor).is_ok());
        let mut checksums = BTreeMap::new();
        let artifact = descriptor.artifact.as_ref().unwrap();
        checksums.insert(artifact.asset.clone(), sha);
        assert!(validate_declared_asset(artifact, &checksums).is_ok());
        checksums.clear();
        assert!(validate_declared_asset(artifact, &checksums).is_err());

        let allowed = "https://release-assets.githubusercontent.com/asset"
            .parse()
            .unwrap();
        let rejected = "https://example.invalid/asset".parse().unwrap();
        assert!(require_uri_host(&allowed, &["release-assets.githubusercontent.com"]).is_ok());
        assert!(require_uri_host(&rejected, &["release-assets.githubusercontent.com"]).is_err());
    }

    #[test]
    fn proxy_samples_rank_fastest_and_preserve_fallbacks() {
        let active = [4, 1, 6, 0];
        assert_eq!(proxy_indices_from(&active, 6), [6, 0, 4, 1]);
        assert_eq!(
            rank_proxy_indices(
                &active,
                vec![
                    (1, Duration::from_millis(200)),
                    (6, Duration::from_millis(50)),
                    (99, Duration::from_millis(1)),
                ],
            ),
            [6, 1, 4, 0]
        );
        assert!(content_range_matches(
            "bytes 0-262143/1000000",
            262_144,
            1_000_000
        ));
        for invalid in [
            "bytes 1-262144/1000000",
            "bytes 0-262144/1000000",
            "bytes 0-262143/999999",
            "0-262143/1000000",
        ] {
            assert!(!content_range_matches(invalid, 262_144, 1_000_000));
        }

        let client = GitHubClient::with_route(LEGACY_COMPAT_REPOSITORY, GitHubRoute::Proxy(2));
        let mut progress: Option<&mut dyn FnMut(InstallEvent)> = None;
        assert!(client.switch_after_failed_download(GitHubRoute::Proxy(2), &mut progress));
        assert_eq!(client.route.get(), GitHubRoute::Proxy(3));
        assert!(!client.proxy_order.borrow().contains(&2));
    }

    /// Simulates a slow first proxy while a later proxy responds.
    fn delayed_first_proxy_with_healthy_second(
        index: usize,
        _repository: &'static str,
        deadline: Instant,
    ) -> bool {
        if index == 0 {
            std::thread::sleep(Duration::from_millis(50));
            return false;
        }
        index == 1 && remaining_deadline(deadline).is_ok()
    }

    /// Simulates a proxy pool where every route is unavailable.
    fn no_proxy_responds(_index: usize, _repository: &'static str, _deadline: Instant) -> bool {
        false
    }

    /// Selects a test proxy through the injected proxy probe.
    fn select_test_proxy_route(repository: &'static str, deadline: Instant) -> Result<usize> {
        select_proxy_index_until_with(
            repository,
            deadline,
            delayed_first_proxy_with_healthy_second,
        )
    }

    /// Fails the test if direct routing unexpectedly probes the proxy pool.
    fn unexpected_proxy_selection(_repository: &'static str, _deadline: Instant) -> Result<usize> {
        panic!("direct route must not select a proxy")
    }

    /// Checks remote initialization selects a healthy proxy before creating a client.
    #[test]
    fn remote_cn_route_selects_a_healthy_proxy_before_creating_the_client() {
        let deadline = Instant::now() + Duration::from_secs(2);
        let route = select_detected_remote_route(
            GitHubRoute::Proxy(0),
            PRIMARY_COMPAT_REPOSITORY,
            deadline,
            select_test_proxy_route,
        )
        .unwrap();
        assert_eq!(route, GitHubRoute::Proxy(1));

        let direct = select_detected_remote_route(
            GitHubRoute::Direct,
            PRIMARY_COMPAT_REPOSITORY,
            deadline,
            unexpected_proxy_selection,
        )
        .unwrap();
        assert_eq!(direct, GitHubRoute::Direct);
    }

    /// Checks proxy selection fails instead of defaulting to a dead first route.
    #[test]
    fn remote_proxy_selection_fails_when_no_route_responds() {
        let error = select_proxy_index_until_with(
            PRIMARY_COMPAT_REPOSITORY,
            Instant::now() + Duration::from_secs(2),
            no_proxy_responds,
        )
        .unwrap_err();
        assert_eq!(error.code, "network_error");
    }

    /// Checks region probes are bounded and missing results fall back to direct.
    #[test]
    fn remote_region_probes_use_a_short_deadline_and_fall_back_to_direct() {
        let now = Instant::now();
        let overall_deadline = now + Duration::from_secs(5);
        assert_eq!(
            remote_region_probe_deadline(now, overall_deadline),
            now + REMOTE_REGION_PROBE_TIMEOUT
        );

        let short_overall_deadline = now + Duration::from_millis(500);
        assert_eq!(
            remote_region_probe_deadline(now, short_overall_deadline),
            short_overall_deadline
        );
        assert_eq!(
            remote_route_from_region_probes([None, None]),
            GitHubRoute::Direct
        );
    }

    #[test]
    fn git_refs_drive_no_login_catalog_and_proxy_urls() {
        let tag = "compat-rust-v1.2.3-native-join-p1";
        let raw = "a".repeat(40);
        let commit = "b".repeat(40);
        let mut advertisement = packet("# service=git-upload-pack\n");
        advertisement.extend_from_slice(b"0000");
        advertisement.extend(packet("version 1\n"));
        advertisement.extend(packet(&format!("{raw} refs/tags/{tag}\0peeled\n")));
        advertisement.extend(packet(&format!("{commit} refs/tags/{tag}^{{}}\n")));
        advertisement.extend_from_slice(b"0000");

        let refs = parse_git_refs(&advertisement).unwrap();
        assert_eq!(compatibility_tags(&refs).unwrap(), [tag]);
        assert_eq!(peel_tag_from_refs(&refs, tag).unwrap(), commit);
        let direct = release_asset_url(LEGACY_COMPAT_REPOSITORY, tag, "SHA256SUMS");
        assert_eq!(routed_url(GitHubRoute::Direct, &direct), direct);
        assert_eq!(
            routed_url(GitHubRoute::Proxy(0), &direct),
            format!("https://gh-proxy.org/{direct}")
        );
        assert_eq!(
            routed_url(GitHubRoute::Proxy(6), &direct),
            format!("https://ghfast.top/{direct}")
        );
        assert_eq!(
            git_refs_url(PRIMARY_COMPAT_REPOSITORY),
            "https://github.com/DSLZL/CSA-codex.git/info/refs?service=git-upload-pack"
        );
        assert_eq!(
            release_asset_url(PRIMARY_COMPAT_REPOSITORY, tag, "SHA256SUMS"),
            format!("https://github.com/DSLZL/CSA-codex/releases/download/{tag}/SHA256SUMS")
        );
        assert_eq!(GH_PROXY_ROUTES.len(), 7);
        assert_eq!(
            GH_PROXY_ROUTES
                .iter()
                .map(|(_, host)| *host)
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            GH_PROXY_ROUTES.len()
        );
        assert_eq!(country_from_cloudflare_trace(b"loc=CN\n"), Some(true));
        assert_eq!(country_from_cloudflare_trace(b"loc=US\n"), Some(false));
        assert_eq!(country_from_cloudflare_trace(b"loc=cn\n"), None);
        assert_eq!(country_from_cloudflare_trace(b"loc=CN\nloc=US\n"), None);
        assert_eq!(country_from_cloudflare_trace(b"colo=HKG\n"), None);
        assert_eq!(
            country_from_alibaba_region(br#"{"code":0,"data":{"country_id":"CN"}}"#),
            Some(true)
        );
        assert_eq!(
            country_from_alibaba_region(br#"{"code":0,"data":{"country_id":"US"}}"#),
            Some(false)
        );
        assert_eq!(
            country_from_alibaba_region(br#"{"code":1,"data":null}"#),
            None
        );
        assert_eq!(
            route_from_region_probes([Some(false), Some(true)]),
            Some(GitHubRoute::Proxy(0))
        );
        assert_eq!(
            route_from_region_probes([Some(true), Some(false)]),
            Some(GitHubRoute::Proxy(0))
        );
        assert_eq!(
            route_from_region_probes([Some(false), None]),
            Some(GitHubRoute::Direct)
        );
        assert_eq!(route_from_region_probes([None, None]), None);
        assert!(parse_git_refs(b"0005x").is_err());
        assert!(should_try_proxy(&ureq::Error::StatusCode(403)));
        assert!(should_try_proxy(&ureq::Error::StatusCode(429)));
        assert!(should_try_proxy(&ureq::Error::StatusCode(503)));
        assert!(!should_try_proxy(&ureq::Error::StatusCode(404)));
        assert!(!should_try_proxy(&ureq::Error::Tls("certificate rejected")));

        let client = GitHubClient::with_route(LEGACY_COMPAT_REPOSITORY, GitHubRoute::Direct);
        let destination = std::env::temp_dir().join("artifact").join("codex.exe");
        let digest = "a".repeat(64);
        assert!(
            client
                .download_asset(
                    tag,
                    "payload--codex.exe",
                    &destination,
                    Some(MAX_ARTIFACT_BYTES + 1),
                    Some(&digest),
                    MAX_ARTIFACT_BYTES,
                )
                .is_err()
        );
    }

    fn packet(payload: &str) -> Vec<u8> {
        format!("{:04x}{payload}", payload.len() + 4).into_bytes()
    }

    /// Checks release bodies remain readable within the configured global timeout.
    #[test]
    fn github_client_allows_large_release_bodies_within_global_timeout() {
        let timeouts = GitHubClient::with_route(LEGACY_COMPAT_REPOSITORY, GitHubRoute::Direct)
            .agent
            .config()
            .timeouts();
        let install_timeout = Duration::from_secs(15 * 60);
        assert_eq!(timeouts.global, Some(install_timeout));
        assert_eq!(timeouts.connect, Some(Duration::from_secs(15)));
        assert_eq!(timeouts.recv_response, Some(Duration::from_secs(30)));
        assert_eq!(timeouts.recv_body, None);
    }
}
