use crate::BUILD_TARGET;
use crate::compat::{LoadedCompatibility, validate_relative};
use crate::detect::{detect_official, parse_codex_version};
use crate::error::{ManagerError, Result};
use crate::hash::sha256_file;
use crate::manager::{OnlineInstallOptions, PrepareOptions};
use crate::process::ProcessRunner;
use crate::state::{ManagerPaths, ensure_managed_directory, remove_managed_tree};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;
use ureq::{Agent, ResponseExt};

const OPENAI_REPOSITORY: &str = "openai/codex";
const CSA_REPOSITORY: &str = "dslzl/CSA";
const API_ROOT: &str = "https://api.github.com/repos";
const RELEASE_DESCRIPTOR: &str = "compatibility-release.json";
const RELEASE_CHECKSUMS: &str = "SHA256SUMS";
const MAX_JSON_BYTES: u64 = 4 * 1024 * 1024;
const MAX_RELEASE_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_ARTIFACT_BYTES: u64 = 1024 * 1024 * 1024;
const RELEASES_PER_PAGE: usize = 100;
const MAX_RELEASE_PAGES: usize = 10;

struct CompatibilityEntry {
    compat_id: String,
    codex_version: String,
    build_target: String,
    version_key: (u64, u64, u64),
    unavailable: Option<String>,
}

pub struct OnlineBundle {
    prepare: PrepareOptions,
    _staging: StagingGuard,
}

impl OnlineBundle {
    pub fn prepare_options(&self) -> PrepareOptions {
        self.prepare.clone()
    }
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
    require_selection_mode(
        options.compat.as_deref(),
        io::stdin().is_terminal(),
        io::stderr().is_terminal(),
    )?;
    let paths = ManagerPaths::resolve(options.manager_root.clone())?;
    let official = detect_official(
        runner,
        options.official.as_deref(),
        options.official_native.as_deref(),
        std::slice::from_ref(&paths.root),
    )?;
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

    let catalog = discover_catalog(&client, &paths.root, &staging_path, &official.version)?;
    if catalog.is_empty() {
        return Err(ManagerError::new(
            "no_compatibility_releases",
            "CSA has no formal compatibility releases",
        ));
    }
    let selected_index = if let Some(requested) = options.compat.as_deref() {
        select_requested(&catalog, requested)?
    } else {
        let mut input = io::stdin().lock();
        let mut output = io::stderr().lock();
        prompt_catalog(&catalog, &mut input, &mut output)?
    };
    let selected = &catalog[selected_index];
    let compat_id = &selected.compat_id;
    let release_tag = format!("compat-{compat_id}");
    let release_url = format!("{API_ROOT}/{CSA_REPOSITORY}/releases/tags/{release_tag}");
    let compatibility_release: GitHubRelease =
        client.get_json_optional(&release_url)?.ok_or_else(|| {
            ManagerError::new(
                "compatibility_release_missing",
                format!("selected compatibility release disappeared: {release_tag}"),
            )
        })?;
    require_formal_compatibility_release(&compatibility_release, &release_tag)?;
    let csa_commit = client.peel_tag(CSA_REPOSITORY, &release_tag)?;
    let selected_path = staging_path.join("selected");
    ensure_managed_directory(&paths.root, &selected_path)?;
    let (descriptor, checksums) = download_release_metadata(
        &client,
        &release_tag,
        &compatibility_release,
        &selected_path,
    )?;
    validate_catalog_descriptor(&descriptor, &release_tag, &csa_commit, compat_id)?;
    if descriptor.upstream.version != selected.codex_version
        || descriptor.build_target != selected.build_target
    {
        return Err(ManagerError::new(
            "compatibility_release_changed",
            "selected compatibility metadata changed during installation",
        ));
    }

    let upstream_url = format!(
        "{API_ROOT}/{OPENAI_REPOSITORY}/releases/tags/{}",
        descriptor.upstream.tag
    );
    let upstream_release: GitHubRelease = client.get_json(&upstream_url)?;
    let upstream_version = stable_release_version(&upstream_release)?;
    let upstream_commit = client.peel_tag(OPENAI_REPOSITORY, &upstream_release.tag_name)?;
    validate_descriptor(
        &descriptor,
        &release_tag,
        &csa_commit,
        compat_id,
        &upstream_version,
        &upstream_release.tag_name,
        &upstream_commit,
    )?;
    if official.version != upstream_version {
        return Err(ManagerError::new(
            "unsupported_official_version",
            format!(
                "selected compatibility requires Codex {upstream_version}, official Codex is {}",
                official.version
            ),
        ));
    }

