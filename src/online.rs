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
use std::io::{self, Write};
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
    let paths = ManagerPaths::resolve(options.manager_root.clone())?;
    let official = detect_official(
        runner,
        options.official.as_deref(),
        options.official_native.as_deref(),
        std::slice::from_ref(&paths.root),
    )?;
    let client = GitHubClient::new();
    let upstream_release: GitHubRelease =
        client.get_json(&format!("{API_ROOT}/{OPENAI_REPOSITORY}/releases/latest"))?;
    let upstream_version = stable_release_version(&upstream_release)?;
    let upstream_commit = client.peel_tag(OPENAI_REPOSITORY, &upstream_release.tag_name)?;
    if official.version != upstream_version {
        return Err(ManagerError::new(
            "official_not_latest_stable",
            format!(
                "installed official Codex is {}, but the current official stable release is {}",
                official.version, upstream_version
            ),
        ));
    }

    let compat_id = format!("rust-v{upstream_version}-native-join-p2");
    let release_tag = format!("compat-{compat_id}");
    let release_url = format!("{API_ROOT}/{CSA_REPOSITORY}/releases/tags/{release_tag}");
    let compatibility_release: GitHubRelease =
        client.get_json_optional(&release_url)?.ok_or_else(|| {
            ManagerError::new(
                "latest_not_yet_supported",
                format!(
                    "CSA has not published a formal compatibility release for {} ({})",
                    upstream_release.tag_name, upstream_commit
                ),
            )
        })?;
    if compatibility_release.draft
        || compatibility_release.prerelease
        || compatibility_release.tag_name != release_tag
    {
        return Err(ManagerError::new(
            "invalid_compatibility_release",
            "compatibility release must be formal and use the exact compatibility tag",
        ));
    }
    let csa_commit = client.peel_tag(CSA_REPOSITORY, &release_tag)?;
    let assets = release_assets(&compatibility_release)?;

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

    let checksums_asset = assets.get(RELEASE_CHECKSUMS).ok_or_else(|| {
        ManagerError::new(
            "invalid_compatibility_release",
            "compatibility release is missing SHA256SUMS",
        )
    })?;
    if checksums_asset.size == 0 || checksums_asset.size > MAX_RELEASE_FILE_BYTES {
        return Err(ManagerError::new(
            "invalid_compatibility_release",
            "SHA256SUMS has an invalid size",
        ));
    }
    let checksums_path = staging_path.join(RELEASE_CHECKSUMS);
    client.download_asset(&release_tag, checksums_asset, &checksums_path, None)?;
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
    let descriptor_path = staging_path.join(RELEASE_DESCRIPTOR);
    client.download_asset(
        &release_tag,
        descriptor_asset,
        &descriptor_path,
        Some(&checksums[RELEASE_DESCRIPTOR]),
    )?;
    let descriptor: CompatibilityRelease = read_json_file(&descriptor_path)?;
    validate_descriptor(
        &descriptor,
        &release_tag,
        &csa_commit,
        &compat_id,
        &upstream_version,
        &upstream_release.tag_name,
        &upstream_commit,
    )?;

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

    let payload_root = staging_path.join(&compat_id);
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
    let artifact_path = staging_path
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

struct GitHubClient {
    agent: Agent,
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
        let mut response = match self
            .agent
            .get(url)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", concat!("csa/", env!("CARGO_PKG_VERSION")))
            .call()
        {
            Ok(response) => response,
            Err(ureq::Error::StatusCode(404)) => return Ok(None),
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
        let expected_url = format!(
            "https://github.com/{CSA_REPOSITORY}/releases/download/{release_tag}/{}",
            asset.name
        );
        if asset.browser_download_url != expected_url {
            return Err(ManagerError::new(
                "invalid_release_asset_url",
                format!(
                    "release asset URL is outside {CSA_REPOSITORY}: {}",
                    asset.name
                ),
            ));
        }
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
            "GitHub latest release must be formal",
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

fn validate_descriptor(
    descriptor: &CompatibilityRelease,
    release_tag: &str,
    csa_commit: &str,
    compat_id: &str,
    upstream_version: &str,
    upstream_tag: &str,
    upstream_commit: &str,
) -> Result<()> {
    if descriptor.schema != 1
        || descriptor.repository != CSA_REPOSITORY
        || descriptor.release_tag != release_tag
        || descriptor.source_commit != csa_commit
        || descriptor.compat_id != compat_id
        || descriptor.build_target != BUILD_TARGET
        || descriptor.upstream.repository != OPENAI_REPOSITORY
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
        BUILD_TARGET, CSA_REPOSITORY, CompatibilityRelease, GitHubAsset, GitHubClient,
        GitHubRelease, MAX_ARTIFACT_BYTES, OPENAI_REPOSITORY, ReleaseFile, UpstreamRelease,
        descriptor_assets, github_sha256, parse_checksums, release_assets, require_uri_host,
        stable_release_version, validate_declared_asset, validate_descriptor,
    };
    use std::collections::BTreeMap;

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
