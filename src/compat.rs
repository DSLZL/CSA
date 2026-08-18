use crate::error::{ManagerError, Result};
use crate::hash::{sha256_bytes, sha256_file};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Deserialize)]
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
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchEntry {
    pub path: String,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactEntry {
    pub url: String,
    pub filename: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Clone, Debug)]
pub struct LoadedCompatibility {
    pub manifest: CompatibilityManifest,
    pub manifest_path: PathBuf,
    pub payload_root: PathBuf,
    pub patch_paths: Vec<PathBuf>,
}

impl LoadedCompatibility {
    pub fn load(manifest_path: &Path) -> Result<Self> {
        if !manifest_path.is_absolute() {
            return Err(ManagerError::new(
                "invalid_manifest_path",
                "manifest path must be absolute",
            ));
        }
        let manifest_path = manifest_path.canonicalize().map_err(|error| {
            ManagerError::io(
                &format!("canonicalize manifest {}", manifest_path.display()),
                error,
            )
        })?;
        let payload_root = manifest_path
            .parent()
            .ok_or_else(|| ManagerError::new("invalid_manifest_path", "manifest has no parent"))?
            .to_path_buf();
        let text = fs::read_to_string(&manifest_path).map_err(|error| {
            ManagerError::io(&format!("read manifest {}", manifest_path.display()), error)
        })?;
        let manifest: CompatibilityManifest = toml::from_str(&text).map_err(|error| {
            ManagerError::new("invalid_manifest", format!("manifest TOML: {error}"))
        })?;
        validate_manifest(&manifest, &payload_root)?;

        let source_hashes_path = join_relative(&payload_root, &manifest.source_hashes)?;
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
            let path = join_relative(&payload_root, &patch.path)?;
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
        })
    }

    pub fn artifact(&self) -> &ArtifactEntry {
        &self.manifest.artifacts[&self.manifest.build_target]
    }

    pub fn test_contract(&self) -> Result<TestContract> {
        let path = self.payload_root.join("test-contract.json");
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

    pub fn payload_files(&self) -> Result<BTreeMap<String, PathBuf>> {
        let mut files = BTreeMap::new();
        insert_payload_file(&mut files, "manifest.toml", self.manifest_path.clone())?;
        insert_payload_file(
            &mut files,
            &self.manifest.source_hashes,
            join_relative(&self.payload_root, &self.manifest.source_hashes)?,
        )?;
        insert_payload_file(
            &mut files,
            "test-contract.json",
            self.payload_root.join("test-contract.json"),
        )?;
        for (entry, path) in self.manifest.patches.iter().zip(&self.patch_paths) {
            insert_payload_file(&mut files, &entry.path, path.clone())?;
        }
        Ok(files)
    }
}

fn insert_payload_file(
    files: &mut BTreeMap<String, PathBuf>,
    relative: &str,
    source: PathBuf,
) -> Result<()> {
    validate_relative(relative, false)?;
    if files.insert(relative.to_owned(), source).is_some() {
        return Err(ManagerError::new(
            "duplicate_payload_path",
            format!("payload path is declared more than once: {relative}"),
        ));
    }
    Ok(())
}

fn validate_manifest(manifest: &CompatibilityManifest, payload_root: &Path) -> Result<()> {
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
    if payload_root.file_name().and_then(|value| value.to_str()) != Some(&manifest.compat_id) {
        return Err(ManagerError::new(
            "compat_path_mismatch",
            "compat_id must equal the payload directory name",
        ));
    }
    validate_relative(&manifest.source_hashes, false)?;

    if manifest.patches.len() != 5 {
        return Err(ManagerError::new(
            "invalid_patch_set",
            "exactly five ordered patches are required",
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

fn join_relative(root: &Path, value: &str) -> Result<PathBuf> {
    validate_relative(value, false)?;
    Ok(root.join(value))
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
        let shape_matches = self.schema == 1
            && self.compat_id == manifest.compat_id
            && self.cwd == "{source}/codex-rs"
            && self.generation.len() == 2
            && self.tests.len() == 7
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

        let parameters_match = map_matches(
            &self.parameters,
            &[
                ("source", "absolute clean checkout path"),
                ("cargo_target", "absolute disposable Cargo target path"),
            ],
        );
        let common_env_matches = map_matches(
            &self.common_env,
            &[
                ("CARGO_INCREMENTAL", "0"),
                ("CARGO_TARGET_DIR", "{cargo_target}"),
                ("RUST_MIN_STACK", "8388608"),
            ],
        );
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
            &self.tests[0],
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
            &self.tests[1],
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
            &self.tests[2],
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
            &self.tests[3],
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
            &self.tests[4],
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
            &self.tests[5],
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
            &self.tests[6],
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
        ) && map_matches(
            &self.build.env,
            &[
                ("CARGO_BUILD_JOBS", "1"),
                ("CARGO_PROFILE_RELEASE_DEBUG", "0"),
                ("SOURCE_DATE_EPOCH", "1786063808"),
            ],
        );
        if !parameters_match
            || !common_env_matches
            || !generation_matches
            || !tests_match
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