    let assets = release_assets(&compatibility_release)?;

    let payload_root = selected_path.join(compat_id);
    ensure_managed_directory(&paths.root, &payload_root)?;
    for file in &descriptor.payload {
        let asset = validate_declared_asset(file, &assets, &checksums)?;
        let destination = payload_root.join(&file.path);
        let parent = destination.parent().ok_or_else(|| {
            ManagerError::new(
                "invalid_compatibility_release",
                "payload path has no parent",
            )
        })?;
        ensure_managed_directory(&paths.root, parent)?;
        client.download_asset(&release_tag, asset, &destination, Some(&file.sha256))?;
    }
    let artifact = validate_declared_asset(&descriptor.artifact, &assets, &checksums)?;
    let artifact_path = selected_path
        .join("artifact")
        .join(&descriptor.artifact.path);
    ensure_managed_directory(
        &paths.root,
        artifact_path.parent().expect("artifact path has a parent"),
    )?;
    client.download_asset(
        &release_tag,
        artifact,
        &artifact_path,
        Some(&descriptor.artifact.sha256),
    )?;

    let manifest_path = payload_root.join("manifest.toml");
    let compatibility = LoadedCompatibility::load(&manifest_path)?;
    compatibility.test_contract()?;
    validate_downloaded_compatibility(&compatibility, &descriptor, &upstream_commit)?;
    Ok(OnlineBundle {
        prepare: PrepareOptions {
            manager_root: options.manager_root.clone(),
            official: Some(official.executable.path),
            official_native: official.native.map(|native| native.path),
            manifest: manifest_path,
            artifact: Some(artifact_path),
            source: None,
        },
        _staging: staging,
    })
}

fn discover_catalog(
    client: &GitHubClient,
    manager_root: &Path,
    staging_path: &Path,
    official_version: &str,
) -> Result<Vec<CompatibilityEntry>> {
    let catalog_path = staging_path.join("catalog");
    ensure_managed_directory(manager_root, &catalog_path)?;
    let mut entries = Vec::new();
    let mut compat_ids = BTreeSet::new();
    for page in 1..=MAX_RELEASE_PAGES {
        let releases: Vec<GitHubRelease> = client.get_json(&format!(
            "{API_ROOT}/{CSA_REPOSITORY}/releases?per_page={RELEASES_PER_PAGE}&page={page}"
        ))?;
        let page_len = releases.len();
        for release in releases {
            if !release.tag_name.starts_with("compat-") || release.draft || release.prerelease {
                continue;
            }
            let compat_id = release.tag_name.strip_prefix("compat-").unwrap();
            validate_asset_name(compat_id)?;
            if !compat_ids.insert(compat_id.to_owned()) {
                return Err(ManagerError::new(
                    "invalid_compatibility_release",
                    format!("duplicate compatibility release: {compat_id}"),
                ));
            }
            let csa_commit = client.peel_tag(CSA_REPOSITORY, &release.tag_name)?;
            let entry_path = catalog_path.join(entries.len().to_string());
            ensure_managed_directory(manager_root, &entry_path)?;
            let (descriptor, _) =
                download_release_metadata(client, &release.tag_name, &release, &entry_path)?;
            validate_catalog_descriptor(&descriptor, &release.tag_name, &csa_commit, compat_id)?;
            entries.push(CompatibilityEntry {
                compat_id: descriptor.compat_id,
                codex_version: descriptor.upstream.version.clone(),
                build_target: descriptor.build_target.clone(),
                version_key: version_key(&descriptor.upstream.version)?,
                unavailable: incompatibility_reason(
                    &descriptor.upstream.version,
                    &descriptor.build_target,
                    official_version,
                ),
            });
        }
        if page_len < RELEASES_PER_PAGE {
            break;
        }
        // ponytail: 1,000 releases bounds unauthenticated API work; paginate further only if the real catalog reaches it.
        if page == MAX_RELEASE_PAGES {
            return Err(ManagerError::new(
                "compatibility_catalog_too_large",
                "compatibility catalog reached the 1,000-release safety limit",
            ));
        }
    }
    sort_catalog(&mut entries);
    Ok(entries)
}

fn sort_catalog(entries: &mut [CompatibilityEntry]) {
    entries.sort_by(|left, right| {
        right
            .version_key
            .cmp(&left.version_key)
            .then_with(|| left.compat_id.cmp(&right.compat_id))
    });
}

