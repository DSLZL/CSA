use crate::error::{ManagerError, Result};
use crate::hash::{sha256_bytes, sha256_file};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CompatibilityManifest {
    pub schema: u32,
    pub compat_id: String,
    pub codex_version: String,
    pub upstream_tag: String,
    pub upstream_commit: String,
    pub patch_api: u32,
    pub patch_set_version: u32,
    pub rust_toolchain: String,
    pub rustc_commit: String,
    pub build_target: String,
    pub source_hashes: String,
    pub source_hashes_sha256: String,
    pub preimage_absent: Vec<String>,
    pub patches: Vec<PatchEntry>,
    pub preimage: BTreeMap<String, String>,
    pub artifacts: BTreeMap<String, ArtifactEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family_id: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub files: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PatchEntry {
    pub path: String,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactEntry {
    pub url: String,
    pub filename: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PatchFamily {
    schema: u32,
    family_id: String,
    patch_api: u32,
    patch_set_version: u32,
    bindings: Vec<FamilyBinding>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FamilyBinding {
    compat_id: String,
    manifest: String,
    sha256: String,
}

#[derive(Clone, Debug)]
pub struct LoadedCompatibility {
    pub manifest: CompatibilityManifest,
    pub manifest_path: PathBuf,
    pub payload_root: PathBuf,
    pub patch_paths: Vec<PathBuf>,
    family_id: Option<String>,
    file_sources: BTreeMap<String, PathBuf>,
}

impl LoadedCompatibility {
    pub fn load(manifest_path: &Path) -> Result<Self> {
        if !manifest_path.is_absolute() {
            return Err(ManagerError::new(
                "invalid_manifest_path",
                "manifest path must be absolute",
            ));
        }
        let manifest_metadata = fs::symlink_metadata(manifest_path).map_err(|error| {
            ManagerError::io(
                &format!("inspect manifest {}", manifest_path.display()),
                error,
            )
        })?;
        if manifest_metadata.file_type().is_symlink() || !manifest_metadata.is_file() {
            return Err(ManagerError::new(
                "invalid_manifest_path",
                "manifest must be a regular file, not a symlink",
            ));
        }
        let manifest_path = manifest_path.canonicalize().map_err(|error| {
            ManagerError::io(
                &format!("canonicalize manifest {}", manifest_path.display()),
                error,
            )
        })?;
        let manifest_parent = manifest_path
            .parent()
            .ok_or_else(|| ManagerError::new("invalid_manifest_path", "manifest has no parent"))?
            .to_path_buf();
        let manifest_bytes = fs::read(&manifest_path).map_err(|error| {
            ManagerError::io(&format!("read manifest {}", manifest_path.display()), error)
        })?;
        let text = std::str::from_utf8(&manifest_bytes).map_err(|error| {
            ManagerError::new("invalid_manifest", format!("manifest UTF-8: {error}"))
        })?;
        let mut manifest: CompatibilityManifest = toml::from_str(text).map_err(|error| {
            ManagerError::new("invalid_manifest", format!("manifest TOML: {error}"))
        })?;
        if manifest_parent.file_name().and_then(|value| value.to_str()) != Some(&manifest.compat_id)
        {
            return Err(ManagerError::new(
                "compat_path_mismatch",
                "compat_id must equal the manifest directory name",
            ));
        }
        let (payload_root, family_id, file_sources) = match manifest.schema {
            1 => {
                if manifest.family_id.is_some() || !manifest.files.is_empty() {
                    return Err(ManagerError::new(
                        "unsupported_manifest",
                        "schema 1 may not declare family_id or files",
                    ));
                }
                let sources = legacy_file_sources(&manifest_parent, &manifest)?;
                (manifest_parent, None, sources)
            }
            2 => {
                let family_id = manifest.family_id.clone().ok_or_else(|| {
                    ManagerError::new("invalid_manifest", "schema 2 requires family_id")
                })?;
                let family_root = validate_family_binding(
                    &manifest_path,
                    &manifest_bytes,
                    &manifest,
                    &family_id,
                )?;
                let sources = family_file_sources(&family_root, &manifest)?;
                manifest.schema = 1;
                manifest.family_id = None;
                manifest.files.clear();
                (family_root, Some(family_id), sources)
            }
            _ => {
                return Err(ManagerError::new(
                    "unsupported_manifest",
                    "only manifest schemas 1 and 2 are supported",
                ));
            }
        };
        validate_manifest(&manifest)?;

        let source_hashes_path = file_sources[&manifest.source_hashes].clone();
        let source_hashes_bytes = fs::read(&source_hashes_path).map_err(|error| {
            ManagerError::io(
                &format!("read source hashes {}", source_hashes_path.display()),
                error,
            )
        })?;
        if sha256_bytes(&source_hashes_bytes) != manifest.source_hashes_sha256 {
            return Err(ManagerError::new(
                "source_hashes_mismatch",
                "source-hashes document does not match manifest digest",
            ));
        }
        let source_hashes: SourceHashes =
            serde_json::from_slice(&source_hashes_bytes).map_err(|error| {
                ManagerError::new(
                    "invalid_source_hashes",
                    format!("source-hashes JSON: {error}"),
                )
            })?;
        validate_source_hashes(&manifest, &source_hashes)?;

        let mut patch_paths = Vec::with_capacity(manifest.patches.len());
        let mut touched = BTreeSet::new();
        for patch in &manifest.patches {
            let path = file_sources[&patch.path].clone();
            let (actual, _) = sha256_file(&path)?;
            if actual != patch.sha256 {
                return Err(ManagerError::new(
                    "patch_hash_mismatch",
                    format!("patch hash mismatch: {}", patch.path),
                ));
            }
            touched.extend(touched_paths(&path)?);
            patch_paths.push(path);
        }
        let expected: BTreeSet<_> = manifest
            .preimage
            .keys()
            .chain(manifest.preimage_absent.iter())
            .cloned()
            .collect();
        if touched != expected {
            return Err(ManagerError::new(
                "preimage_coverage_mismatch",
                "preimages do not exactly cover patch-touched paths",
            ));
        }

        Ok(Self {
            manifest,
            manifest_path,
            payload_root,
            patch_paths,
            family_id,
            file_sources,
        })
    }

    pub fn artifact(&self) -> &ArtifactEntry {
        &self.manifest.artifacts[&self.manifest.build_target]
    }

    pub fn test_contract(&self) -> Result<TestContract> {
        let path = self.file_sources["test-contract.json"].clone();
        let bytes = fs::read(&path).map_err(|error| {
            ManagerError::io(&format!("read test contract {}", path.display()), error)
        })?;
        let contract: TestContract = serde_json::from_slice(&bytes).map_err(|error| {
            ManagerError::new(
                "invalid_test_contract",
                format!("test-contract JSON: {error}"),
            )
        })?;
        contract.validate(&self.manifest)?;
        Ok(contract)
    }

    pub fn payload_files(&self) -> Result<BTreeMap<String, Vec<u8>>> {
        let mut files = BTreeMap::new();
        let manifest = toml::to_string(&self.manifest).map_err(|error| {
            ManagerError::new(
                "invalid_manifest",
                format!("serialize canonical schema-1 manifest: {error}"),
            )
        })?;
        insert_payload_file(&mut files, "manifest.toml", manifest.into_bytes())?;
        for (logical, path) in &self.file_sources {
            let bytes = fs::read(path).map_err(|error| {
                ManagerError::io(&format!("read payload file {}", path.display()), error)
            })?;
            insert_payload_file(&mut files, logical, bytes)?;
        }
        Ok(files)
    }

    pub fn family_id(&self) -> Option<&str> {
        self.family_id.as_deref()
    }
}

fn insert_payload_file(
    files: &mut BTreeMap<String, Vec<u8>>,
    relative: &str,
    contents: Vec<u8>,
) -> Result<()> {
    validate_relative(relative, false)?;
    if files.insert(relative.to_owned(), contents).is_some() {
        return Err(ManagerError::new(
            "duplicate_payload_path",
            format!("payload path is declared more than once: {relative}"),
        ));
    }
    Ok(())
}

fn logical_payload_paths(manifest: &CompatibilityManifest) -> BTreeSet<String> {
    manifest
        .patches
        .iter()
        .map(|patch| patch.path.clone())
        .chain([
            manifest.source_hashes.clone(),
            "test-contract.json".to_owned(),
        ])
        .collect()
}

fn legacy_file_sources(
    payload_root: &Path,
    manifest: &CompatibilityManifest,
) -> Result<BTreeMap<String, PathBuf>> {
    logical_payload_paths(manifest)
        .into_iter()
        .map(|logical| {
            let source = resolve_regular_relative(payload_root, &logical)?;
            Ok((logical, source))
        })
        .collect()
}

fn family_file_sources(
    family_root: &Path,
    manifest: &CompatibilityManifest,
) -> Result<BTreeMap<String, PathBuf>> {
    let expected = logical_payload_paths(manifest);
    let actual: BTreeSet<_> = manifest.files.keys().cloned().collect();
    if actual != expected {
        return Err(ManagerError::new(
            "invalid_manifest",
            "schema-2 files must exactly map every logical payload file",
        ));
    }
    let mut physical = BTreeSet::new();
    manifest
        .files
        .iter()
        .map(|(logical, relative)| {
            if !physical.insert(relative) {
                return Err(ManagerError::new(
                    "duplicate_payload_path",
                    "one physical family file may not serve multiple logical paths",
                ));
            }
            let source = resolve_regular_relative(family_root, relative)?;
            Ok((logical.clone(), source))
        })
        .collect()
}

fn validate_family_binding(
    manifest_path: &Path,
    manifest_bytes: &[u8],
    manifest: &CompatibilityManifest,
    family_id: &str,
) -> Result<PathBuf> {
    if !valid_compat_id(family_id) {
        return Err(ManagerError::new(
            "invalid_manifest",
            "schema-2 family_id is invalid",
        ));
    }
    let compat_root = manifest_path
        .parent()
        .ok_or_else(|| ManagerError::new("invalid_manifest_path", "manifest has no parent"))?;
    let bindings_root = compat_root.parent().ok_or_else(|| {
        ManagerError::new("invalid_manifest_path", "binding directory has no parent")
    })?;
    let family_root = bindings_root.parent().ok_or_else(|| {
        ManagerError::new("invalid_manifest_path", "bindings directory has no parent")
    })?;
    if bindings_root.file_name().and_then(|value| value.to_str()) != Some("bindings")
        || family_root.file_name().and_then(|value| value.to_str()) != Some(family_id)
    {
        return Err(ManagerError::new(
            "compat_path_mismatch",
            "schema-2 manifest must be under <family>/bindings/<compat_id>",
        ));
    }
    let family_root = family_root.canonicalize().map_err(|error| {
        ManagerError::io(
            &format!("canonicalize patch family {}", family_root.display()),
            error,
        )
    })?;
    let family_path = resolve_regular_relative(&family_root, "family.toml")?;
    let family_text = fs::read_to_string(&family_path).map_err(|error| {
        ManagerError::io(
            &format!("read patch family {}", family_path.display()),
            error,
        )
    })?;
    let family: PatchFamily = toml::from_str(&family_text).map_err(|error| {
        ManagerError::new("invalid_manifest", format!("patch family TOML: {error}"))
    })?;
    if family.schema != 2
        || family.family_id != family_id
        || family.patch_api != manifest.patch_api
        || family.patch_set_version != manifest.patch_set_version
        || family.bindings.is_empty()
    {
        return Err(ManagerError::new(
            "invalid_manifest",
            "patch family identity or API differs from its binding",
        ));
    }

    let mut ids = BTreeSet::new();
    let mut paths = BTreeSet::new();
    let mut selected = 0;
    for binding in &family.bindings {
        let expected_path = format!("bindings/{}/manifest.toml", binding.compat_id);
        if !valid_compat_id(&binding.compat_id)
            || !valid_sha(&binding.sha256, 64)
            || binding.manifest != expected_path
            || !ids.insert(&binding.compat_id)
            || !paths.insert(&binding.manifest)
        {
            return Err(ManagerError::new(
                "invalid_manifest",
                "patch family contains an invalid or duplicate binding",
            ));
        }
        let binding_path = resolve_regular_relative(&family_root, &binding.manifest)?;
        let bytes = fs::read(&binding_path).map_err(|error| {
            ManagerError::io(
                &format!("read family binding {}", binding_path.display()),
                error,
            )
        })?;
        if sha256_bytes(&bytes) != binding.sha256 {
            return Err(ManagerError::new(
                "manifest_hash_mismatch",
                format!("family binding digest mismatch: {}", binding.manifest),
            ));
        }
        if binding_path == manifest_path {
            selected += 1;
            if binding.compat_id != manifest.compat_id || bytes != manifest_bytes {
                return Err(ManagerError::new(
                    "manifest_hash_mismatch",
                    "family index selects a different exact binding",
                ));
            }
        }
    }
    if selected != 1 {
        return Err(ManagerError::new(
            "invalid_manifest",
            "family index must select the exact binding once",
        ));
    }
    Ok(family_root)
}

fn resolve_regular_relative(root: &Path, value: &str) -> Result<PathBuf> {
    validate_relative(value, false)?;
    let mut path = root.to_path_buf();
    for component in value.split('/') {
        path.push(component);
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            ManagerError::io(&format!("inspect payload file {}", path.display()), error)
        })?;
        if metadata.file_type().is_symlink() {
            return Err(ManagerError::new(
                "unsafe_payload_path",
                format!("payload path may not contain a symlink: {value}"),
            ));
        }
    }
    if !path.is_file() {
        return Err(ManagerError::new(
            "invalid_payload_path",
            format!("payload path is not a regular file: {value}"),
        ));
    }
    let canonical = path.canonicalize().map_err(|error| {
        ManagerError::io(
            &format!("canonicalize payload file {}", path.display()),
            error,
        )
    })?;
    if !canonical.starts_with(root) {
        return Err(ManagerError::new(
            "unsafe_payload_path",
            format!("payload path escapes its root: {value}"),
        ));
    }
    Ok(canonical)
}

fn validate_manifest(manifest: &CompatibilityManifest) -> Result<()> {
    if manifest.schema != 1 || manifest.patch_api != 1 || manifest.patch_set_version == 0 {
        return Err(ManagerError::new(
            "unsupported_manifest",
            "only schema 1 / patch API 1 with a positive patch set is supported",
        ));
    }
    if !valid_compat_id(&manifest.compat_id)
        || !valid_version(&manifest.codex_version)
        || !valid_version(&manifest.rust_toolchain)
        || !valid_sha(&manifest.upstream_commit, 40)
        || !valid_sha(&manifest.rustc_commit, 40)
        || !valid_sha(&manifest.source_hashes_sha256, 64)
        || !valid_target(&manifest.build_target)
        || manifest.upstream_tag.is_empty()
    {
        return Err(ManagerError::new(
            "invalid_manifest",
            "manifest contains an invalid identifier, version, commit, digest, tag, or target",
        ));
    }
    validate_relative(&manifest.source_hashes, false)?;

    let expected_patch_count = match manifest.patch_set_version {
        1 => 5,
        2 => 6,
        3 => 11,
        4 => 12,
        5 => 13,
        6 => 14,
        7 => 15,
        8 => 16,
        9 => 17,
        _ => {
            return Err(ManagerError::new(
                "unsupported_manifest",
                "only patch sets 1 through 9 are supported",
            ));
        }
    };
    if manifest.patches.len() != expected_patch_count {
        return Err(ManagerError::new(
            "invalid_patch_set",
            format!(
                "patch set {} requires exactly {expected_patch_count} ordered patches",
                manifest.patch_set_version
            ),
        ));
    }
    let patch_names: Vec<_> = manifest.patches.iter().map(|patch| &patch.path).collect();
    if !patch_names.windows(2).all(|pair| pair[0] < pair[1]) {
        return Err(ManagerError::new(
            "invalid_patch_set",
            "patch paths must be unique and lexically ordered",
        ));
    }
    for patch in &manifest.patches {
        validate_relative(&patch.path, false)?;
        if !valid_sha(&patch.sha256, 64) {
            return Err(ManagerError::new(
                "invalid_patch_set",
                format!("invalid patch digest: {}", patch.path),
            ));
        }
    }

    if manifest.preimage.is_empty() {
        return Err(ManagerError::new(
            "invalid_preimage",
            "present preimage map must not be empty",
        ));
    }
    let mut all_preimages = BTreeSet::new();
    for (path, digest) in &manifest.preimage {
        validate_relative(path, true)?;
        if !valid_sha(digest, 64) || !all_preimages.insert(path) {
            return Err(ManagerError::new(
                "invalid_preimage",
                format!("invalid or duplicate preimage: {path}"),
            ));
        }
    }
    for path in &manifest.preimage_absent {
        validate_relative(path, true)?;
        if !all_preimages.insert(path) {
            return Err(ManagerError::new(
                "invalid_preimage",
                format!("present and absent preimages overlap: {path}"),
            ));
        }
    }

    if manifest.artifacts.len() != 1 || !manifest.artifacts.contains_key(&manifest.build_target) {
        return Err(ManagerError::new(
            "invalid_artifact",
            "artifacts must contain only the exact build target",
        ));
    }
    let artifact = &manifest.artifacts[&manifest.build_target];
    if !["https://", "artifact://", "unpublished://"]
        .iter()
        .any(|prefix| artifact.url.starts_with(prefix))
        || !valid_filename(&artifact.filename)
        || !valid_sha(&artifact.sha256, 64)
        || artifact.size == 0
    {
        return Err(ManagerError::new(
            "invalid_artifact",
            "artifact URL, filename, digest, or size is invalid",
        ));
    }
    Ok(())
}

fn validate_source_hashes(manifest: &CompatibilityManifest, hashes: &SourceHashes) -> Result<()> {
    if hashes.schema != 1
        || hashes.algorithm != "sha256"
        || hashes.content != "git_blob"
        || hashes.commit != manifest.upstream_commit
        || hashes.present != manifest.preimage
        || hashes.absent != manifest.preimage_absent
    {
        return Err(ManagerError::new(
            "source_hashes_mismatch",
            "source-hashes contract differs from the manifest",
        ));
    }
    Ok(())
}

fn touched_paths(path: &Path) -> Result<BTreeSet<String>> {
    let text = fs::read_to_string(path)
        .map_err(|error| ManagerError::io(&format!("read patch {}", path.display()), error))?;
    let mut touched = BTreeSet::new();
    for line in text
        .lines()
        .filter(|line| line.starts_with("diff --git a/"))
    {
        let rest = line
            .strip_prefix("diff --git a/")
            .and_then(|value| value.split_once(" b/"))
            .ok_or_else(|| {
                ManagerError::new("invalid_patch", format!("invalid diff header: {line}"))
            })?;
        if rest.0 != rest.1 {
            return Err(ManagerError::new(
                "invalid_patch",
                format!("renames are unsupported: {line}"),
            ));
        }
        validate_relative(rest.0, true)?;
        touched.insert(rest.0.to_owned());
    }
    if touched.is_empty() {
        return Err(ManagerError::new(
            "invalid_patch",
            format!("patch touches no files: {}", path.display()),
        ));
    }
    Ok(touched)
}

pub(crate) fn validate_relative(value: &str, source: bool) -> Result<()> {
    let invalid = value.is_empty()
        || value.starts_with('/')
        || value.contains('\\')
        || value.contains(':')
        || value
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..");
    if invalid || (source && !value.starts_with("codex-rs/")) {
        return Err(ManagerError::new(
            "invalid_relative_path",
            format!("invalid payload path: {value}"),
        ));
    }
    Ok(())
}

fn valid_compat_id(value: &str) -> bool {
    let Some(first) = value.bytes().next() else {
        return false;
    };
    value.len() >= 2
        && (first.is_ascii_lowercase() || first.is_ascii_digit())
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
        })
}

