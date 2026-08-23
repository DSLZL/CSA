use csa::BUILD_TARGET;
use csa::activation::{forward_shim, plug, purge, select_shim_target, shim_path, unplug};
use csa::compat::LoadedCompatibility;
use csa::error::Result;
use csa::hash::sha256_bytes;
use csa::isolation::IsolationRequest;
use csa::manager::{
    ExecOptions, InstallOptions, OfflineArtifactProvider, PrepareOptions, exec, install, prepare,
    status, uninstall,
};
use csa::process::{CommandResult, CommandSpec, ProcessRunner};
use csa::state::{Clock, ManagerPaths, PrepareLock};
use serde_json::Value;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

const VERSION: &str = "1.2.3";
const COMMIT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const RUSTC_COMMIT: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const TOOLCHAIN: &str = "1.95.0";

static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "csa-test-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn join(&self, path: impl AsRef<Path>) -> PathBuf {
        self.0.join(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Fixture {
    manifest: PathBuf,
    official: PathBuf,
    native: PathBuf,
    #[cfg(windows)]
    official_package: PathBuf,
    #[cfg(windows)]
    managed_package: PathBuf,
    artifact: PathBuf,
    source: PathBuf,
    artifact_bytes: Vec<u8>,
}

impl Fixture {
    fn new(temp: &TempDir, artifact_bytes: &[u8]) -> Self {
        let payload = temp.join("payload/test-compat");
        let patches = payload.join("patches");
        let expected = payload.join("expected");
        let source = temp.join("upstream");
        fs::create_dir_all(&patches).unwrap();
        fs::create_dir_all(&expected).unwrap();
        fs::create_dir_all(source.join("codex-rs/core/src")).unwrap();
        fs::write(
            source.join("codex-rs/Cargo.toml"),
            format!("[workspace]\n[workspace.package]\nversion = \"{VERSION}\"\n"),
        )
        .unwrap();

        let mut present = BTreeMap::new();
        let mut patch_entries = Vec::new();
        for index in 1..=5 {
            let relative = format!("codex-rs/core/src/payload_{index}.rs");
            let original = format!("old_{index}\n");
            fs::write(source.join(&relative), &original).unwrap();
            present.insert(relative.clone(), sha256_bytes(original.as_bytes()));
            let patch = format!(
                "diff --git a/{relative} b/{relative}\n--- a/{relative}\n+++ b/{relative}\n@@ -1 +1 @@\n-old_{index}\n+new_{index}\n"
            );
            let name = format!("000{index}-layer-{index}.patch");
            fs::write(patches.join(&name), &patch).unwrap();
            patch_entries.push((format!("patches/{name}"), sha256_bytes(patch.as_bytes())));
        }

        let source_hashes = serde_json::json!({
            "schema": 1,
            "algorithm": "sha256",
            "content": "git_blob",
            "commit": COMMIT,
            "present": present.clone(),
            "absent": [],
        });
        let source_hashes_bytes = serde_json::to_vec_pretty(&source_hashes).unwrap();
        fs::write(expected.join("source-hashes.json"), &source_hashes_bytes).unwrap();

        let artifact_name = if cfg!(windows) { "codex.exe" } else { "codex" };
        let artifact = temp.join(artifact_name);
        write_executable(&artifact, artifact_bytes);
        let artifact_sha = sha256_bytes(artifact_bytes);
        let patch_toml = patch_entries
            .iter()
            .map(|(path, sha)| format!("[[patches]]\npath = \"{path}\"\nsha256 = \"{sha}\"\n"))
            .collect::<Vec<_>>()
            .join("\n");
        let preimage_toml = present
            .iter()
            .map(|(path, sha)| format!("\"{path}\" = \"{sha}\""))
            .collect::<Vec<_>>()
            .join("\n");
        let manifest = payload.join("manifest.toml");
        fs::write(
            &manifest,
            format!(
                "schema = 1\ncompat_id = \"test-compat\"\ncodex_version = \"{VERSION}\"\nupstream_tag = \"rust-v{VERSION}\"\nupstream_commit = \"{COMMIT}\"\npatch_api = 1\npatch_set_version = 1\nrust_toolchain = \"{TOOLCHAIN}\"\nrustc_commit = \"{RUSTC_COMMIT}\"\nbuild_target = \"{BUILD_TARGET}\"\nsource_hashes = \"expected/source-hashes.json\"\nsource_hashes_sha256 = \"{}\"\npreimage_absent = []\n\n{patch_toml}\n[preimage]\n{preimage_toml}\n\n[artifacts.\"{BUILD_TARGET}\"]\nurl = \"unpublished://test/{artifact_name}\"\nfilename = \"{artifact_name}\"\nsha256 = \"{artifact_sha}\"\nsize = {}\n",
                sha256_bytes(&source_hashes_bytes),
                artifact_bytes.len()
            ),
        )
        .unwrap();
        fs::write(
            payload.join("test-contract.json"),
            test_contract(artifact_name),
        )
        .unwrap();

        #[cfg(windows)]
        let (official, native, official_package, managed_package) = {
            let bun_root = temp.join(".bun");
            let managed_package = bun_root.join("install/global/node_modules/@openai/codex");
            let official_package = bun_root
                .join("install/global/node_modules/@openai/codex-win32-x64/vendor")
                .join(BUILD_TARGET);
            fs::create_dir_all(bun_root.join("bin")).unwrap();
            fs::create_dir_all(official_package.join("bin")).unwrap();
            fs::create_dir_all(official_package.join("codex-resources")).unwrap();
            fs::create_dir_all(official_package.join("codex-path")).unwrap();
            fs::create_dir_all(&managed_package).unwrap();
            fs::write(
                managed_package.join("package.json"),
                format!(r#"{{"name":"@openai/codex","version":"{VERSION}"}}"#),
            )
            .unwrap();
            fs::write(
                official_package.join("codex-package.json"),
                format!(
                    r#"{{"layoutVersion":1,"version":"{VERSION}","target":"{BUILD_TARGET}","variant":"codex","entrypoint":"bin/codex.exe","resourcesDir":"codex-resources","pathDir":"codex-path"}}"#
                ),
            )
            .unwrap();
            for relative in [
                "bin/codex-code-mode-host.exe",
                "codex-resources/codex-command-runner.exe",
                "codex-resources/codex-windows-sandbox-setup.exe",
                "codex-path/rg.exe",
            ] {
                write_executable(&official_package.join(relative), relative.as_bytes());
            }
            let official = bun_root.join("bin/codex.exe");
            let native = official_package.join("bin/codex.exe");
            write_executable(&official, b"official-launcher");
            write_executable(&native, b"official-native");
            (official, native, official_package, managed_package)
        };
        #[cfg(not(windows))]
        let (official, native) = {
            let official = temp.join("official");
            let native = temp.join("official-native");
            write_executable(&official, b"official-launcher");
            write_executable(&native, b"official-native");
            (official, native)
        };
        Self {
            manifest,
            official,
            native,
            #[cfg(windows)]
            official_package,
            #[cfg(windows)]
            managed_package,
            artifact,
            source,
            artifact_bytes: artifact_bytes.to_vec(),
        }
    }

    fn options(&self, manager_root: PathBuf) -> PrepareOptions {
        PrepareOptions {
            manager_root: Some(manager_root),
            official: Some(self.official.clone()),
            official_native: Some(self.native.clone()),
            manifest: self.manifest.clone(),
            artifact: Some(self.artifact.clone()),
            source: None,
        }
    }

    fn set_official_package_version(&self, _version: &str) {
        #[cfg(windows)]
        {
            fs::write(
                self.managed_package.join("package.json"),
                format!(r#"{{"name":"@openai/codex","version":"{_version}"}}"#),
            )
            .unwrap();
            fs::write(
                self.official_package.join("codex-package.json"),
                format!(
                    r#"{{"layoutVersion":1,"version":"{_version}","target":"{BUILD_TARGET}","variant":"codex","entrypoint":"bin/codex.exe","resourcesDir":"codex-resources","pathDir":"codex-path"}}"#
                ),
            )
            .unwrap();
        }
    }

    #[cfg(windows)]
    fn set_official_package_target(&self, target: &str) {
        fs::write(
            self.official_package.join("codex-package.json"),
            format!(
                r#"{{"layoutVersion":1,"version":"{VERSION}","target":"{target}","variant":"codex","entrypoint":"bin/codex.exe","resourcesDir":"codex-resources","pathDir":"codex-path"}}"#
            ),
        )
        .unwrap();
    }
}

#[derive(Clone)]
struct FakeRunner {
    version: Arc<Mutex<String>>,
    child_code: i32,
    source_build: bool,
    fail_build: bool,
    preimage_drift: bool,
    artifact_bytes: Vec<u8>,
    commands: Arc<Mutex<Vec<CommandSpec>>>,
}

impl FakeRunner {
    fn new(artifact_bytes: &[u8]) -> Self {
        Self {
            version: Arc::new(Mutex::new(VERSION.to_owned())),
            child_code: 0,
            source_build: false,
            fail_build: false,
            preimage_drift: false,
            artifact_bytes: artifact_bytes.to_vec(),
            commands: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn source_build(mut self) -> Self {
        self.source_build = true;
        self
    }

    fn child_code(mut self, code: i32) -> Self {
        self.child_code = code;
        self
    }

    fn fail_build(mut self) -> Self {
        self.fail_build = true;
        self
    }

    fn preimage_drift(mut self) -> Self {
        self.preimage_drift = true;
        self
    }

    fn set_version(&self, value: &str) {
        *self.version.lock().unwrap() = value.to_owned();
    }

    fn commands(&self) -> Vec<CommandSpec> {
        self.commands.lock().unwrap().clone()
    }
}

impl ProcessRunner for FakeRunner {
    fn run(&self, command: &CommandSpec) -> Result<CommandResult> {
        self.commands.lock().unwrap().push(command.clone());
        let args: Vec<_> = command
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        if args == ["--version"] {
            return Ok(CommandResult::success(format!(
                "codex-cli {}\n",
                self.version.lock().unwrap()
            )));
        }
        if self.source_build && args == ["run", TOOLCHAIN, "rustc", "--version", "--verbose"] {
            return Ok(CommandResult::success(format!(
                "rustc {TOOLCHAIN}\ncommit-hash: {RUSTC_COMMIT}\nrelease: {TOOLCHAIN}\n"
            )));
        }
        if self.source_build && args.first().map(String::as_str) == Some("clone") {
            let source = PathBuf::from(&args[args.len() - 2]);
            let destination = PathBuf::from(&args[args.len() - 1]);
            copy_tree(&source, &destination);
            return Ok(CommandResult::success(Vec::new()));
        }
        if self.source_build && args.first().map(String::as_str) == Some("-C") {
            return self.fake_git(&args);
        }
        if self.source_build
            && args.starts_with(&["run".to_owned(), TOOLCHAIN.to_owned(), "cargo".to_owned()])
        {
            if args.get(3).map(String::as_str) == Some("build") {
                if self.fail_build {
                    return Ok(CommandResult {
                        code: Some(1),
                        stdout: Vec::new(),
                        stderr: b"simulated build failure".to_vec(),
                    });
                }
                let target = command
                    .env
                    .get(&OsString::from("CARGO_TARGET_DIR"))
                    .map(PathBuf::from)
                    .unwrap();
                let artifact = target
                    .join(BUILD_TARGET)
                    .join("release")
                    .join(if cfg!(windows) { "codex.exe" } else { "codex" });
                fs::create_dir_all(artifact.parent().unwrap()).unwrap();
                write_executable(&artifact, &self.artifact_bytes);
            }
            return Ok(CommandResult::success(Vec::new()));
        }
        Ok(CommandResult {
            code: Some(self.child_code),
            stdout: Vec::new(),
            stderr: Vec::new(),
        })
    }
}

impl FakeRunner {
    fn fake_git(&self, args: &[String]) -> Result<CommandResult> {
        let cwd = PathBuf::from(&args[1]);
        match args.get(2).map(String::as_str) {
            Some("status") | Some("checkout") | Some("read-tree") | Some("apply") => {
                Ok(CommandResult::success(Vec::new()))
            }
            Some("rev-parse") => Ok(CommandResult::success(format!("{COMMIT}\n"))),
            Some("show") => {
                if self.preimage_drift {
                    return Ok(CommandResult::success(b"drifted blob".to_vec()));
                }
                let relative = args[3].split_once(':').unwrap().1;
                Ok(CommandResult::success(
                    fs::read(cwd.join(relative)).unwrap(),
                ))
            }
            Some("cat-file") => Ok(CommandResult {
                code: Some(1),
                stdout: Vec::new(),
                stderr: Vec::new(),
            }),
            value => panic!("unexpected fake git command: {value:?} {args:?}"),
        }
    }
}

struct FixedClock;

impl Clock for FixedClock {
    fn unix_seconds(&self) -> Result<u64> {
        Ok(1_786_063_808)
    }
}

#[cfg(windows)]
#[test]
fn official_runtime_is_discovered_from_the_launcher_and_requires_all_helpers() {
    let temp = TempDir::new();
    let fixture = Fixture::new(&temp, b"patched-binary");
    let runner = FakeRunner::new(&fixture.artifact_bytes);
    let mut options = fixture.options(temp.join("manager"));
    options.official_native = None;
    let report = prepare(options, &runner, &FixedClock, &OfflineArtifactProvider).unwrap();
    let runtime = report.state.official.runtime.unwrap();
    assert_eq!(runtime.files.len(), 6);
    assert_eq!(
        runtime.package_root,
        fixture.official_package.canonicalize().unwrap()
    );
    assert_eq!(
        runtime.managed_package_root,
        fixture.managed_package.canonicalize().unwrap()
    );

    let incomplete = Fixture::new(&temp, b"second-patched-binary");
    fs::remove_file(
        incomplete
            .official_package
            .join("bin/codex-code-mode-host.exe"),
    )
    .unwrap();
    let error = prepare(
        incomplete.options(temp.join("incomplete-manager")),
        &FakeRunner::new(&incomplete.artifact_bytes),
        &FixedClock,
        &OfflineArtifactProvider,
    )
    .unwrap_err();
    assert_eq!(error.code, "official_runtime_incomplete");

    let overlap_root = fixture.official_package.join("manager");
    let error = prepare(
        fixture.options(overlap_root.clone()),
        &runner,
        &FixedClock,
        &OfflineArtifactProvider,
    )
    .unwrap_err();
    assert_eq!(error.code, "official_in_manager_root");
    assert!(!overlap_root.join("state.json").exists());
}

#[test]
fn prebuilt_prepare_status_and_isolated_exec_are_fail_closed() {
    let temp = TempDir::new();
    let fixture = Fixture::new(&temp, b"patched-binary");
    let manager_root = temp.join("manager");
    let runner = FakeRunner::new(&fixture.artifact_bytes).child_code(17);
    let report = prepare(
        fixture.options(manager_root.clone()),
        &runner,
        &FixedClock,
        &OfflineArtifactProvider,
    )
    .unwrap();
    assert_eq!(report.status, "prepared");
    assert!(report.official_unchanged);
    assert!(
        report
            .state
            .artifact_path
            .starts_with(manager_root.canonicalize().unwrap())
    );
    assert_eq!(
        report
            .state
            .artifact_path
            .parent()
            .and_then(Path::file_name),
        Some(std::ffi::OsStr::new("bin"))
    );
    assert_eq!(
        report
            .state
            .artifact_path
            .parent()
            .and_then(Path::parent)
            .and_then(Path::file_name),
        Some(std::ffi::OsStr::new("runtime"))
    );
    assert_eq!(
        fs::read(&report.state.artifact_path).unwrap(),
        fixture.artifact_bytes
    );
    assert!(
        report
            .state
            .manifest_path
            .starts_with(manager_root.canonicalize().unwrap().join("manifests"))
    );
    fs::remove_dir_all(fixture.manifest.parent().unwrap()).unwrap();
    assert_eq!(fs::read(&fixture.official).unwrap(), b"official-launcher");
    assert_eq!(
        status(Some(manager_root.clone()), &runner).unwrap().status,
        "prepared"
    );

    let isolation_root = temp.join("isolated");
    let cwd = isolation_root.join("cwd");
    fs::create_dir_all(&cwd).unwrap();
    let record = isolation_root.join("evidence/exec.json");
    let outcome = exec(
        ExecOptions {
            manager_root: Some(manager_root),
            isolation: IsolationRequest {
                codex_home: isolation_root.join("home"),
                cwd: cwd.clone(),
                logs_dir: isolation_root.join("logs"),
                state_dir: isolation_root.join("state"),
                npm_prefix: Some(isolation_root.join("npm")),
                record_path: record.clone(),
            },
            args: ["--model", "test-model"].map(OsString::from).to_vec(),
        },
        &runner,
    )
    .unwrap();
    assert_eq!(outcome.exit_code, 17);
    assert_eq!(outcome.record.result, "child_exit");
    let record_json: Value = serde_json::from_slice(&fs::read(record).unwrap()).unwrap();
    assert_eq!(record_json["path_prefix"], Value::Null);
    assert_eq!(record_json["result"], "child_exit");

    let child = runner
        .commands()
        .into_iter()
        .find(|command| command.args == ["--model", "test-model"].map(OsString::from))
        .unwrap();
    assert!(child.inherit_stdio);
    assert_eq!(child.cwd, Some(cwd.canonicalize().unwrap()));
    assert_eq!(
        child.env.get(&OsString::from("CODEX_HOME")),
        Some(
            &isolation_root
                .join("home")
                .canonicalize()
                .unwrap()
                .into_os_string()
        )
    );
    assert!(!child.env.contains_key(&OsString::from("PATH")));
    #[cfg(windows)]
    {
        assert_eq!(
            child.env.get(&OsString::from("CODEX_MANAGED_BY_BUN")),
            Some(&OsString::from("1"))
        );
        assert_eq!(
            child.env.get(&OsString::from("CODEX_MANAGED_PACKAGE_ROOT")),
            Some(
                &fixture
                    .managed_package
                    .canonicalize()
                    .unwrap()
                    .into_os_string()
            )
        );
        assert_eq!(
            child
                .env
                .get(&OsString::from("CSA_CODEX_OFFICIAL_PACKAGE_ROOT")),
            Some(
                &fixture
                    .official_package
                    .canonicalize()
                    .unwrap()
                    .into_os_string()
            )
        );
        for key in [
            "CODEX_MANAGED_BY_NPM",
            "CODEX_MANAGED_BY_BUN",
            "CODEX_MANAGED_BY_PNPM",
        ] {
            assert!(child.env_remove.contains(&OsString::from(key)));
        }
        assert!(
            !report
                .state
                .artifact_path
                .parent()
                .unwrap()
                .join("codex-code-mode-host.exe")
                .exists()
        );
    }
}

#[test]
fn prepare_rejects_hash_version_and_same_path_without_state() {
    let temp = TempDir::new();
    let fixture = Fixture::new(&temp, b"expected-patched");
    fs::write(&fixture.artifact, b"wrong-patched").unwrap();
    let root = temp.join("manager-hash");
    let runner = FakeRunner::new(&fixture.artifact_bytes);
    let error = prepare(
        fixture.options(root.clone()),
        &runner,
        &FixedClock,
        &OfflineArtifactProvider,
    )
    .unwrap_err();
    assert_eq!(error.code, "artifact_hash_mismatch");
    assert!(!root.join("state.json").exists());

    write_executable(&fixture.artifact, &fixture.artifact_bytes);
    let cached_root = temp.join("manager-cached");
    prepare(
        fixture.options(cached_root.clone()),
        &runner,
        &FixedClock,
        &OfflineArtifactProvider,
    )
    .unwrap();
    let cached_state = fs::read(cached_root.join("state.json")).unwrap();
    fs::write(&fixture.artifact, b"wrong-patched").unwrap();
    let error = prepare(
        fixture.options(cached_root.clone()),
        &runner,
        &FixedClock,
        &OfflineArtifactProvider,
    )
    .unwrap_err();
    assert_eq!(error.code, "artifact_hash_mismatch");
    assert_eq!(
        fs::read(cached_root.join("state.json")).unwrap(),
        cached_state
    );

    let version_root = temp.join("manager-version");
    runner.set_version("9.9.9");
    fixture.set_official_package_version("9.9.9");
    let error = prepare(
        fixture.options(version_root.clone()),
        &runner,
        &FixedClock,
        &OfflineArtifactProvider,
    )
    .unwrap_err();
    assert_eq!(error.code, "unsupported_official_version");
    assert!(!version_root.join("state.json").exists());
    fixture.set_official_package_version(VERSION);

    #[cfg(windows)]
    {
        let target_root = temp.join("manager-target");
        fixture.set_official_package_target("aarch64-pc-windows-msvc");
        let error = prepare(
            fixture.options(target_root.clone()),
            &runner,
            &FixedClock,
            &OfflineArtifactProvider,
        )
        .unwrap_err();
        assert_eq!(error.code, "official_runtime_incomplete");
        assert!(!target_root.join("state.json").exists());
        fixture.set_official_package_target(BUILD_TARGET);
    }

    let same = Fixture::new(&temp, b"official-launcher");
    let same_root = temp.join("manager-same");
    let same_runner = FakeRunner::new(&same.artifact_bytes);
    let mut options = same.options(same_root.clone());
    options.artifact = Some(same.official.clone());
    let error = prepare(options, &same_runner, &FixedClock, &OfflineArtifactProvider).unwrap_err();
    assert_eq!(error.code, "official_patched_same_path");
    assert!(!same_root.join("state.json").exists());

    runner.set_version(VERSION);
    let error = prepare(
        fixture.options(temp.0.clone()),
        &runner,
        &FixedClock,
        &OfflineArtifactProvider,
    )
    .unwrap_err();
    assert_eq!(error.code, "official_in_manager_root");
}

#[test]
fn status_recovers_previous_state_and_detects_official_drift() {
    let temp = TempDir::new();
    let fixture = Fixture::new(&temp, b"patched-binary");
    let root = temp.join("manager");
    let runner = FakeRunner::new(&fixture.artifact_bytes);
    prepare(
        fixture.options(root.clone()),
        &runner,
        &FixedClock,
        &OfflineArtifactProvider,
    )
    .unwrap();
    fs::rename(root.join("state.json"), root.join("state.json.previous")).unwrap();
    assert_eq!(
        status(Some(root.clone()), &runner).unwrap().status,
        "prepared"
    );
    assert!(root.join("state.json").exists());

    fs::write(&fixture.official, b"mutated-official").unwrap();
    let report = status(Some(root), &runner).unwrap();
    assert_eq!(report.status, "invalidated");
    assert!(report.reason.unwrap().contains("official_invalidated"));
}

#[test]
fn prepare_lock_is_exclusive() {
    let temp = TempDir::new();
    let fixture = Fixture::new(&temp, b"patched-binary");
    let root = temp.join("manager");
    let paths = ManagerPaths::resolve(Some(root.clone())).unwrap();
    let _lock = PrepareLock::acquire(&paths).unwrap();
    let error = prepare(
        fixture.options(root),
        &FakeRunner::new(&fixture.artifact_bytes),
        &FixedClock,
        &OfflineArtifactProvider,
    )
    .unwrap_err();
    assert_eq!(error.code, "prepare_locked");
}

#[test]
fn source_prepare_runs_exact_patch_generation_test_and_build_sequence() {
    let temp = TempDir::new();
    let fixture = Fixture::new(&temp, b"source-built-patched");
    let root = temp.join("manager");
    let runner = FakeRunner::new(&fixture.artifact_bytes).source_build();
    let mut options = fixture.options(root.clone());
    options.artifact = None;
    options.source = Some(fixture.source.clone());
    let report = prepare(options, &runner, &FixedClock, &OfflineArtifactProvider).unwrap();
    assert_eq!(report.status, "prepared");
    assert_eq!(
        fs::read(report.state.artifact_path).unwrap(),
        fixture.artifact_bytes
    );
    let commands = runner.commands();
    let cargo_commands: Vec<_> = commands
        .iter()
        .filter(|command| {
            let args: Vec<_> = command
                .args
                .iter()
                .map(|arg| arg.to_string_lossy())
                .collect();
            args.get(2).is_some_and(|arg| arg == "cargo")
        })
        .collect();
    assert_eq!(cargo_commands.len(), 10);
    assert!(cargo_commands.iter().all(|command| {
        command
            .env
            .contains_key(&OsString::from("CARGO_TARGET_DIR"))
    }));
    assert!(commands.iter().all(|command| {
        let program = command.program.file_name().unwrap().to_string_lossy();
        !program.eq_ignore_ascii_case("cmd.exe")
            && !program.eq_ignore_ascii_case("powershell.exe")
            && !program.eq_ignore_ascii_case("pwsh.exe")
    }));
}

#[test]
fn source_prepare_rejects_preimage_drift_and_build_failure_without_state() {
    let temp = TempDir::new();
    let fixture = Fixture::new(&temp, b"source-built-patched");

    let drift_root = temp.join("manager-drift");
    let mut drift_options = fixture.options(drift_root.clone());
    drift_options.artifact = None;
    drift_options.source = Some(fixture.source.clone());
    let error = prepare(
        drift_options,
        &FakeRunner::new(&fixture.artifact_bytes)
            .source_build()
            .preimage_drift(),
        &FixedClock,
        &OfflineArtifactProvider,
    )
    .unwrap_err();
    assert_eq!(error.code, "preimage_hash_mismatch");
    assert!(!drift_root.join("state.json").exists());

    let build_root = temp.join("manager-build-failure");
    let mut build_options = fixture.options(build_root.clone());
    build_options.artifact = None;
    build_options.source = Some(fixture.source.clone());
    let error = prepare(
        build_options,
        &FakeRunner::new(&fixture.artifact_bytes)
            .source_build()
            .fail_build(),
        &FixedClock,
        &OfflineArtifactProvider,
    )
    .unwrap_err();
    assert_eq!(error.code, "command_failed");
    assert!(!build_root.join("state.json").exists());
    assert_eq!(fs::read(&fixture.official).unwrap(), b"official-launcher");
}

#[test]
fn isolated_exec_rejects_manager_and_codex_home_overlap_before_launch() {
    let temp = TempDir::new();
    let fixture = Fixture::new(&temp, b"patched-binary");
    let root = temp.join("manager");
    let runner = FakeRunner::new(&fixture.artifact_bytes);
    prepare(
        fixture.options(root.clone()),
        &runner,
        &FixedClock,
        &OfflineArtifactProvider,
    )
    .unwrap();
    let cwd = temp.join("fixture-cwd");
    fs::create_dir_all(&cwd).unwrap();
    let record = temp.join("unsafe-record.json");
    let before = runner.commands().len();
    let error = exec(
        ExecOptions {
            manager_root: Some(root.clone()),
            isolation: IsolationRequest {
                codex_home: root,
                cwd,
                logs_dir: temp.join("unsafe-logs"),
                state_dir: temp.join("unsafe-state"),
                npm_prefix: None,
                record_path: record.clone(),
            },
            args: Vec::new(),
        },
        &runner,
    )
    .unwrap_err();
    assert_eq!(error.code, "shared_isolation_path");
    assert!(!record.exists());
    assert_eq!(runner.commands().len(), before + 2);
}

#[test]
fn activation_lifecycle_is_reversible_and_drift_falls_back_without_recursion() {
    let temp = TempDir::new();
    let fixture = Fixture::new(&temp, b"patched-binary");
    let root = temp.join("manager");
    let runner = FakeRunner::new(&fixture.artifact_bytes).child_code(23);
    prepare(
        fixture.options(root.clone()),
        &runner,
        &FixedClock,
        &OfflineArtifactProvider,
    )
    .unwrap();
    let manager = temp.join(if cfg!(windows) { "csa.exe" } else { "csa" });
    write_executable(&manager, b"manager-forwarder");

    let error = plug(Some(root.clone()), &runner, &FixedClock, &fixture.official).unwrap_err();
    assert_eq!(error.code, "unsafe_shim_source");

    let report = plug(Some(root.clone()), &runner, &FixedClock, &manager).unwrap();
    assert!(report.changed);
    assert_eq!(report.activation.status, "plugged");
    let paths = ManagerPaths::resolve(Some(root.clone())).unwrap();
    let shim = shim_path(&paths);
    assert!(shim.exists());
    assert!(paths.active.exists());
    let prepared_status = status(Some(root.clone()), &runner).unwrap();
    assert_eq!(prepared_status.activation.status, "plugged");
    let prepared_artifact = prepared_status.state.unwrap().artifact_path;
    assert!(
        !plug(Some(root.clone()), &runner, &FixedClock, &manager)
            .unwrap()
            .changed
    );

    let code = forward_shim(
        &paths,
        ["--probe", "value"].map(OsString::from).to_vec(),
        None,
        &shim,
        &runner,
    )
    .unwrap();
    assert_eq!(code, 23);
    let forwarded = runner
        .commands()
        .into_iter()
        .find(|command| command.args == ["--probe", "value"].map(OsString::from))
        .unwrap();
    assert_eq!(forwarded.program, prepared_artifact);
    assert!(forwarded.inherit_stdio);
    assert_eq!(forwarded.cwd, None);
    #[cfg(windows)]
    {
        assert_eq!(
            forwarded.env.get(&OsString::from("CODEX_MANAGED_BY_BUN")),
            Some(&OsString::from("1"))
        );
        assert_eq!(
            forwarded
                .env
                .get(&OsString::from("CSA_CODEX_OFFICIAL_PACKAGE_ROOT")),
            Some(
                &fixture
                    .official_package
                    .canonicalize()
                    .unwrap()
                    .into_os_string()
            )
        );
    }
    #[cfg(not(windows))]
    assert!(forwarded.env.is_empty());

    let fallback_bin = temp.join("official-bin");
    fs::create_dir_all(&fallback_bin).unwrap();
    let fallback = fallback_bin.join(if cfg!(windows) { "codex.exe" } else { "codex" });
    write_executable(&fallback, b"fallback-official");
    let fallback_path = std::env::join_paths([&paths.bin, &fallback_bin]).unwrap();
    let lock = PrepareLock::acquire(&paths).unwrap();
    assert_eq!(
        forward_shim(
            &paths,
            vec![OsString::from("--locked")],
            Some(&fallback_path),
            &shim,
            &runner,
        )
        .unwrap(),
        23
    );
    drop(lock);
    let locked_fallback = runner
        .commands()
        .into_iter()
        .find(|command| command.args == ["--locked"].map(OsString::from))
        .unwrap();
    assert_eq!(locked_fallback.program, fallback.canonicalize().unwrap());

    runner.set_version("9.9.9");
    fixture.set_official_package_version("9.9.9");
    let selection = select_shim_target(&paths, Some(&fallback_path), &shim, &runner).unwrap();
    assert_eq!(selection.mode, "official");
    assert_eq!(selection.target, fallback.canonicalize().unwrap());
    assert!(
        selection
            .fallback_reason
            .unwrap()
            .contains("official_invalidated")
    );
    let drifted = status(Some(root.clone()), &runner).unwrap();
    assert_eq!(drifted.status, "invalidated");
    assert_eq!(drifted.activation.status, "fallback");
    runner.set_version(VERSION);
    fixture.set_official_package_version(VERSION);

    assert!(unplug(Some(root.clone())).unwrap().changed);
    assert!(!shim.exists());
    assert!(!paths.active.exists());
    assert!(!unplug(Some(root.clone())).unwrap().changed);
    assert!(purge(Some(root.clone())).unwrap().changed);
    assert!(!paths.state.exists());
    assert!(!paths.artifacts.exists());
    assert!(!paths.manifests.exists());
    assert!(!paths.downloads.exists());
    assert!(!purge(Some(root)).unwrap().changed);
    assert_eq!(fs::read(&fixture.official).unwrap(), b"official-launcher");
    #[cfg(windows)]
    assert_eq!(
        fs::read(
            fixture
                .official_package
                .join("bin/codex-code-mode-host.exe")
        )
        .unwrap(),
        b"bin/codex-code-mode-host.exe"
    );
}

#[test]
fn cold_install_and_uninstall_are_idempotent_and_rollback_activation_failure() {
    let temp = TempDir::new();
    let fixture = Fixture::new(&temp, b"patched-binary");
    let root = temp.join("manager");
    let manager = temp.join(if cfg!(windows) { "csa.exe" } else { "csa" });
    write_executable(&manager, b"manager-forwarder");
    let sentinel = temp.join("external-sentinel");
    fs::write(&sentinel, b"keep").unwrap();
    let runner = FakeRunner::new(&fixture.artifact_bytes);

    let first = install(
        InstallOptions::Local(fixture.options(root.clone())),
        &runner,
        &FixedClock,
        &OfflineArtifactProvider,
        &manager,
    )
    .unwrap();
    assert_eq!(first.status, "installed");
    assert!(first.activation.changed);
    let second = install(
        InstallOptions::Local(fixture.options(root.clone())),
        &runner,
        &FixedClock,
        &OfflineArtifactProvider,
        &manager,
    )
    .unwrap();
    assert!(!second.activation.changed);

    assert!(uninstall(Some(root.clone())).unwrap().changed);
    assert!(!uninstall(Some(root.clone())).unwrap().changed);
    assert_eq!(fs::read(&sentinel).unwrap(), b"keep");
    assert_eq!(fs::read(&fixture.official).unwrap(), b"official-launcher");

    let error = install(
        InstallOptions::Local(fixture.options(root.clone())),
        &runner,
        &FixedClock,
        &OfflineArtifactProvider,
        &fixture.official,
    )
    .unwrap_err();
    assert_eq!(error.code, "unsafe_shim_source");
    let paths = ManagerPaths::resolve(Some(root.clone())).unwrap();
    assert!(!shim_path(&paths).exists());
    assert!(!paths.active.exists());
    let report = status(Some(root), &runner).unwrap();
    assert_eq!(report.status, "prepared");
    assert_eq!(report.activation.status, "unplugged");
    assert_eq!(fs::read(&fixture.official).unwrap(), b"official-launcher");
}

#[test]
fn interrupted_activation_recovers_to_unplugged() {
    let temp = TempDir::new();
    let fixture = Fixture::new(&temp, b"patched-binary");
    let root = temp.join("manager");
    let runner = FakeRunner::new(&fixture.artifact_bytes);
    prepare(
        fixture.options(root.clone()),
        &runner,
        &FixedClock,
        &OfflineArtifactProvider,
    )
    .unwrap();
    let manager = temp.join(if cfg!(windows) { "csa.exe" } else { "csa" });
    write_executable(&manager, b"manager-forwarder");
    let paths = ManagerPaths::resolve(Some(root.clone())).unwrap();

    plug(Some(root.clone()), &runner, &FixedClock, &manager).unwrap();
    fs::remove_file(shim_path(&paths)).unwrap();
    assert!(unplug(Some(root.clone())).unwrap().changed);
    assert!(!paths.active.exists());

    plug(Some(root.clone()), &runner, &FixedClock, &manager).unwrap();
    fs::remove_file(&paths.active).unwrap();
    assert!(unplug(Some(root)).unwrap().changed);
    assert!(!shim_path(&paths).exists());
}

#[test]
fn schema_one_state_falls_back_and_is_replaced_only_by_reinstall() {
    let temp = TempDir::new();
    let fixture = Fixture::new(&temp, b"patched-binary");
    let root = temp.join("manager");
    let manager = temp.join(if cfg!(windows) { "csa.exe" } else { "csa" });
    write_executable(&manager, b"manager-forwarder");
    let runner = FakeRunner::new(&fixture.artifact_bytes);
    install(
        InstallOptions::Local(fixture.options(root.clone())),
        &runner,
        &FixedClock,
        &OfflineArtifactProvider,
        &manager,
    )
    .unwrap();
    let paths = ManagerPaths::resolve(Some(root.clone())).unwrap();
    let mut state: Value = serde_json::from_slice(&fs::read(&paths.state).unwrap()).unwrap();
    state["schema"] = Value::from(1);
    state["official"].as_object_mut().unwrap().remove("runtime");
    fs::write(&paths.state, serde_json::to_vec_pretty(&state).unwrap()).unwrap();

    let legacy = status(Some(root.clone()), &runner).unwrap();
    assert_eq!(legacy.status, "invalidated");
    assert!(legacy.reason.unwrap().contains("state_upgrade_required"));
    let selection = select_shim_target(&paths, None, &shim_path(&paths), &runner).unwrap();
    assert_eq!(selection.mode, "official");

    install(
        InstallOptions::Local(fixture.options(root.clone())),
        &runner,
        &FixedClock,
        &OfflineArtifactProvider,
        &manager,
    )
    .unwrap();
    let upgraded: Value = serde_json::from_slice(&fs::read(&paths.state).unwrap()).unwrap();
    assert_eq!(upgraded["schema"], Value::from(2));
    assert_eq!(upgraded["official"]["runtime"].is_object(), cfg!(windows));
    assert!(uninstall(Some(root)).unwrap().changed);
}

#[test]
fn manifest_types_are_strict() {
    let temp = TempDir::new();
    let fixture = Fixture::new(&temp, b"patched-binary");
    let text = fs::read_to_string(&fixture.manifest).unwrap();
    fs::write(
        &fixture.manifest,
        text.replacen("schema = 1", "schema = true", 1),
    )
    .unwrap();
    assert_eq!(
        LoadedCompatibility::load(&fixture.manifest)
            .unwrap_err()
            .code,
        "invalid_manifest"
    );

    fs::write(&fixture.manifest, text).unwrap();
    let contract_path = fixture
        .manifest
        .parent()
        .unwrap()
        .join("test-contract.json");
    let contract: Value = serde_json::from_slice(&fs::read(&contract_path).unwrap()).unwrap();

    let mut parallel_env = contract.clone();
    parallel_env["build"]["env"]["CARGO_BUILD_JOBS"] = Value::String("4".to_owned());
    fs::write(
        &contract_path,
        serde_json::to_vec_pretty(&parallel_env).unwrap(),
    )
    .unwrap();
    LoadedCompatibility::load(&fixture.manifest)
        .unwrap()
        .test_contract()
        .unwrap();

    let mut changed_env = contract.clone();
    changed_env["build"]["env"]["CARGO_BUILD_JOBS"] = Value::String("2".to_owned());
    fs::write(
        &contract_path,
        serde_json::to_vec_pretty(&changed_env).unwrap(),
    )
    .unwrap();
    assert_eq!(
        LoadedCompatibility::load(&fixture.manifest)
            .unwrap()
            .test_contract()
            .unwrap_err()
            .code,
        "invalid_test_contract"
    );

    let mut changed_argv = contract;
    changed_argv["build"]["argv"][1] = Value::String("check".to_owned());
    fs::write(
        &contract_path,
        serde_json::to_vec_pretty(&changed_argv).unwrap(),
    )
    .unwrap();
    assert_eq!(
        LoadedCompatibility::load(&fixture.manifest)
            .unwrap()
            .test_contract()
            .unwrap_err()
            .code,
        "invalid_test_contract"
    );
}

#[test]
fn bundled_p2_contract_requires_branding_and_runner_build_jobs() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("payload/codex/rust-v0.148.0-native-join-p2/manifest.toml")
        .canonicalize()
        .unwrap();
    let loaded = LoadedCompatibility::load(&manifest).unwrap();
    let contract = loaded.test_contract().unwrap();

    assert_eq!(loaded.manifest.patch_set_version, 2);
    assert_eq!(loaded.patch_paths.len(), 6);
    assert_eq!(contract.tests.len(), 11);
    assert!(!contract.build.env.contains_key("CARGO_BUILD_JOBS"));
}

#[test]
fn bundled_p3_contract_requires_full_tui_gates() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("payload/codex/rust-v0.149.0-native-join-p3/manifest.toml")
        .canonicalize()
        .unwrap();
    let loaded = LoadedCompatibility::load(&manifest).unwrap();
    let contract = loaded.test_contract().unwrap();

    assert_eq!(loaded.manifest.patch_set_version, 6);
    assert_eq!(loaded.patch_paths.len(), 14);
    assert_eq!(contract.tests.len(), 17);
    assert_eq!(
        contract
            .common_env
            .get("CARGO_BUILD_JOBS")
            .map(String::as_str),
        Some("2")
    );
    assert_eq!(
        contract
            .common_env
            .get("INSTA_WORKSPACE_ROOT")
            .map(String::as_str),
        Some("{source}/codex-rs")
    );
    assert_eq!(
        contract
            .build
            .env
            .get("CARGO_BUILD_JOBS")
            .map(String::as_str),
        Some("4")
    );
}

#[test]
fn family_bindings_resolve_to_the_exact_legacy_p2_payloads() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    for version in ["0.147.0", "0.148.0"] {
        let compat_id = format!("rust-v{version}-native-join-p2");
        let legacy = repository
            .join(format!("payload/codex/{compat_id}/manifest.toml"))
            .canonicalize()
            .unwrap();
        let binding = repository
            .join(format!(
                "payload/codex/native-join-p2/bindings/{compat_id}/manifest.toml"
            ))
            .canonicalize()
            .unwrap();
        let legacy = LoadedCompatibility::load(&legacy).unwrap();
        let binding = LoadedCompatibility::load(&binding).unwrap();

        assert_eq!(binding.family_id(), Some("native-join-p2"));
        assert_eq!(binding.manifest.compat_id, compat_id);
        assert_eq!(
            binding.test_contract().unwrap().tests.len(),
            if version == "0.148.0" { 11 } else { 8 }
        );
        assert_eq!(
            binding.payload_files().unwrap(),
            legacy.payload_files().unwrap()
        );
    }

    let shared = repository
        .join("payload/codex/native-join-p2/shared/patches/0006-csa-version-display.patch")
        .canonicalize()
        .unwrap();
    let binding = LoadedCompatibility::load(
        &repository
            .join(
                "payload/codex/native-join-p2/bindings/rust-v0.148.0-native-join-p2/manifest.toml",
            )
            .canonicalize()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(binding.patch_paths.last(), Some(&shared));
}

fn write_executable(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn test_contract(artifact_name: &str) -> Vec<u8> {
    serde_json::to_vec_pretty(&serde_json::json!({
        "schema": 1,
        "compat_id": "test-compat",
        "parameters": {
            "source": "absolute clean checkout path",
            "cargo_target": "absolute disposable Cargo target path"
        },
        "cwd": "{source}/codex-rs",
        "common_env": {
            "CARGO_INCREMENTAL": "0",
            "CARGO_TARGET_DIR": "{cargo_target}",
            "RUST_MIN_STACK": "8388608"
        },
        "generation": [
            {
                "name": "stable schema and embedded exports",
                "env": {
                    "CODEX_APP_SERVER_SCHEMA_EXPERIMENTAL": "0",
                    "CODEX_APP_SERVER_SCHEMA_ROOT": "{source}/codex-rs/app-server-protocol/schema"
                },
                "argv": ["cargo", "test", "-p", "codex-app-server-protocol", "write_schema_fixtures_from_env", "--", "--ignored", "--nocapture"]
            },
            {
                "name": "experimental embedded exports",
                "env": {
                    "CODEX_APP_SERVER_SCHEMA_EXPERIMENTAL": "1",
                    "CODEX_APP_SERVER_SCHEMA_ROOT": "{source}/codex-rs/app-server-protocol/schema"
                },
                "argv": ["cargo", "test", "-p", "codex-app-server-protocol", "write_schema_fixtures_from_env", "--", "--ignored", "--nocapture"]
            }
        ],
        "tests": [
            {
                "name": "schema reverse-check",
                "env": {"CODEX_APP_SERVER_SCHEMA_ROOT": "{source}/codex-rs/app-server-protocol/schema"},
                "argv": ["cargo", "test", "-p", "codex-app-server-protocol", "schema_fixtures_tests", "--", "--nocapture"]
            },
            {"name": "completion registry", "argv": ["cargo", "test", "-p", "codex-core", "agent::completion::tests", "--", "--nocapture"]},
            {"name": "terminal outcome mapping", "argv": ["cargo", "test", "-p", "codex-core", "agent_run_terminal_mapper_preserves_exact_outcomes", "--", "--nocapture"]},
            {"name": "replayable terminal publication", "argv": ["cargo", "test", "-p", "codex-core", "spawned_v2_terminal_events_publish_replayable_exact_run_outcomes", "--", "--nocapture"]},
            {"name": "Join tool schema", "argv": ["cargo", "test", "-p", "codex-core", "join_agent_tool_requires_exact_run_without_timeout", "--", "--nocapture"]},
            {"name": "invalid Join inputs", "argv": ["cargo", "test", "-p", "codex-core", "multi_agent_v2_join_rejects_invalid_arguments_targets_and_runs", "--", "--nocapture"]},
            {"name": "Native Join integration", "argv": ["cargo", "test", "-p", "codex-core", "--test", "all", "multi_agent_join", "--", "--nocapture"]}
        ],
        "build": {
            "env": {
                "CARGO_BUILD_JOBS": "1",
                "CARGO_PROFILE_RELEASE_DEBUG": "0",
                "SOURCE_DATE_EPOCH": "1786063808"
            },
            "argv": ["cargo", "build", "-p", "codex-cli", "--bin", "codex", "--release", "--target", BUILD_TARGET],
            "artifact": format!("{{cargo_target}}/{BUILD_TARGET}/release/{artifact_name}")
        },
        "known_upstream_errata": []
    }))
    .unwrap()
}