fn download_release_metadata(
    client: &GitHubClient,
    release_tag: &str,
    release: &GitHubRelease,
    destination: &Path,
) -> Result<(CompatibilityRelease, BTreeMap<String, String>)> {
    let assets = release_assets(release)?;
    let checksums_asset = assets.get(RELEASE_CHECKSUMS).ok_or_else(|| {
        ManagerError::new(
            "invalid_compatibility_release",
            "compatibility release is missing SHA256SUMS",
        )
    })?;
    let checksums_path = destination.join(RELEASE_CHECKSUMS);
    client.download_asset(release_tag, checksums_asset, &checksums_path, None)?;
    let checksums =
        parse_checksums(&fs::read(&checksums_path).map_err(|error| {
            ManagerError::io("read downloaded compatibility checksums", error)
        })?)?;
    let release_asset_names: BTreeSet<_> = assets
        .keys()
        .filter(|name| name.as_str() != RELEASE_CHECKSUMS)
        .cloned()
        .collect();
    if checksums.keys().cloned().collect::<BTreeSet<_>>() != release_asset_names {
        return Err(ManagerError::new(
            "invalid_compatibility_release",
            "SHA256SUMS must cover every release asset except itself",
        ));
    }
    let descriptor_asset = assets.get(RELEASE_DESCRIPTOR).ok_or_else(|| {
        ManagerError::new(
            "invalid_compatibility_release",
            "compatibility release is missing its provenance descriptor",
        )
    })?;
    let descriptor_path = destination.join(RELEASE_DESCRIPTOR);
    client.download_asset(
        release_tag,
        descriptor_asset,
        &descriptor_path,
        checksums.get(RELEASE_DESCRIPTOR).map(String::as_str),
    )?;
    let descriptor: CompatibilityRelease = read_json_file(&descriptor_path)?;
    let declared_assets = descriptor_assets(&descriptor)?;
    let expected_assets: BTreeSet<_> = declared_assets
        .keys()
        .cloned()
        .chain([RELEASE_DESCRIPTOR.to_owned(), RELEASE_CHECKSUMS.to_owned()])
        .collect();
    if assets.keys().cloned().collect::<BTreeSet<_>>() != expected_assets {
        return Err(ManagerError::new(
            "invalid_compatibility_release",
            "release assets differ from the reviewed compatibility descriptor",
        ));
    }
    for file in declared_assets.values() {
        validate_declared_asset(file, &assets, &checksums)?;
    }
    Ok((descriptor, checksums))
}

fn require_formal_compatibility_release(release: &GitHubRelease, release_tag: &str) -> Result<()> {
    if release.draft || release.prerelease || release.tag_name != release_tag {
        return Err(ManagerError::new(
            "invalid_compatibility_release",
            "compatibility release must be formal and use the exact compatibility tag",
        ));
    }
    Ok(())
}

fn require_selection_mode(
    requested: Option<&str>,
    stdin_terminal: bool,
    stderr_terminal: bool,
) -> Result<()> {
    if requested.is_none() && !(stdin_terminal && stderr_terminal) {
        return Err(ManagerError::new(
            "interactive_selection_required",
            "non-interactive install requires --compat <compat-id>",
        ));
    }
    Ok(())
}

fn select_requested(catalog: &[CompatibilityEntry], requested: &str) -> Result<usize> {
    let index = catalog
        .iter()
        .position(|entry| entry.compat_id == requested)
        .ok_or_else(|| {
            ManagerError::new(
                "compatibility_not_found",
                format!("no formal compatibility release has ID {requested}"),
            )
        })?;
    if let Some(reason) = &catalog[index].unavailable {
        return Err(ManagerError::new(
            "compatibility_not_installable",
            format!("{requested} is not installable: {reason}"),
        ));
    }
    Ok(index)
}

