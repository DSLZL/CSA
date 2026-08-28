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
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;
use ureq::{Agent, ResponseExt};

const OPENAI_REPOSITORY: &str = "openai/codex";
const CSA_REPOSITORY: &str = "dslzl/CSA";
const GH_PROXY_ROOT: &str = "https://gh-proxy.com/";
const CLOUDFLARE_TRACE_URL: &str = "https://www.cloudflare.com/cdn-cgi/trace";
const ALIBABA_REGION_URL: &str = "https://ip.taobao.com/outGetIpInfo?ip=myip&accessKey=alibaba-inc";
const RELEASE_DESCRIPTOR: &str = "compatibility-release.json";
const RELEASE_CHECKSUMS: &str = "SHA256SUMS";
const MAX_REGION_TRACE_BYTES: u64 = 8 * 1024;
const MAX_GIT_REFS_BYTES: u64 = 16 * 1024 * 1024;
const MAX_RELEASE_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_ARTIFACT_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_COMPATIBILITY_TAGS: usize = 1_000;

struct CompatibilityEntry {
    compat_id: String,
    codex_version: String,
    build_target: String,
    version_key: (u64, u64, u64),
    csa_commit: String,
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
    let csa_commit = client.peel_tag(CSA_REPOSITORY, &release_tag)?;
    if csa_commit != selected.csa_commit {
        return Err(ManagerError::new(
            "compatibility_release_changed",
            "selected compatibility tag changed during installation",
        ));
    }
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
    if descriptor.upstream.version != selected.codex_version
        || descriptor.build_target != selected.build_target
    {
        return Err(ManagerError::new(
            "compatibility_release_changed",
            "selected compatibility metadata changed during installation",
        ));
    }