fn valid_version(value: &str) -> bool {
    let parts: Vec<_> = value.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

fn valid_sha(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_target(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

fn valid_filename(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && !value.bytes().any(|byte| b"/\\:".contains(&byte))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceHashes {
    schema: u32,
    algorithm: String,
    content: String,
    commit: String,
    present: BTreeMap<String, String>,
    absent: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestContract {
    pub schema: u32,
    pub compat_id: String,
    pub parameters: BTreeMap<String, String>,
    pub cwd: String,
    pub common_env: BTreeMap<String, String>,
    pub generation: Vec<ContractStep>,
    pub tests: Vec<ContractStep>,
    pub build: BuildContract,
    pub known_upstream_errata: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContractStep {
    pub name: String,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    pub argv: Vec<String>,
    #[serde(default)]
    pub output: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildContract {
    pub env: BTreeMap<String, String>,
    pub argv: Vec<String>,
    pub artifact: String,
}

impl TestContract {
    fn validate(&self, manifest: &CompatibilityManifest) -> Result<()> {
        let p2 = manifest.patch_set_version == 2;
        let p3 = matches!(manifest.patch_set_version, 3..=9);
        let p3_isolates_background_exit = matches!(manifest.patch_set_version, 6..=9);
        let p3_tui_offset = usize::from(p3_isolates_background_exit);
        let p3_offset = usize::from(p3);
        let transport_fallback = matches!(manifest.patch_set_version, 6..=9)
            && manifest.patches.iter().any(|patch| {
                patch.path == "patches/0013-subagent-live-polish.patch"
                    && matches!(
                        patch.sha256.as_str(),
                        "37853b54b759412b4f10a942dc036a2ffb18a03091455617ea81cd832ace9ce4"
                            | "764afeb0d0fb06b58ac42dabce9778c6065ed1b44b2b696d12e396d94affeb9b"
                    )
            });
        let transport_fallback_offset = usize::from(transport_fallback);
        let csa_orbit = matches!(manifest.patch_set_version, 7..=9)
            && manifest
                .patches
                .iter()
                .any(|patch| patch.path == "patches/0015-csa-1x1-lossless-orbit.patch");
        let csa_orbit_offset = usize::from(csa_orbit);
        let state_db_compat = manifest.patch_set_version == 9
            && manifest
                .patches
                .iter()
                .any(|patch| patch.path == "patches/0017-codex-state-db-line-endings.patch");
        let batch_join = (p2 || p3)
            && manifest
                .patches
                .iter()
                .any(|patch| patch.path == "patches/0005-tests-telemetry-batch-join.patch");
        let expected_test_count = match manifest.patch_set_version {
            1 => 7,
            2 if batch_join => 11,
            2 => 8,
            3 if batch_join => 15,
            4 if batch_join => 16,
            5 if batch_join => 16,
            6 if batch_join => 17 + transport_fallback_offset,
            7..=8 if batch_join && csa_orbit => 17 + transport_fallback_offset + csa_orbit_offset,
            9 if batch_join && csa_orbit && state_db_compat => {
                18 + transport_fallback_offset + csa_orbit_offset
            }
            _ => {
                return Err(ManagerError::new(
                    "invalid_test_contract",
                    "test contract uses an unsupported patch set",
                ));
            }
        };
        let shape_matches = self.schema == 1
            && self.compat_id == manifest.compat_id
            && self.cwd == "{source}/codex-rs"
            && self.generation.len() == 2
            && self.tests.len() == expected_test_count
            && self.build.artifact
                == format!(
                    "{{cargo_target}}/{}/release/{}",
                    manifest.build_target, manifest.artifacts[&manifest.build_target].filename
                );
        if !shape_matches {
            return Err(ManagerError::new(
                "invalid_test_contract",
                "test contract shape does not match patch API 1",
            ));
        }
        if self.generation.iter().chain(&self.tests).any(|step| {
            !matches!(
                step.output.as_deref(),
                None | Some("live") | Some("failure-only")
            )
        }) {
            return Err(ManagerError::new(
                "invalid_test_contract",
                "test contract uses an unsupported output policy",
            ));
        }

        let branding_matches = !(p2 || p3)
            || step_matches(
                &self.tests[p3_offset],
                "CSA startup version display",
                &[
                    "cargo",
                    "test",
                    "-p",
                    "codex-tui",
                    "session_header_appends_csa_to_display_and_raw_versions",
                    "--",
                    "--nocapture",
                ],
                &[],
            );
        let first_native_test = if batch_join {
            2 + p3_offset + transport_fallback_offset
        } else if p2 {
            1
        } else {
            0
        };
        let native_tests = &self.tests[first_native_test..];
        let batch_tests_match = !batch_join
            || (step_matches(
                &self.tests[1 + p3_offset],
                "ephemeral parent full-history fork",
                &[
                    "cargo",
                    "test",
                    "-p",
                    "codex-core",
                    "multi_agent_v2_ephemeral_full_history_fork_uses_live_context_and_accepts_service_tier",
                    "--",
                    "--nocapture",
                ],
                &[],
            ) && step_matches(
                &self.tests[7 + p3_offset + transport_fallback_offset],
                "batch Join tool schema",
                &[
                    "cargo",
                    "test",
                    "-p",
                    "codex-core",
                    "join_agents_tool_requires_all_exact_runs_in_one_call",
                    "--",
                    "--nocapture",
                ],
                &[],
            ) && step_matches(
                &self.tests[8 + p3_offset + transport_fallback_offset],
                "batch Join waits for every exact run",
                &[
                    "cargo",
                    "test",
                    "-p",
                    "codex-core",
                    "join_agents_waits_for_every_exact_run_and_preserves_order",
                    "--",
                    "--nocapture",
                ],
                &[],
            ));
        let transport_fallback_test_matches = !transport_fallback
            || step_matches(
                &self.tests[3],
                "subagent transport fallback inheritance",
                &[
                    "cargo",
                    "test",
                    "-p",
                    "codex-core",
                    "spawned_child_inherits_parent_http_fallback_for_the_same_provider",
                    "--",
                    "--nocapture",
                ],
                &[],
            );

        let parameters_match = map_matches(
            &self.parameters,
            &[
                ("source", "absolute clean checkout path"),
                ("cargo_target", "absolute disposable Cargo target path"),
            ],
        );
        let common_env_matches = if p3 {
            map_matches(
                &self.common_env,
                &[
                    (
                        "CARGO_BUILD_JOBS",
                        if p3_isolates_background_exit {
                            "2"
                        } else {
                            "1"
                        },
                    ),
                    ("CARGO_INCREMENTAL", "0"),
                    ("CARGO_TARGET_DIR", "{cargo_target}"),
                    ("INSTA_OUTPUT", "none"),
                    ("INSTA_UPDATE", "no"),
                    ("INSTA_WORKSPACE_ROOT", "{source}/codex-rs"),
                    ("RUST_MIN_STACK", "8388608"),
                ],
            )
        } else {
            map_matches(
                &self.common_env,
                &[
                    ("CARGO_INCREMENTAL", "0"),
                    ("CARGO_TARGET_DIR", "{cargo_target}"),
                    ("RUST_MIN_STACK", "8388608"),
                ],
            )
        };
        let generation_matches = step_matches(
            &self.generation[0],
            "stable schema and embedded exports",
            &[
                "cargo",
                "test",
                "-p",
                "codex-app-server-protocol",
                "write_schema_fixtures_from_env",
                "--",
                "--ignored",
                "--nocapture",
            ],
            &[
                ("CODEX_APP_SERVER_SCHEMA_EXPERIMENTAL", "0"),
                (
                    "CODEX_APP_SERVER_SCHEMA_ROOT",
                    "{source}/codex-rs/app-server-protocol/schema",
                ),
            ],
        ) && step_matches(
            &self.generation[1],
            "experimental embedded exports",
            &[
                "cargo",
                "test",
                "-p",
                "codex-app-server-protocol",
                "write_schema_fixtures_from_env",
                "--",
                "--ignored",
                "--nocapture",
            ],
            &[
                ("CODEX_APP_SERVER_SCHEMA_EXPERIMENTAL", "1"),
                (
                    "CODEX_APP_SERVER_SCHEMA_ROOT",
                    "{source}/codex-rs/app-server-protocol/schema",
                ),
            ],
        );
        let tests_match = step_matches(
            &native_tests[0],
            "schema reverse-check",
            &[
                "cargo",
                "test",
                "-p",
                "codex-app-server-protocol",
                "schema_fixtures_tests",
                "--",
                "--nocapture",
            ],
            &[(
                "CODEX_APP_SERVER_SCHEMA_ROOT",
                "{source}/codex-rs/app-server-protocol/schema",
            )],
        ) && step_matches(
            &native_tests[1],
            "completion registry",
            &[
                "cargo",
                "test",
                "-p",
                "codex-core",
                "agent::completion::tests",
                "--",
                "--nocapture",
            ],
            &[],
        ) && step_matches(
            &native_tests[2],
            "terminal outcome mapping",
            &[
                "cargo",
                "test",
                "-p",
                "codex-core",
                "agent_run_terminal_mapper_preserves_exact_outcomes",
                "--",
                "--nocapture",
            ],
            &[],
        ) && step_matches(
            &native_tests[3],
            "replayable terminal publication",
            &[
                "cargo",
                "test",
                "-p",
                "codex-core",
                "spawned_v2_terminal_events_publish_replayable_exact_run_outcomes",
                "--",
                "--nocapture",
            ],
            &[],
        ) && step_matches(
            &native_tests[4],
            "Join tool schema",
            &[
                "cargo",
                "test",
                "-p",
                "codex-core",
                "join_agent_tool_requires_exact_run_without_timeout",
                "--",
                "--nocapture",
            ],
            &[],
        ) && step_matches(
            &self.tests[first_native_test + if batch_join { 7 } else { 5 }],
            "invalid Join inputs",
            &[
                "cargo",
                "test",
                "-p",
                "codex-core",
                "multi_agent_v2_join_rejects_invalid_arguments_targets_and_runs",
                "--",
                "--nocapture",
            ],
            &[],
        ) && step_matches(
            &self.tests[first_native_test + if batch_join { 8 } else { 6 }],
            "Native Join integration",
            &[
                "cargo",
                "test",
                "-p",
                "codex-core",
                "--test",
                "all",
                "multi_agent_join",
                "--",
                "--nocapture",
            ],
            &[],
        );
        let background_exit_test_matches = !p3_isolates_background_exit
            || step_matches(
                &self.tests[13 + transport_fallback_offset + csa_orbit_offset],
                "TUI background exit isolation",
                &[
                    "cargo",
                    "test",
                    "-p",
                    "codex-tui",
                    "--lib",
                    "app::tests::background_exit_tests::exit_interrupts_before_requesting_shutdown",
                    "--",
                    "--test-threads=1",
                    "--format=terse",
                ],
                &[],
            );
        let complete_tui_test_matches = !p3
            || if p3_isolates_background_exit {
                step_matches(
                    &self.tests[13 + p3_tui_offset + transport_fallback_offset + csa_orbit_offset],
                    "complete TUI library",
                    &[
                        "cargo",
                        "test",
                        "-p",
                        "codex-tui",
                        "--lib",
                        "--",
                        "--skip",
                        "app::tests::background_exit_tests::exit_interrupts_before_requesting_shutdown",
                        "--format=terse",
                    ],
                    &[],
                )
            } else {
                step_matches(
                    &self.tests[13],
                    "complete TUI library",
                    &[
                        "cargo",
                        "test",
                        "-p",
                        "codex-tui",
                        "--lib",
                        "--",
                        "--format=terse",
                    ],
                    &[],
                )
            };
        let csa_orbit_test_matches = !csa_orbit
            || step_matches(
                &self.tests[13 + transport_fallback_offset],
                "CSA lossless Orbit",
                &[
                    "cargo",
                    "test",
                    "-p",
                    "codex-tui",
                    "--lib",
                    "csa_",
                    "--",
                    "--test-threads=1",
                    "--format=terse",
                ],
                &[],
            );
        let p3_tests_match = !p3
            || (step_matches(
                &self.tests[0],
                "workspace formatting",
                &["cargo", "fmt", "--all", "--", "--check"],
                &[],
            ) && step_matches(
                &self.tests[12 + transport_fallback_offset],
                "TUI live state and panel",
                &[
                    "cargo",
                    "test",
                    "-p",
                    "codex-tui",
                    "--lib",
                    "subagent_live",
                    "--",
                    "--test-threads=1",
                    "--format=terse",
                ],
                &[],
            ) && background_exit_test_matches
                && complete_tui_test_matches
                && csa_orbit_test_matches
                && step_matches(
                    &self.tests[14 + p3_tui_offset + transport_fallback_offset + csa_orbit_offset],
                    "TUI clippy",
                    &[
                        "cargo",
                        "clippy",
                        "-p",
                        "codex-tui",
                        "--lib",
                        "--tests",
                        "--",
                        "-D",
                        "warnings",
                    ],
                    &[],
                ));
        let overlay_test_matches = !matches!(manifest.patch_set_version, 4..=9)
            || step_matches(
                &self.tests[15 + p3_tui_offset + transport_fallback_offset + csa_orbit_offset],
                "CSA official runtime overlay",
                &[
                    "cargo",
                    "test",
                    "-p",
                    "codex-install-context",
                    "csa_overlay_prefers_owned_files_and_falls_back_to_the_official_package",
                    "--",
                    "--nocapture",
                ],
                &[],
            );
        let state_db_compat_test_matches = !state_db_compat
            || step_matches(
                &self.tests[16 + p3_tui_offset + transport_fallback_offset + csa_orbit_offset],
                "Codex state DB line-ending compatibility",
                &[
                    "cargo",
                    "test",
                    "-p",
                    "codex-state",
                    "migration_line_endings",
                    "--",
                    "--nocapture",
                ],
                &[],
            );
        let build_env_matches = match manifest.patch_set_version {
            1 => ["1", "4"].into_iter().any(|jobs| {
                map_matches(
                    &self.build.env,
                    &[
                        ("CARGO_BUILD_JOBS", jobs),
                        ("CARGO_PROFILE_RELEASE_DEBUG", "0"),
                        ("SOURCE_DATE_EPOCH", "1786063808"),
                    ],
                )
            }),
            2 => map_matches(
                &self.build.env,
                &[
                    ("CARGO_PROFILE_RELEASE_DEBUG", "0"),
                    ("SOURCE_DATE_EPOCH", "1786063808"),
                ],
            ),
            3..=5 => map_matches(
                &self.build.env,
                &[
                    ("CARGO_BUILD_JOBS", "2"),
                    ("CARGO_PROFILE_RELEASE_DEBUG", "0"),
                    ("SOURCE_DATE_EPOCH", "1786063808"),
                ],
            ),
            6..=9 => map_matches(
                &self.build.env,
                &[
                    ("CARGO_BUILD_JOBS", "4"),
                    ("CARGO_PROFILE_RELEASE_DEBUG", "0"),
                    ("SOURCE_DATE_EPOCH", "1786063808"),
                ],
            ),
            _ => false,
        };
        let build_matches = argv_matches(
            &self.build.argv,
            &[
                "cargo",
                "build",
                "-p",
                "codex-cli",
                "--bin",
                "codex",
                "--release",
                "--target",
                &manifest.build_target,
            ],
        ) && build_env_matches;
        if !parameters_match
            || !common_env_matches
            || !generation_matches
            || !branding_matches
            || !batch_tests_match
            || !transport_fallback_test_matches
            || !tests_match
            || !p3_tests_match
            || !overlay_test_matches
            || !state_db_compat_test_matches
            || !build_matches
        {
            return Err(ManagerError::new(
                "invalid_test_contract",
                "test contract changes a required parameter, environment, generation, test, or build gate",
            ));
        }
        Ok(())
    }
}

fn step_matches(actual: &ContractStep, name: &str, argv: &[&str], env: &[(&str, &str)]) -> bool {
    actual.name == name && argv_matches(&actual.argv, argv) && map_matches(&actual.env, env)
}

fn argv_matches(actual: &[String], expected: &[&str]) -> bool {
    actual.len() == expected.len()
        && actual
            .iter()
            .zip(expected)
            .all(|(actual, expected)| actual == expected)
}

fn map_matches(actual: &BTreeMap<String, String>, expected: &[(&str, &str)]) -> bool {
    actual.len() == expected.len()
        && expected
            .iter()
            .all(|(key, value)| actual.get(*key).is_some_and(|actual| actual == value))
}