fn prompt_catalog(
    catalog: &[CompatibilityEntry],
    input: &mut dyn BufRead,
    output: &mut dyn Write,
) -> Result<usize> {
    writeln!(output, "Available patched Codex CLI releases:")
        .map_err(|error| ManagerError::io("write compatibility catalog", error))?;
    for (index, entry) in catalog.iter().enumerate() {
        writeln!(
            output,
            "  {}) Codex {}  {}  {}  {}",
            index + 1,
            entry.codex_version,
            entry.compat_id,
            entry.build_target,
            entry.unavailable.as_deref().unwrap_or("installable")
        )
        .map_err(|error| ManagerError::io("write compatibility catalog", error))?;
    }
    if catalog.iter().all(|entry| entry.unavailable.is_some()) {
        return Err(ManagerError::new(
            "no_installable_compatibility_releases",
            "no formal compatibility release matches this Manager target and official Codex version",
        ));
    }
    loop {
        write!(output, "Select an installable release number: ")
            .and_then(|()| output.flush())
            .map_err(|error| ManagerError::io("write compatibility prompt", error))?;
        let mut line = String::new();
        if input
            .read_line(&mut line)
            .map_err(|error| ManagerError::io("read compatibility selection", error))?
            == 0
        {
            return Err(ManagerError::new(
                "compatibility_selection_aborted",
                "compatibility selection ended before a choice was made",
            ));
        }
        let Ok(choice) = line.trim().parse::<usize>() else {
            writeln!(output, "Enter one of the displayed numbers.")
                .map_err(|error| ManagerError::io("write compatibility prompt", error))?;
            continue;
        };
        if choice == 0 || choice > catalog.len() {
            writeln!(output, "Enter one of the displayed numbers.")
                .map_err(|error| ManagerError::io("write compatibility prompt", error))?;
            continue;
        }
        if let Some(reason) = &catalog[choice - 1].unavailable {
            writeln!(output, "That release is not installable: {reason}")
                .map_err(|error| ManagerError::io("write compatibility prompt", error))?;
            continue;
        }
        return Ok(choice - 1);
    }
}

