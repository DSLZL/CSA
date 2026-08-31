use crate::BUILD_TARGET;
use crate::compat::{RuntimeArtifact, RuntimeManifest, validate_relative};
use crate::detect::{detect_official, parse_codex_version};
use crate::error::{ManagerError, Result};
use crate::hash::sha256_file;
use crate::manager::{InstallEvent, OnlineInstallOptions};
use crate::process::ProcessRunner;
use crate::state::{ManagerPaths, ensure_managed_directory, remove_managed_tree};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;
use ureq::{Agent, ResponseExt};

const OPENAI_REPOSITORY: &str = "openai/codex";
const CSA_REPOSITORY: &str = "dslzl/CSA";
const GH_PROXY_ROUTES: [(&str, &str); 7] = [
    ("https://gh-proxy.org/", "gh-proxy.org"),
    ("https://v4.gh-proxy.org/", "v4.gh-proxy.org"),
    ("https://v6.gh-proxy.org/", "v6.gh-proxy.org"),
    ("https://cdn.gh-proxy.org/", "cdn.gh-proxy.org"),
    ("https://axisnow.gh-proxy.org/", "axisnow.gh-proxy.org"),
    ("https://gh-proxy.com/", "gh-proxy.com"),
    ("https://ghfast.top/", "ghfast.top"),
];
const GH_PROXY_PROBE_URL: &str =
    "https://github.com/DSLZL/CSA.git/info/refs?service=git-upload-pack";
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
const MAX_ARTIFACT_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_COMPATIBILITY_TAGS: usize = 1_000;
const MAX_INSTALL_CATALOG_PROBES: usize = 16;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallCandidate {
    pub compat_id: String,
    pub codex_version: String,
    pub build_target: String,
    pub patch_revision: u64,
    pub recorded_on: String,
    pub recommended: bool,
    pub release_tag: String,
    pub release_commit: String,
}

pub type InstallSelector<'a> = dyn FnMut(&[InstallCandidate]) -> Result<String> + 'a;

struct SelectedCompatibility {
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
    let client = GitHubClient::new();
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

    let refs = client.repository_refs(CSA_REPOSITORY)?;
    let selected = if let Some(requested) = options.compat.as_deref() {
        validate_asset_name(requested)?;
        let release_tag = format!("compat-{requested}");
        let release_commit = peel_tag_from_refs(&refs, &release_tag).map_err(|_| {
            ManagerError::new(
                "compatibility_not_found",
                format!("no formal compatibility release has ID {requested}"),
            )
        })?;
        SelectedCompatibility {
            compat_id: requested.to_owned(),
            release_tag,
            release_commit,
            catalog_entry: None,
        }
    } else {
        let mut candidates = discover_install_candidates(
            &client,
            &paths.root,
            &staging_path,
            &refs,
            &official.version,
        )?;
        let recommended = select_automatic(&candidates)?;
        candidates[recommended].recommended = true;
        let selected_id = if let Some(select) = selector {
            progress(InstallEvent::SelectingCompatibility);
            select(&candidates)?
        } else {
            candidates[recommended].compat_id.clone()
        };
        let candidate = take_selected_candidate(candidates, &selected_id)?;
        SelectedCompatibility {
            compat_id: candidate.compat_id.clone(),
            release_tag: candidate.release_tag.clone(),
            release_commit: candidate.release_commit.clone(),
            catalog_entry: Some(candidate),
        }
    };
    let compat_id = &selected.compat_id;
    progress(InstallEvent::SelectedCompatibility {
        compat_id: compat_id.clone(),
    });
    let release_tag = selected.release_tag;
    let csa_commit = selected.release_commit;
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
    validate_catalog_descriptor(&descriptor, &release_tag, &csa_commit, compat_id)?;
    if selected.catalog_entry.is_some_and(|expected| {
        descriptor.upstream.version != expected.codex_version
            || !descriptor_supports_target(&descriptor, &expected.build_target)
    }) {
        return Err(ManagerError::new(
            "compatibility_release_changed",
            "selected compatibility metadata changed during installation",
        ));
    }

    let upstream_version = stable_release_version(&descriptor.upstream.tag)?;
    descriptor_artifact(&descriptor, BUILD_TARGET)?;
    if official.version != upstream_version {
        return Err(ManagerError::new(
            "unsupported_official_version",
            format!(
                "selected compatibility requires Codex {upstream_version}, official Codex is {}",
                official.version
            ),
        ));
    }