    let upstream_version = stable_release_version(&descriptor.upstream.tag)?;
    let upstream_commit = client.peel_tag(OPENAI_REPOSITORY, &descriptor.upstream.tag)?;
    validate_descriptor(
        &descriptor,
        &release_tag,
        &csa_commit,
        compat_id,
        &upstream_version,
        &descriptor.upstream.tag,
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

    let payload_root = selected_path.join(compat_id);
    ensure_managed_directory(&paths.root, &payload_root)?;
    for file in &descriptor.payload {
        validate_declared_asset(file, &checksums)?;
        let destination = payload_root.join(&file.path);
        let parent = destination.parent().ok_or_else(|| {
            ManagerError::new(
                "invalid_compatibility_release",
                "payload path has no parent",
            )
        })?;
        ensure_managed_directory(&paths.root, parent)?;
        if !client.download_asset(
            &release_tag,
            &file.asset,
            &destination,
            Some(file.size),
            Some(&file.sha256),
            MAX_RELEASE_FILE_BYTES,
        )? {
            return Err(ManagerError::new(
                "invalid_compatibility_release",
                format!("declared release asset is missing: {}", file.asset),
            ));
        }
    }
    validate_declared_asset(&descriptor.artifact, &checksums)?;
    let artifact_path = selected_path
        .join("artifact")
        .join(&descriptor.artifact.path);
    ensure_managed_directory(
        &paths.root,
        artifact_path.parent().expect("artifact path has a parent"),
    )?;
    if !client.download_asset(
        &release_tag,
        &descriptor.artifact.asset,
        &artifact_path,
        Some(descriptor.artifact.size),
        Some(&descriptor.artifact.sha256),
        MAX_ARTIFACT_BYTES,
    )? {
        return Err(ManagerError::new(
            "invalid_compatibility_release",
            format!(
                "declared release asset is missing: {}",
                descriptor.artifact.asset
            ),
        ));
    }

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
    let refs = client.repository_refs(CSA_REPOSITORY)?;
    for release_tag in compatibility_tags(&refs)? {
        let compat_id = release_tag
            .strip_prefix("compat-")
            .expect("compatibility_tags only returns compatibility tags");
        let csa_commit = peel_tag_from_refs(&refs, &release_tag)?;
        let entry_path = catalog_path.join(entries.len().to_string());
        ensure_managed_directory(manager_root, &entry_path)?;
        let Some((descriptor, _)) = download_release_metadata(client, &release_tag, &entry_path)?
        else {
            continue;
        };
        validate_catalog_descriptor(&descriptor, &release_tag, &csa_commit, compat_id)?;
        entries.push(CompatibilityEntry {
            compat_id: descriptor.compat_id,
            codex_version: descriptor.upstream.version.clone(),
            build_target: descriptor.build_target.clone(),
            version_key: version_key(&descriptor.upstream.version)?,
            csa_commit,
            unavailable: incompatibility_reason(
                &descriptor.upstream.version,
                &descriptor.build_target,
                official_version,
            ),
        });
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
    route: Cell<GitHubRoute>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GitHubRoute {
    Direct,
    Proxy,
}

impl GitHubClient {
    fn new() -> Self {
        Self::with_route(detect_github_route().unwrap_or(GitHubRoute::Direct))
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

    fn peel_tag(&self, repository: &str, tag: &str) -> Result<String> {
        peel_tag_from_refs(&self.repository_refs(repository)?, tag)
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
        if copied == 0 || copied > max_size || expected_size.is_some_and(|size| copied != size) {
            let _ = fs::remove_file(destination);
            return Err(ManagerError::new(
                "release_asset_size_mismatch",
                format!("release asset {asset_name} has an unexpected size: {copied} bytes"),
            ));
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
    ) -> Result<Option<ureq::http::Response<ureq::Body>>> {
        let route = self.route.get();
        match self.request_on_route(route, direct_url, accept, git_protocol) {
            Ok(response) => {
                require_route_host(&response, route, direct_hosts)?;
                Ok(Some(response))
            }
            Err(ureq::Error::StatusCode(404)) => Ok(None),
            Err(error) if route == GitHubRoute::Direct && should_try_proxy(&error) => {
                let direct_error = error.to_string();
                self.route.set(GitHubRoute::Proxy);
                match self.request_on_route(GitHubRoute::Proxy, direct_url, accept, git_protocol) {
                    Ok(response) => {
                        require_route_host(&response, GitHubRoute::Proxy, direct_hosts)?;
                        Ok(Some(response))
                    }
                    Err(ureq::Error::StatusCode(404)) => Ok(None),
                    Err(proxy_error) => Err(ManagerError::new(
                        "network_error",
                        format!(
                            "{context}: GitHub direct failed ({direct_error}); gh-proxy.com failed ({proxy_error})"
                        ),
                    )),
                }
            }
            Err(error) => Err(network_error(context, error)),
        }
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
        Some(GitHubRoute::Proxy)
    } else if probes.iter().any(Option::is_some) {
        Some(GitHubRoute::Direct)
    } else {
        None
    }
}

fn routed_url(route: GitHubRoute, direct_url: &str) -> String {
    match route {
        GitHubRoute::Direct => direct_url.to_owned(),
        GitHubRoute::Proxy => format!("{GH_PROXY_ROOT}{direct_url}"),
    }
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
    if route == GitHubRoute::Proxy {
        require_response_host(response, &["gh-proxy.com"])
    } else {
        require_response_host(response, direct_hosts)
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
        tags.push(format!("compat-{tag}"));
    }
    if tags.len() > MAX_COMPATIBILITY_TAGS {
        return Err(ManagerError::new(
            "compatibility_catalog_too_large",
            "compatibility catalog reached the 1,000-tag safety limit",
        ));
    }
    Ok(tags)
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

fn validate_declared_asset(file: &ReleaseFile, checksums: &BTreeMap<String, String>) -> Result<()> {
    if checksums.get(&file.asset) != Some(&file.sha256) {
        return Err(ManagerError::new(
            "invalid_compatibility_release",
            format!("release checksum differs for asset: {}", file.asset),
        ));
    }
    Ok(())
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

fn network_error(context: &str, error: impl std::fmt::Display) -> ManagerError {
    ManagerError::new("network_error", format!("{context}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::{
        BUILD_TARGET, CSA_REPOSITORY, CompatibilityEntry, CompatibilityRelease, GitHubClient,
        GitHubRoute, MAX_ARTIFACT_BYTES, OPENAI_REPOSITORY, ReleaseFile, UpstreamRelease,
        compatibility_tags, country_from_alibaba_region, country_from_cloudflare_trace,
        descriptor_assets, incompatibility_reason, parse_checksums, parse_git_refs,
        peel_tag_from_refs, prompt_catalog, release_asset_url, require_selection_mode,
        require_uri_host, route_from_region_probes, routed_url, select_requested, should_try_proxy,
        sort_catalog, stable_release_version, validate_declared_asset, validate_descriptor,
        version_key,
    };
    use std::collections::BTreeMap;
    use std::io::Cursor;
    use std::time::Duration;

    #[test]
    fn only_exact_formal_rust_release_tags_are_stable() {
        assert_eq!(stable_release_version("rust-v0.147.0").unwrap(), "0.147.0");
        for tag in ["rust-v0.148.0-rc.1", "rust-v0.148", "v0.148.0"] {
            assert!(stable_release_version(tag).is_err());
        }
    }

    #[test]
    fn compatibility_catalog_sorts_and_selects_without_guessing() {
        let entry = |compat_id: &str, version: &str, target: &str| CompatibilityEntry {
            compat_id: compat_id.to_owned(),
            codex_version: version.to_owned(),
            build_target: target.to_owned(),
            version_key: version_key(version).unwrap(),
            csa_commit: "a".repeat(40),
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

        assert!(descriptor_assets(&descriptor).is_ok());
        let mut checksums = BTreeMap::new();
        checksums.insert(descriptor.artifact.asset.clone(), sha);
        assert!(validate_declared_asset(&descriptor.artifact, &checksums).is_ok());
        checksums.clear();
        assert!(validate_declared_asset(&descriptor.artifact, &checksums).is_err());

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
            routed_url(GitHubRoute::Proxy, &direct),
            format!("https://gh-proxy.com/{direct}")
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
            Some(GitHubRoute::Proxy)
        );
        assert_eq!(
            route_from_region_probes([Some(true), Some(false)]),
            Some(GitHubRoute::Proxy)
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