fn incompatibility_reason(
    codex_version: &str,
    build_target: &str,
    official_version: &str,
) -> Option<String> {
    let mut reasons = Vec::new();
    if build_target != BUILD_TARGET {
        reasons.push(format!(
            "requires target {build_target}; manager is {BUILD_TARGET}"
        ));
    }
    if codex_version != official_version {
        reasons.push(format!(
            "requires official Codex {codex_version}; installed is {official_version}"
        ));
    }
    (!reasons.is_empty()).then(|| reasons.join("; "))
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

struct GitHubClient {
    agent: Agent,
    token: Option<String>,
}

impl GitHubClient {
    fn new() -> Self {
        let config = Agent::config_builder()
            .https_only(true)
            .max_redirects(5)
            .timeout_global(Some(Duration::from_secs(15 * 60)))
            .timeout_connect(Some(Duration::from_secs(15)))
            .timeout_recv_response(Some(Duration::from_secs(30)))
            .timeout_recv_body(Some(Duration::from_secs(30)))
            .build();
        Self {
            agent: Agent::new_with_config(config),
            token: ["GITHUB_TOKEN", "GH_TOKEN"]
                .into_iter()
                .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty())),
        }
    }

    fn get_json<T: DeserializeOwned>(&self, url: &str) -> Result<T> {
        self.get_json_optional(url)?.ok_or_else(|| {
            ManagerError::new(
                "github_api_not_found",
                format!("GitHub resource not found: {url}"),
            )
        })
    }

    fn get_json_optional<T: DeserializeOwned>(&self, url: &str) -> Result<Option<T>> {
        let mut request = self
            .agent
            .get(url)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", concat!("csa/", env!("CARGO_PKG_VERSION")));
        if let Some(token) = &self.token {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        let mut response = match request.call() {
            Ok(response) => response,
            Err(ureq::Error::StatusCode(404)) => return Ok(None),
            Err(ureq::Error::StatusCode(401)) => {
                return Err(ManagerError::new(
                    "github_auth_failed",
                    "GitHub rejected GITHUB_TOKEN or GH_TOKEN",
                ));
            }
            Err(ureq::Error::StatusCode(403)) => {
                return Err(ManagerError::new(
                    "github_api_forbidden",
                    "GitHub denied the API request; the public rate limit may be exhausted, so set GITHUB_TOKEN or GH_TOKEN and retry",
                ));
            }
            Err(error) => return Err(network_error("query GitHub API", error)),
        };
        require_response_host(&response, &["api.github.com"])?;
        let bytes = response
            .body_mut()
            .with_config()
            .limit(MAX_JSON_BYTES + 1)
            .read_to_vec()
            .map_err(|error| network_error("read GitHub API response", error))?;
        if bytes.len() as u64 > MAX_JSON_BYTES {
            return Err(ManagerError::new(
                "github_response_too_large",
                "GitHub API response exceeds the supported size",
            ));
        }
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| ManagerError::new("invalid_github_response", error.to_string()))
    }

    fn peel_tag(&self, repository: &str, tag: &str) -> Result<String> {
        let reference: GitReference =
            self.get_json(&format!("{API_ROOT}/{repository}/git/ref/tags/{tag}"))?;
        if reference.reference != format!("refs/tags/{tag}") {
            return Err(ManagerError::new(
                "invalid_release_tag",
                "GitHub tag reference differs from the requested release tag",
            ));
        }
        let mut object = reference.object;
        for depth in 0..5 {
            validate_sha(&object.sha)?;
            match object.kind.as_str() {
                "commit" => return Ok(object.sha),
                "tag" => {
                    let annotated: GitTag =
                        self.get_json(&format!("{API_ROOT}/{repository}/git/tags/{}", object.sha))?;
                    if depth == 0 && annotated.tag != tag {
                        return Err(ManagerError::new(
                            "invalid_release_tag",
                            "annotated tag name differs from the release tag",
                        ));
                    }
                    object = annotated.object;
                }
                _ => {
                    return Err(ManagerError::new(
                        "invalid_release_tag",
                        "release tag does not resolve to a commit",
                    ));
                }
            }
        }
        Err(ManagerError::new(
            "invalid_release_tag",
            "release tag indirection is too deep",
        ))
    }

    fn download_asset(
        &self,
        release_tag: &str,
        asset: &GitHubAsset,
        destination: &Path,
        expected_sha256: Option<&str>,
    ) -> Result<()> {
        validate_release_asset_url(release_tag, &asset.name, &asset.browser_download_url)?;
        let max_size = if destination
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == "artifact")
        {
            MAX_ARTIFACT_BYTES
        } else {
            MAX_RELEASE_FILE_BYTES
        };
        if asset.size == 0 || asset.size > max_size {
            return Err(ManagerError::new(
                "invalid_release_asset_size",
                format!("release asset has an invalid size: {}", asset.name),
            ));
        }
        let api_sha256 = github_sha256(&asset.digest)?;
        if expected_sha256.is_some_and(|expected| expected != api_sha256) {
            return Err(ManagerError::new(
                "release_asset_digest_mismatch",
                format!("GitHub asset digest differs: {}", asset.name),
            ));
        }
        let mut response = self
            .agent
            .get(&asset.browser_download_url)
            .header("Accept", "application/octet-stream")
            .header("User-Agent", concat!("csa/", env!("CARGO_PKG_VERSION")))
            .call()
            .map_err(|error| network_error("download compatibility release asset", error))?;
        require_response_host(
            &response,
            &[
                "github.com",
                "objects.githubusercontent.com",
                "release-assets.githubusercontent.com",
            ],
        )?;
        if response
            .body()
            .content_length()
            .is_some_and(|length| length != asset.size)
        {
            return Err(ManagerError::new(
                "release_asset_size_mismatch",
                format!("Content-Length differs for release asset: {}", asset.name),
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
                .limit(asset.size + 1)
                .reader();
            io::copy(&mut reader, &mut output)
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
        if copied != asset.size {
            let _ = fs::remove_file(destination);
            return Err(ManagerError::new(
                "release_asset_size_mismatch",
                format!(
                    "release asset {} expected {} bytes, received {copied}",
                    asset.name, asset.size
                ),
            ));
        }
        let (actual, size) = sha256_file(destination)?;
        if actual != api_sha256 || size != asset.size {
            let _ = fs::remove_file(destination);
            return Err(ManagerError::new(
                "release_asset_hash_mismatch",
                format!("release asset failed SHA-256 verification: {}", asset.name),
            ));
        }
        Ok(())
    }
}

#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<GitHubAsset>,
}

#[derive(Deserialize)]
struct GitHubAsset {
    name: String,
    state: String,
    size: u64,
    browser_download_url: String,
    digest: String,
}

#[derive(Deserialize)]
struct GitReference {
    #[serde(rename = "ref")]
    reference: String,
    object: GitObject,
}

#[derive(Deserialize)]
struct GitTag {
    tag: String,
    object: GitObject,
}