    let release_artifact = descriptor_artifact(&descriptor, BUILD_TARGET)?;
    validate_declared_asset(release_artifact, &checksums)?;
    let runtime = RuntimeManifest::new(
        compat_id.clone(),
        upstream_version,
        BUILD_TARGET.to_owned(),
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

    Ok(OnlineBundle {
        manager_root: options.manager_root.clone(),
        official: official.executable.path,
        official_native: official.native.map(|native| native.path),
        runtime,
        artifact: artifact_path,
        _staging: staging,
    })
}

fn discover_install_candidates(
    client: &GitHubClient,
    manager_root: &Path,
    staging_path: &Path,
    refs: &BTreeMap<String, String>,
    official_version: &str,
) -> Result<Vec<InstallCandidate>> {
    let catalog_path = staging_path.join("catalog");
    ensure_managed_directory(manager_root, &catalog_path)?;
    let catalog = load_install_catalog(client, &catalog_path, refs)?;
    install_candidates(catalog, official_version)
}

fn install_candidates(
    catalog: InstallCatalog,
    official_version: &str,
) -> Result<Vec<InstallCandidate>> {
    let candidates: Vec<_> = catalog
        .entries
        .into_iter()
        .filter(|entry| {
            entry.codex_version == official_version && entry.supports_target(BUILD_TARGET)
        })
        .map(|entry| InstallCandidate {
            compat_id: entry.compat_id,
            codex_version: entry.codex_version,
            build_target: BUILD_TARGET.to_owned(),
            patch_revision: entry.patch_revision,
            recorded_on: entry.recorded_on,
            recommended: false,
            release_tag: entry.release_tag,
            release_commit: entry.release_commit,
        })
        .collect();
    if candidates.is_empty() {
        return Err(ManagerError::new(
            "no_installable_compatibility_releases",
            "no formal compatibility release matches this Manager target and official Codex version",
        ));
    }
    Ok(candidates)
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
) -> Result<InstallCatalog> {
    // ponytail: probe only the newest 16 tags; switch to one stable catalog URL if release history outgrows it.
    for (index, release_tag) in compatibility_tags(refs)?
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
            validate_install_catalog(&catalog, refs, Some(&release_tag))?;
            return Ok(catalog);
        }
    }
    let catalog: InstallCatalog =
        serde_json::from_str(INSTALL_CATALOG_BOOTSTRAP).map_err(|error| {
            ManagerError::new(
                "invalid_install_catalog",
                format!("invalid bundled install catalog: {error}"),
            )
        })?;
    validate_install_catalog(&catalog, refs, None)?;
    Ok(catalog)
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
    refs: &BTreeMap<String, String>,
    containing_release_tag: Option<&str>,
) -> Result<()> {
    if !matches!(catalog.schema, 1 | 2)
        || !catalog.repository.eq_ignore_ascii_case(CSA_REPOSITORY)
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
    agent: Agent,
    route: Cell<GitHubRoute>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GitHubRoute {
    Direct,
    Proxy(usize),
}

impl GitHubClient {
    fn new() -> Self {
        let route = match detect_github_route().unwrap_or(GitHubRoute::Direct) {
            GitHubRoute::Direct => GitHubRoute::Direct,
            GitHubRoute::Proxy(_) => GitHubRoute::Proxy(select_proxy_index()),
        };
        Self::with_route(route)
    }

    fn with_route(route: GitHubRoute) -> Self {
        let config = Agent::config_builder()
            .https_only(true)
            .max_redirects(5)
            .timeout_global(Some(Duration::from_secs(15 * 60)))
            .timeout_connect(Some(Duration::from_secs(15)))
            .timeout_recv_response(Some(Duration::from_secs(30)))
            .build();
        Self {
            agent: Agent::new_with_config(config),
            route: Cell::new(route),
        }
    }

    fn repository_refs(&self, repository: &str) -> Result<BTreeMap<String, String>> {
        let url = format!("https://github.com/{repository}.git/info/refs?service=git-upload-pack");
        let mut response = self
            .get_response(
                &url,
                "application/x-git-upload-pack-advertisement",
                true,
                &["github.com"],
                "query GitHub repository refs",
                false,
            )?
            .ok_or_else(|| {
                ManagerError::new(
                    "github_repository_not_found",
                    format!("GitHub repository not found: {repository}"),
                )
            })?;
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
        parse_git_refs(&bytes)
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
        let url = release_asset_url(release_tag, asset_name);
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
        if response.body().content_length().is_some_and(|length| {
            length == 0 || length > max_size || expected_size.is_some_and(|size| size != length)
        }) {
            return Err(ManagerError::new(
                "release_asset_size_mismatch",
                format!("Content-Length differs for release asset: {asset_name}"),
            ));
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
                return Err(ManagerError::io("stream release asset", error));
            }
        };
        if let Err(error) = output.sync_all() {
            drop(output);
            let _ = fs::remove_file(destination);
            return Err(ManagerError::io("sync release asset", error));
        }
        drop(output);
        if copied == 0 || copied > max_size || expected_size.is_some_and(|size| copied != size) {
            let _ = fs::remove_file(destination);
            return Err(ManagerError::new(
                "release_asset_size_mismatch",
                format!("release asset {asset_name} has an unexpected size: {copied} bytes"),
            ));
        }
        if let Some(progress) = progress {
            progress(InstallEvent::VerifyingArtifact);
        }
        let (actual, size) = sha256_file(destination)?;
        if size != copied || expected_sha256.is_some_and(|expected| actual != expected) {
            let _ = fs::remove_file(destination);
            return Err(ManagerError::new(
                "release_asset_hash_mismatch",
                format!("release asset failed SHA-256 verification: {asset_name}"),
            ));
        }
        Ok(true)
    }

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
            Err(ureq::Error::StatusCode(404)) => Ok(None),
            Err(error) if should_try_proxy(&error) => {
                let direct_error = error.to_string();
                let index = select_proxy_index();
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
            Err(error) => Err(network_error(context, error)),
        }
    }

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
        for offset in 0..GH_PROXY_ROUTES.len() {
            let index = (first + offset) % GH_PROXY_ROUTES.len();
            let route = GitHubRoute::Proxy(index);
            match self.request_on_route(route, direct_url, accept, git_protocol) {
                Ok(response) => {
                    require_route_host(&response, route, &[])?;
                    self.route.set(route);
                    return Ok(Some(response));
                }
                Err(ureq::Error::StatusCode(404)) if !retry_404 => {
                    self.route.set(route);
                    return Ok(None);
                }
                Err(ureq::Error::StatusCode(404)) => {
                    not_found += 1;
                    failures.push(format!("{}: 404", GH_PROXY_ROUTES[index].1));
                }
                Err(error) => failures.push(format!("{}: {error}", GH_PROXY_ROUTES[index].1)),
            }
        }
        if not_found == GH_PROXY_ROUTES.len() {
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

    fn request_on_route(
        &self,
        route: GitHubRoute,
        direct_url: &str,
        accept: &str,
        git_protocol: bool,
    ) -> std::result::Result<ureq::http::Response<ureq::Body>, ureq::Error> {
        let url = routed_url(route, direct_url);
        let mut request = self
            .agent
            .get(&url)
            .header("Accept", accept)
            .header("User-Agent", concat!("csa/", env!("CARGO_PKG_VERSION")));
        if git_protocol {
            request = request.header("Git-Protocol", "version=1");
        }
        request.call()
    }
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

fn detect_cloudflare_country() -> Option<bool> {
    let bytes = read_region_response(CLOUDFLARE_TRACE_URL, "text/plain", &["www.cloudflare.com"])?;
    country_from_cloudflare_trace(&bytes)
}

fn detect_alibaba_country() -> Option<bool> {
    let bytes = read_region_response(ALIBABA_REGION_URL, "application/json", &["ip.taobao.com"])?;
    country_from_alibaba_region(&bytes)
}

fn read_region_response(url: &str, accept: &str, allowed_hosts: &[&str]) -> Option<Vec<u8>> {
    let config = Agent::config_builder()
        .https_only(true)
        .max_redirects(0)
        .timeout_global(Some(Duration::from_secs(5)))
        .timeout_connect(Some(Duration::from_secs(3)))
        .timeout_recv_response(Some(Duration::from_secs(3)))
        .timeout_recv_body(Some(Duration::from_secs(3)))
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

fn routed_url(route: GitHubRoute, direct_url: &str) -> String {
    match route {
        GitHubRoute::Direct => direct_url.to_owned(),
        GitHubRoute::Proxy(index) => format!("{}{direct_url}", GH_PROXY_ROUTES[index].0),
    }
}

fn select_proxy_index() -> usize {
    let (sender, receiver) = mpsc::channel();
    for index in 0..GH_PROXY_ROUTES.len() {
        let sender = sender.clone();
        std::thread::spawn(move || {
            if proxy_responds(index) {
                let _ = sender.send(index);
            }
        });
    }
    drop(sender);
    receiver.recv_timeout(Duration::from_secs(4)).unwrap_or(0)
}

fn proxy_responds(index: usize) -> bool {
    let config = Agent::config_builder()
        .https_only(true)
        .max_redirects(2)
        .timeout_global(Some(Duration::from_secs(4)))
        .timeout_connect(Some(Duration::from_secs(3)))
        .timeout_recv_response(Some(Duration::from_secs(3)))
        .build();
    let agent = Agent::new_with_config(config);
    let response = agent
        .get(&routed_url(GitHubRoute::Proxy(index), GH_PROXY_PROBE_URL))
        .header("Accept", "application/x-git-upload-pack-advertisement")
        .header("Git-Protocol", "version=1")
        .header("User-Agent", concat!("csa/", env!("CARGO_PKG_VERSION")))
        .call();
    response
        .as_ref()
        .is_ok_and(|response| require_route_host(response, GitHubRoute::Proxy(index), &[]).is_ok())
}

fn release_asset_url(release_tag: &str, asset_name: &str) -> String {
    format!("https://github.com/{CSA_REPOSITORY}/releases/download/{release_tag}/{asset_name}")
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
    release_tag: &str,
    csa_commit: &str,
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
    validate_sha(csa_commit)?;
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
    if descriptor.repository != CSA_REPOSITORY
        || descriptor.release_tag != release_tag
        || descriptor.source_commit != csa_commit
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

fn network_error(context: &str, error: impl std::fmt::Display) -> ManagerError {
    ManagerError::new("network_error", format!("{context}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::{
        BUILD_TARGET, CSA_REPOSITORY, CompatibilityRelease, GH_PROXY_PROBE_URL, GH_PROXY_ROUTES,
        GitHubClient, GitHubRoute, InstallCandidate, InstallCatalog, InstallCatalogEntry,
        MAX_ARTIFACT_BYTES, OPENAI_REPOSITORY, ProgressReader, ReleaseFile, UpstreamRelease,
        compatibility_tags, country_from_alibaba_region, country_from_cloudflare_trace,
        descriptor_artifact, descriptor_assets, install_candidates, parse_checksums,
        parse_git_refs, patch_revision, peel_tag_from_refs, release_asset_url, require_uri_host,
        route_from_region_probes, routed_url, select_automatic, should_try_proxy,
        stable_release_version, take_selected_candidate, valid_recorded_on,
        validate_catalog_descriptor, validate_declared_asset, validate_install_catalog,
    };
    use crate::manager::InstallEvent;
    use std::collections::BTreeMap;
    use std::io::{self, Cursor};
    use std::time::Duration;

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
    fn install_catalog_is_strict_and_bound_to_git_refs() {
        let source_tag = "compat-rust-v0.10.0-native-join-p10";
        let source_commit = "a".repeat(40);
        let mut refs = BTreeMap::new();
        refs.insert(format!("refs/tags/{source_tag}"), source_commit.clone());
        let catalog = InstallCatalog {
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
        validate_install_catalog(&catalog, &refs, Some(source_tag)).unwrap();
        assert!(valid_recorded_on("2024-02-29"));
        assert!(!valid_recorded_on("2025-02-29"));
        assert!(!valid_recorded_on("0000-01-01"));
    }

    #[test]
    fn schema_two_install_catalog_deserializes_and_selects_current_target() {
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
                "build_targets": [BUILD_TARGET],
                "patch_revision": 10,
                "recorded_on": "2026-08-29"
            }]
        }))
        .unwrap();
        let refs = BTreeMap::from([(format!("refs/tags/{source_tag}"), source_commit.to_owned())]);

        validate_install_catalog(&catalog, &refs, Some(source_tag)).unwrap();
        let candidates = install_candidates(catalog, "0.10.0").unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].build_target, BUILD_TARGET);
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
        validate_install_catalog(&catalog, &refs, None).unwrap();
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
                    build_target: Some(BUILD_TARGET.to_owned()),
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
            repository: CSA_REPOSITORY.to_owned(),
            source_release_tag: source.release_tag.clone(),
            source_commit: source.release_commit.clone(),
            entries,
        };
        validate_install_catalog(&catalog, &refs, None).unwrap();
        let candidates = install_candidates(catalog, "0.10.0").unwrap();
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
            repository: CSA_REPOSITORY.to_owned(),
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
                "compat-rust-v1.2.3-native-join-p1",
                &csa_commit,
                "rust-v1.2.3-native-join-p1",
            )
            .is_ok()
        );
        assert!(descriptor_artifact(&descriptor, BUILD_TARGET).is_ok());
        descriptor.upstream.tag = "rust-v9.9.9".to_owned();
        assert!(
            validate_catalog_descriptor(
                &descriptor,
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
        let direct = release_asset_url(tag, "SHA256SUMS");
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
            GH_PROXY_PROBE_URL,
            "https://github.com/DSLZL/CSA.git/info/refs?service=git-upload-pack"
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

        let client = GitHubClient::with_route(GitHubRoute::Direct);
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

    #[test]
    fn github_client_allows_large_release_bodies_within_global_timeout() {
        let timeouts = GitHubClient::with_route(GitHubRoute::Direct)
            .agent
            .config()
            .timeouts();
        assert_eq!(timeouts.global, Some(Duration::from_secs(15 * 60)));
        assert_eq!(timeouts.connect, Some(Duration::from_secs(15)));
        assert_eq!(timeouts.recv_response, Some(Duration::from_secs(30)));
        assert_eq!(timeouts.recv_body, None);
    }
}
