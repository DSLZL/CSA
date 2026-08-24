#!/usr/bin/env python3
from __future__ import annotations

import base64
import hashlib
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("compat_catalog.py")
TARGET = "x86_64-pc-windows-msvc"
COMPAT = "rust-v9.9.9-native-join-p9"
LEGACY = "rust-v9.8.0-native-join-p8"
RUSTC = "59807616e1fa2540724bfbac14d7976d7e4a3860"
UPSTREAM = "1" * 40
ARTIFACT_SHA = hashlib.sha256(b"accepted-codex").hexdigest()
ARTIFACT_SIZE = len(b"accepted-codex")
SRI = "sha512-" + base64.b64encode(b"x" * 64).decode("ascii")


def sha(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def dump(path: Path, value: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")


class CatalogTests(unittest.TestCase):
    def fixture(self) -> tuple[tempfile.TemporaryDirectory[str], Path]:
        holder: tempfile.TemporaryDirectory[str] = tempfile.TemporaryDirectory()
        root = Path(holder.name)
        (root / "release/build-profiles").mkdir(parents=True)
        (root / "release/runtime-locks").mkdir(parents=True)
        (root / f"release/acceptance/{COMPAT}").mkdir(parents=True)
        (root / f"payload/codex/{COMPAT}").mkdir(parents=True)
        (root / f"payload/codex/{LEGACY}").mkdir(parents=True)

        profile = root / "release/build-profiles/windows-msvc-x64.json"
        dump(
            profile,
            {
                "schema": 1,
                "id": "windows-msvc-x64-v1",
                "host": "x86_64-unknown-linux-gnu",
                "target": TARGET,
                "product": {
                    "cargo_package": "codex-cli",
                    "cargo_bin": "codex",
                    "artifact_filename": "codex.exe",
                },
                "rust": {
                    "toolchain": "1.95.0",
                    "rustc_commit": RUSTC,
                    "rustup_version": "1.29.0",
                    "rustup_init": {
                        "url": "https://static.rust-lang.org/rustup/archive/1.29.0/x86_64-unknown-linux-gnu/rustup-init",
                        "sha256": "3" * 64,
                    },
                },
                "tools": {
                    "cargo_xwin": {
                        "version": "0.23.0",
                        "url": "https://github.com/rust-cross/cargo-xwin/releases/download/v0.23.0/tool.tar.gz",
                        "sha256": "4" * 64,
                        "archive_member": "cargo-xwin",
                    },
                    "sccache": {
                        "version": "0.16.0",
                        "url": "https://github.com/mozilla/sccache/releases/download/v0.16.0/tool.tar.gz",
                        "sha256": "5" * 64,
                        "archive_member": "sccache/sccache",
                    },
                },
                "xwin": {"version": "17", "arch": "x86_64", "variant": "desktop"},
                "llvm": {
                    "version": "21.1.8",
                    "major": 21,
                    "apt_key_url": "https://apt.llvm.org/llvm-snapshot.gpg.key",
                    "apt_key_fingerprint": "FINGERPRINT",
                    "apt_repository": "deb https://apt.llvm.org/noble/ llvm-toolchain-noble-21 main",
                },
                "build": {
                    "cargo_build_jobs": 4,
                    "cargo_incremental": 0,
                    "sccache_cache_size": "4G",
                },
            },
        )

        def write_manifest(compat: str, version: str, artifact_sha: str, size: int, url: str) -> Path:
            path = root / f"payload/codex/{compat}/manifest.toml"
            path.write_text(
                f'''schema = 1
compat_id = "{compat}"
codex_version = "{version}"
upstream_tag = "rust-v{version}"
upstream_commit = "{UPSTREAM}"
rust_toolchain = "1.95.0"
rustc_commit = "{RUSTC}"
build_target = "{TARGET}"

[artifacts."{TARGET}"]
filename = "codex.exe"
sha256 = "{artifact_sha}"
size = {size}
url = "{url}"
''',
                encoding="utf-8",
            )
            return path

        manifest = write_manifest(
            COMPAT,
            "9.9.9",
            ARTIFACT_SHA,
            ARTIFACT_SIZE,
            f"https://github.com/dslzl/CSA/releases/download/compat-{COMPAT}/{COMPAT}--codex.exe",
        )
        legacy_manifest = write_manifest(
            LEGACY,
            "9.8.0",
            "0" * 64,
            1,
            f"unpublished://csa/{LEGACY}/{TARGET}/codex.exe",
        )

        def runtime(compat: str, version: str) -> Path:
            path = root / f"release/runtime-locks/{compat}.json"
            dump(
                path,
                {
                    "schema": 1,
                    "compat_id": compat,
                    "codex_version": version,
                    "target": TARGET,
                    "package": "@openai/codex",
                    "archive_url": f"https://registry.npmjs.org/@openai/codex/-/codex-{version}-win32-x64.tgz",
                    "integrity": SRI,
                    "required_files": [
                        "package/vendor/x86_64-pc-windows-msvc/bin/codex-code-mode-host.exe"
                    ],
                },
            )
            return path

        runtime_current = runtime(COMPAT, "9.9.9")
        runtime_legacy = runtime(LEGACY, "9.8.0")
        acceptance = root / f"release/acceptance/{COMPAT}/{TARGET}.json"
        dump(
            acceptance,
            {
                "schema": 1,
                "status": "accepted",
                "compat_id": COMPAT,
                "target": TARGET,
                "artifact_filename": "codex.exe",
                "artifact_sha256": ARTIFACT_SHA,
                "artifact_size": ARTIFACT_SIZE,
                "manifest_sha256": sha(manifest),
                "build_profile_sha256": sha(profile),
                "runtime_lock_sha256": sha(runtime_current),
                "evidence": {"kind": "unit-test"},
            },
        )
        index = {
            "schema": 1,
            "current": {TARGET: COMPAT},
            "compatibilities": {
                COMPAT: {
                    "lifecycle": "accepted",
                    "build_enabled": True,
                    "release_enabled": True,
                    "manifest": f"payload/codex/{COMPAT}/manifest.toml",
                    "manifest_sha256": sha(manifest),
                    "targets": {
                        TARGET: {
                            "build_profile": "release/build-profiles/windows-msvc-x64.json",
                            "build_profile_sha256": sha(profile),
                            "runtime_lock": f"release/runtime-locks/{COMPAT}.json",
                            "runtime_lock_sha256": sha(runtime_current),
                            "acceptance": f"release/acceptance/{COMPAT}/{TARGET}.json",
                            "acceptance_sha256": sha(acceptance),
                        }
                    },
                },
                LEGACY: {
                    "lifecycle": "legacy",
                    "build_enabled": True,
                    "release_enabled": False,
                    "manifest": f"payload/codex/{LEGACY}/manifest.toml",
                    "manifest_sha256": sha(legacy_manifest),
                    "targets": {
                        TARGET: {
                            "build_profile": "release/build-profiles/windows-msvc-x64.json",
                            "build_profile_sha256": sha(profile),
                            "runtime_lock": f"release/runtime-locks/{LEGACY}.json",
                            "runtime_lock_sha256": sha(runtime_legacy),
                            "acceptance": None,
                            "acceptance_sha256": None,
                        }
                    },
                },
            },
        }
        dump(root / "release/compatibility-index.json", index)
        return holder, root

    def run_catalog(self, root: Path | None, *args: str) -> subprocess.CompletedProcess[str]:
        command = [sys.executable, str(SCRIPT), *args]
        if root is not None:
            command.extend(["--repository", str(root)])
        return subprocess.run(
            command,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )

    def test_current_resolves_and_validates_acceptance(self) -> None:
        holder, root = self.fixture()
        self.addCleanup(holder.cleanup)
        result = self.run_catalog(
            root,
            "resolve",
            "--selector",
            "current",
            "--target",
            TARGET,
            "--require-release",
            "--require-acceptance",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        data = json.loads(result.stdout)
        self.assertEqual(data["compat_id"], COMPAT)
        self.assertEqual(data["artifact_sha256"], ARTIFACT_SHA)
        self.assertEqual(data["accepted_artifact_sha256"], ARTIFACT_SHA)

    def test_complete_catalog_and_legacy_placeholder_validate(self) -> None:
        holder, root = self.fixture()
        self.addCleanup(holder.cleanup)
        result = self.run_catalog(root, "validate")
        self.assertEqual(result.returncode, 0, result.stderr)
        legacy = self.run_catalog(root, "resolve", "--selector", LEGACY, "--target", TARGET)
        self.assertEqual(legacy.returncode, 0, legacy.stderr)
        data = json.loads(legacy.stdout)
        self.assertEqual(data["artifact_sha256"], "0" * 64)
        self.assertFalse(data["release_enabled"])

    def test_unknown_selector_fails_closed(self) -> None:
        holder, root = self.fixture()
        self.addCleanup(holder.cleanup)
        result = self.run_catalog(root, "resolve", "--selector", "unknown", "--target", TARGET)
        self.assertEqual(result.returncode, 2)
        self.assertIn("unknown compatibility selector", result.stderr)

    def test_hash_binding_drift_fails_closed(self) -> None:
        holder, root = self.fixture()
        self.addCleanup(holder.cleanup)
        runtime = root / f"release/runtime-locks/{COMPAT}.json"
        runtime.write_text(runtime.read_text(encoding="utf-8") + "\n", encoding="utf-8")
        result = self.run_catalog(root, "resolve", "--selector", COMPAT, "--target", TARGET)
        self.assertEqual(result.returncode, 2)
        self.assertIn("runtime-lock binding drifted", result.stderr)

    def test_runtime_lock_rejects_unknown_fields_after_hash_rebind(self) -> None:
        holder, root = self.fixture()
        self.addCleanup(holder.cleanup)
        runtime = root / f"release/runtime-locks/{LEGACY}.json"
        value = json.loads(runtime.read_text(encoding="utf-8"))
        value["unexpected"] = True
        dump(runtime, value)
        index_path = root / "release/compatibility-index.json"
        index = json.loads(index_path.read_text(encoding="utf-8"))
        index["compatibilities"][LEGACY]["targets"][TARGET]["runtime_lock_sha256"] = sha(runtime)
        dump(index_path, index)
        result = self.run_catalog(root, "resolve", "--selector", LEGACY, "--target", TARGET)
        self.assertEqual(result.returncode, 2)
        self.assertIn("contains unknown keys", result.stderr)

    def test_build_profile_rejects_unsafe_archive_member(self) -> None:
        holder, root = self.fixture()
        self.addCleanup(holder.cleanup)
        profile = root / "release/build-profiles/windows-msvc-x64.json"
        value = json.loads(profile.read_text(encoding="utf-8"))
        value["tools"]["cargo_xwin"]["archive_member"] = "../cargo-xwin"
        dump(profile, value)
        index_path = root / "release/compatibility-index.json"
        index = json.loads(index_path.read_text(encoding="utf-8"))
        index["compatibilities"][LEGACY]["targets"][TARGET]["build_profile_sha256"] = sha(profile)
        dump(index_path, index)
        result = self.run_catalog(root, "resolve", "--selector", LEGACY, "--target", TARGET)
        self.assertEqual(result.returncode, 2)
        self.assertIn("safe relative archive path", result.stderr)

    def test_candidate_record_is_bound_to_resolution_and_artifact(self) -> None:
        holder, root = self.fixture()
        self.addCleanup(holder.cleanup)
        resolution = root / "resolution.json"
        resolved = self.run_catalog(
            root,
            "resolve",
            "--selector",
            LEGACY,
            "--target",
            TARGET,
            "--output",
            str(resolution),
        )
        self.assertEqual(resolved.returncode, 0, resolved.stderr)
        artifact = root / "codex.exe"
        artifact.write_bytes(b"candidate")
        output = root / "candidate.json"
        result = self.run_catalog(
            root,
            "candidate",
            "--resolution",
            str(resolution),
            "--artifact",
            str(artifact),
            "--output",
            str(output),
            "--provider",
            "local",
            "--source-commit",
            "a" * 40,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        record = json.loads(output.read_text(encoding="utf-8"))
        self.assertEqual(record["compat_id"], LEGACY)
        self.assertEqual(record["artifact"]["sha256"], hashlib.sha256(b"candidate").hexdigest())

    def test_list_is_data_driven_and_does_not_expand_in_yaml(self) -> None:
        holder, root = self.fixture()
        self.addCleanup(holder.cleanup)
        result = self.run_catalog(root, "list", "--target", TARGET)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.splitlines(), sorted([COMPAT, LEGACY]))
        release = self.run_catalog(root, "list", "--target", TARGET, "--release")
        self.assertEqual(release.returncode, 0, release.stderr)
        self.assertEqual(release.stdout.strip(), COMPAT)

    def test_workflow_guard_rejects_compatibility_authority(self) -> None:
        holder, root = self.fixture()
        self.addCleanup(holder.cleanup)
        clean = root / "clean.yml"
        clean.write_text("selector: current\n", encoding="utf-8")
        okay = self.run_catalog(None, "guard-workflows", str(clean))
        self.assertEqual(okay.returncode, 0, okay.stderr)
        bad = root / "bad.yml"
        bad.write_text("compat: rust-v9.9.9-native-join-p9\n", encoding="utf-8")
        rejected = self.run_catalog(None, "guard-workflows", str(bad))
        self.assertEqual(rejected.returncode, 2)
        self.assertIn("repeats compatibility authority", rejected.stderr)


if __name__ == "__main__":
    unittest.main()