#[derive(Deserialize)]
struct GitObject {
    #[serde(rename = "type")]
    kind: String,
    sha: String,
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
    build_target: String,
    payload: Vec<ReleaseFile>,
    artifact: ReleaseFile,
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

fn stable_release_version(release: &GitHubRelease) -> Result<String> {
    if release.draft || release.prerelease {
        return Err(ManagerError::new(
            "invalid_upstream_release",
            "selected upstream release must be formal",
        ));
    }
    let version = release.tag_name.strip_prefix("rust-v").ok_or_else(|| {
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

fn release_assets(release: &GitHubRelease) -> Result<BTreeMap<String, &GitHubAsset>> {
    let mut assets = BTreeMap::new();
    for asset in &release.assets {
        validate_asset_name(&asset.name)?;
        if asset.state != "uploaded" {
            return Err(ManagerError::new(
                "invalid_compatibility_release",
                format!("release asset is not fully uploaded: {}", asset.name),
            ));
        }
        if assets.insert(asset.name.clone(), asset).is_some() {
            return Err(ManagerError::new(
                "invalid_compatibility_release",
                format!("duplicate release asset: {}", asset.name),
            ));
        }
    }
    Ok(assets)
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

fn validate_catalog_descriptor(
    descriptor: &CompatibilityRelease,
    release_tag: &str,
    csa_commit: &str,
    compat_id: &str,
) -> Result<()> {
    validate_asset_name(compat_id)?;
    validate_asset_name(&descriptor.build_target)?;
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
    if descriptor.schema != 1
        || descriptor.repository != CSA_REPOSITORY
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

fn validate_descriptor(
    descriptor: &CompatibilityRelease,
    release_tag: &str,
    csa_commit: &str,
    compat_id: &str,
    upstream_version: &str,
    upstream_tag: &str,
    upstream_commit: &str,
) -> Result<()> {
    validate_catalog_descriptor(descriptor, release_tag, csa_commit, compat_id)?;
    if descriptor.build_target != BUILD_TARGET
        || descriptor.upstream.version != upstream_version
        || descriptor.upstream.tag != upstream_tag
        || descriptor.upstream.commit != upstream_commit
    {
        return Err(ManagerError::new(
            "invalid_compatibility_release",
            "compatibility provenance differs from the exact release, source commit, upstream, or target",
        ));
    }
    Ok(())
}

fn descriptor_assets(descriptor: &CompatibilityRelease) -> Result<BTreeMap<String, &ReleaseFile>> {
    let mut assets = BTreeMap::new();
    let mut paths = BTreeSet::new();
    for (file, limit) in descriptor
        .payload
        .iter()
        .map(|file| (file, MAX_RELEASE_FILE_BYTES))
        .chain([(&descriptor.artifact, MAX_ARTIFACT_BYTES)])
    {
        validate_relative(&file.path, false)?;
        validate_asset_name(&file.asset)?;
        validate_sha256(&file.sha256)?;
        if file.size == 0 || file.size > limit {
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
    Ok(assets)
}

fn validate_declared_asset<'a>(
    file: &ReleaseFile,
    assets: &'a BTreeMap<String, &GitHubAsset>,
    checksums: &BTreeMap<String, String>,
) -> Result<&'a GitHubAsset> {
    let asset = assets.get(&file.asset).copied().ok_or_else(|| {
        ManagerError::new(
            "invalid_compatibility_release",
            format!("declared release asset is missing: {}", file.asset),
        )
    })?;
    if asset.size != file.size
        || checksums.get(&file.asset) != Some(&file.sha256)
        || asset.digest != format!("sha256:{}", file.sha256)
    {
        return Err(ManagerError::new(
            "invalid_compatibility_release",
            format!("release metadata differs for asset: {}", file.asset),
        ));
    }
    Ok(asset)
}

fn validate_downloaded_compatibility(
    compatibility: &LoadedCompatibility,
    descriptor: &CompatibilityRelease,
    upstream_commit: &str,
) -> Result<()> {
    let manifest = &compatibility.manifest;
    let manifest_artifact = compatibility.artifact();
    if manifest.compat_id != descriptor.compat_id
        || manifest.codex_version != descriptor.upstream.version
        || manifest.upstream_tag != descriptor.upstream.tag
        || manifest.upstream_commit != upstream_commit
        || manifest.build_target != BUILD_TARGET
        || manifest_artifact.filename != descriptor.artifact.path
        || manifest_artifact.size != descriptor.artifact.size
        || manifest_artifact.sha256 != descriptor.artifact.sha256
    {
        return Err(ManagerError::new(
            "invalid_compatibility_release",
            "downloaded manifest differs from the release provenance or artifact",
        ));
    }
    let expected_paths: BTreeSet<_> = compatibility.payload_files()?.into_keys().collect();
    let actual_paths: BTreeSet<_> = descriptor
        .payload
        .iter()
        .map(|file| file.path.clone())
        .collect();
    if expected_paths != actual_paths
        || descriptor.artifact.path != compatibility.artifact().filename
    {
        return Err(ManagerError::new(
            "invalid_compatibility_release",
            "release payload files do not exactly match the manifest",
        ));
    }
    Ok(())
}

fn validate_release_asset_url(release_tag: &str, asset_name: &str, url: &str) -> Result<()> {
    let uri: ureq::http::Uri = url.parse().map_err(|_| {
        ManagerError::new(
            "invalid_release_asset_url",
            format!("release asset URL is invalid: {asset_name}"),
        )
    })?;
    let suffix = format!("/releases/download/{release_tag}/{asset_name}");
    let repository = uri
        .path()
        .strip_suffix(&suffix)
        .and_then(|prefix| prefix.strip_prefix('/'));
    if uri.scheme_str() != Some("https")
        || !uri
            .host()
            .is_some_and(|host| host.eq_ignore_ascii_case("github.com"))
        || uri.port_u16().is_some()
        || uri.query().is_some()
        || !repository.is_some_and(|value| value.eq_ignore_ascii_case(CSA_REPOSITORY))
    {
        return Err(ManagerError::new(
            "invalid_release_asset_url",
            format!("release asset URL is outside {CSA_REPOSITORY}: {asset_name}"),
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

fn github_sha256(value: &str) -> Result<&str> {
    let digest = value.strip_prefix("sha256:").ok_or_else(|| {
        ManagerError::new(
            "invalid_release_asset_digest",
            "GitHub release asset digest must use sha256",
        )
    })?;
    validate_sha256(digest)?;
    Ok(digest)
}

fn network_error(context: &str, error: ureq::Error) -> ManagerError {
    ManagerError::new("network_error", format!("{context}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::{
        BUILD_TARGET, CSA_REPOSITORY, CompatibilityEntry, CompatibilityRelease, GitHubAsset,
        GitHubClient, GitHubRelease, MAX_ARTIFACT_BYTES, OPENAI_REPOSITORY, ReleaseFile,
        UpstreamRelease, descriptor_assets, github_sha256, incompatibility_reason, parse_checksums,
        prompt_catalog, release_assets, require_selection_mode, require_uri_host, select_requested,
        sort_catalog, stable_release_version, validate_declared_asset, validate_descriptor,
        validate_release_asset_url, version_key,
    };
    use std::collections::BTreeMap;
    use std::io::Cursor;

    fn release(tag: &str, draft: bool, prerelease: bool) -> GitHubRelease {
        GitHubRelease {
            tag_name: tag.to_owned(),
            draft,
            prerelease,
            assets: Vec::new(),
        }
    }

    #[test]
    fn only_exact_formal_rust_release_tags_are_stable() {
        assert_eq!(
            stable_release_version(&release("rust-v0.147.0", false, false)).unwrap(),
            "0.147.0"
        );
        for value in [
            release("rust-v0.148.0-rc.1", false, false),
            release("rust-v0.148", false, false),
            release("v0.148.0", false, false),
            release("rust-v0.148.0", true, false),
            release("rust-v0.148.0", false, true),
        ] {
            assert!(stable_release_version(&value).is_err());
        }
    }

    #[test]
    fn compatibility_catalog_sorts_and_selects_without_guessing() {
        let entry = |compat_id: &str, version: &str, target: &str| CompatibilityEntry {
            compat_id: compat_id.to_owned(),
            codex_version: version.to_owned(),
            build_target: target.to_owned(),
            version_key: version_key(version).unwrap(),
            unavailable: incompatibility_reason(version, target, "0.10.0"),
        };
        let mut catalog = vec![
            entry("rust-v0.9.0-native-join-p1", "0.9.0", "other-target"),
            entry("rust-v0.10.0-native-join-p3", "0.10.0", BUILD_TARGET),
            entry("rust-v0.10.0-native-join-p2", "0.10.0", BUILD_TARGET),
        ];
        sort_catalog(&mut catalog);
        assert_eq!(catalog[0].compat_id, "rust-v0.10.0-native-join-p2");
        assert_eq!(catalog[1].compat_id, "rust-v0.10.0-native-join-p3");
        assert_eq!(
            select_requested(&catalog, &catalog[1].compat_id).unwrap(),
            1
        );
        assert!(select_requested(&catalog, &catalog[2].compat_id).is_err());
        assert!(select_requested(&catalog, "missing").is_err());
        assert!(require_selection_mode(None, false, true).is_err());
        assert!(require_selection_mode(Some(&catalog[0].compat_id), false, false).is_ok());

        let mut input = Cursor::new(b"invalid\n3\n2\n");
        let mut output = Vec::new();
        assert_eq!(
            prompt_catalog(&catalog, &mut input, &mut output).unwrap(),
            1
        );
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("Codex 0.10.0"));
        assert!(output.contains("requires target other-target"));

        let mut input = Cursor::new(Vec::<u8>::new());
        assert!(prompt_catalog(&catalog[2..], &mut input, &mut Vec::new()).is_err());
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
    fn github_asset_digest_must_be_lowercase_sha256() {
        let digest = "a".repeat(64);
        assert_eq!(github_sha256(&format!("sha256:{digest}")).unwrap(), digest);
        assert!(github_sha256(&format!("sha512:{digest}")).is_err());
        assert!(github_sha256(&format!("sha256:{}", digest.to_uppercase())).is_err());
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
            build_target: BUILD_TARGET.to_owned(),
            payload: vec![file],
            artifact,
        };
        assert!(
            validate_descriptor(
                &descriptor,
                "compat-rust-v1.2.3-native-join-p1",
                &csa_commit,
                "rust-v1.2.3-native-join-p1",
                "1.2.3",
                "rust-v1.2.3",
                &commit,
            )
            .is_ok()
        );
        descriptor.upstream.commit = "d".repeat(40);
        assert!(
            validate_descriptor(
                &descriptor,
                "compat-rust-v1.2.3-native-join-p1",
                &csa_commit,
                "rust-v1.2.3-native-join-p1",
                "1.2.3",
                "rust-v1.2.3",
                &commit,
            )
            .is_err()
        );

        let asset = GitHubAsset {
            name: "payload--codex.exe".to_owned(),
            state: "uploaded".to_owned(),
            size: 3,
            browser_download_url: "https://example.invalid/asset".to_owned(),
            digest: format!("sha256:{sha}"),
        };
        let duplicate = GitHubRelease {
            tag_name: "compat-rust-v1.2.3-native-join-p1".to_owned(),
            draft: false,
            prerelease: false,
            assets: vec![
                asset,
                GitHubAsset {
                    name: "payload--codex.exe".to_owned(),
                    state: "uploaded".to_owned(),
                    size: 3,
                    browser_download_url: "https://example.invalid/asset".to_owned(),
                    digest: format!("sha256:{sha}"),
                },
            ],
        };
        assert!(release_assets(&duplicate).is_err());
        assert!(descriptor_assets(&descriptor).is_ok());
        assert!(
            validate_declared_asset(&descriptor.artifact, &BTreeMap::new(), &BTreeMap::new(),)
                .is_err()
        );

        let allowed = "https://release-assets.githubusercontent.com/asset"
            .parse()
            .unwrap();
        let rejected = "https://example.invalid/asset".parse().unwrap();
        assert!(require_uri_host(&allowed, &["release-assets.githubusercontent.com"]).is_ok());
        assert!(require_uri_host(&rejected, &["release-assets.githubusercontent.com"]).is_err());
    }

    #[test]
    fn download_preflight_rejects_untrusted_asset_metadata() {
        let client = GitHubClient::new();
        let tag = "compat-rust-v1.2.3-native-join-p1";
        let digest = "a".repeat(64);
        assert!(
            validate_release_asset_url(
                tag,
                "payload--codex.exe",
                &format!("https://github.com/DSLZL/CSA/releases/download/{tag}/payload--codex.exe"),
            )
            .is_ok()
        );
        assert!(
            validate_release_asset_url(
                tag,
                "payload--codex.exe",
                &format!("https://github.com/other/CSA/releases/download/{tag}/payload--codex.exe"),
            )
            .is_err()
        );
        let destination = std::env::temp_dir().join("artifact").join("codex.exe");
        let mut asset = GitHubAsset {
            name: "payload--codex.exe".to_owned(),
            state: "uploaded".to_owned(),
            size: 3,
            browser_download_url: "https://example.invalid/asset".to_owned(),
            digest: format!("sha256:{digest}"),
        };
        assert!(
            client
                .download_asset(tag, &asset, &destination, Some(&digest))
                .is_err()
        );
        asset.browser_download_url = format!(
            "https://github.com/{CSA_REPOSITORY}/releases/download/{tag}/{}",
            asset.name
        );
        asset.size = MAX_ARTIFACT_BYTES + 1;
        assert!(
            client
                .download_asset(tag, &asset, &destination, Some(&digest))
                .is_err()
        );
        asset.size = 3;
        asset.digest = format!("sha256:{}", "b".repeat(64));
        assert!(
            client
                .download_asset(tag, &asset, &destination, Some(&digest))
                .is_err()
        );
    }
}
