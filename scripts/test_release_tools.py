#!/usr/bin/env python3
"""Runnable standard-library checks for release and compatibility tooling."""

from __future__ import annotations

import hashlib
import io
import json
import os
import platform as sys_platform
import shutil
import sys
import tarfile
import tempfile
from pathlib import Path
from unittest.mock import patch


SCRIPTS = Path(__file__).resolve().parent
REPOSITORY = SCRIPTS.parent
sys.path.insert(0, str(SCRIPTS))

from assemble_release_candidate import ReleaseError, assemble, digest, load_matrix  # noqa: E402
from ci_release import build_input, platform_artifact  # noqa: E402
from compatibility_audit import AuditError, check_immutability  # noqa: E402
from compat_release import (  # noqa: E402
    CompatibilityReleaseError,
    blocker_body,
    detect,
    finalize,
    pack,
    port,
    render_manifest,
    stable_version,
)
from run_patch_contract import ContractError, execute_version, load_contract  # noqa: E402


def write_tar(path: Path, members: dict[str, bytes]) -> None:
    with tarfile.open(path, "w:gz") as archive:
        for name, data in sorted(members.items()):
            info = tarfile.TarInfo(name)
            info.size = len(data)
            info.mode = 0o755 if name.endswith(".js") else 0o644
            info.mtime = 0
            archive.addfile(info, io.BytesIO(data))


def package_fixtures(root: Path) -> tuple[Path, Path, Path, Path]:
    matrix, platforms = load_matrix(REPOSITORY)
    version = matrix["manager_version"]
    platform = platforms["win32-x64"]
    manager = root / platform["binary"]
    manager.write_bytes(b"small deterministic manager fixture")
    manager_hash = digest(manager)
    optional = {entry["npm_package"]: version for entry in platforms.values()}
    meta_manifest = {
        "name": "@dslzl/csa",
        "version": version,
        "bin": {"csa": "bin/csa.js"},
        "optionalDependencies": optional,
    }
    meta = root / "dslzl-csa-0.1.0.tgz"
    write_tar(
        meta,
        {
            "package/LICENSE": b"fixture license\n",
            "package/README.md": b"fixture readme\n",
            "package/THIRD_PARTY_NOTICES.md": b"fixture notices\n",
            "package/package.json": json.dumps(meta_manifest).encode(),
            "package/bin/csa.js": b"#!/usr/bin/env node\n",
            "package/platforms.json": b'{"schema":1}\n',
        },
    )
    platform_manifest = {
        "name": platform["npm_package"],
        "version": version,
        "os": [platform["os"]],
        "cpu": [platform["arch"]],
        "csa": {
            "schema": 1,
            "target": platform["target"],
            "binary": f"bin/{platform['binary']}",
            "sha256": manager_hash,
        },
    }
    npm_platform = root / "dslzl-csa-win32-x64-0.1.0.tgz"
    write_tar(
        npm_platform,
        {
            "package/LICENSE": b"fixture license\n",
            "package/README.md": b"fixture readme\n",
            "package/THIRD_PARTY_NOTICES.md": b"fixture notices\n",
            "package/package.json": json.dumps(platform_manifest).encode(),
            f"package/bin/{platform['binary']}": manager.read_bytes(),
        },
    )
    evidence = root / "evidence.json"
    evidence.write_text(
        '{"schema":1,"result":"pass","isolation":{"root":"fixture"}}\n',
        encoding="utf-8",
    )
    return manager, meta, npm_platform, evidence


def release_input(
    root: Path, manager: Path, meta: Path, npm_platform: Path, evidence: Path
) -> Path:
    _, platforms = load_matrix(REPOSITORY)
    results = {}
    for platform_id in platforms:
        results[platform_id] = (
            {
                "status": "pass",
                "manager": str(manager),
                "npm_tarball": str(npm_platform),
                "evidence": str(evidence),
                "reproduce": ["fixture command"],
            }
            if platform_id == "win32-x64"
            else {
                "status": "not_verified",
                "reason": "fixture intentionally covers one platform",
                "reproduce": ["run on the native fixture host"],
            }
        )
    value = {
        "schema": 1,
        "release_version": "0.1.0",
        "source": {"revision": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "ref": None, "repository": None},
        "meta_tarball": str(meta),
        "platforms": results,
        "signing": {"status": "not_available", "reason": "fixture has no signer"},
        "manual_gates": {"production_plug": "not_executed"},
    }
    path = root / "release-input.json"
    path.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")
    return path


def assert_checksums(candidate: Path) -> None:
    for line in (candidate / "SHA256SUMS").read_text(encoding="ascii").splitlines():
        expected, relative = line.split("  ", 1)
        assert digest(candidate / relative).upper() == expected


def test_assembler(root: Path) -> None:
    manager, meta, npm_platform, evidence = package_fixtures(root)
    inputs = release_input(root, manager, meta, npm_platform, evidence)
    first = root / "candidate-one"
    second = root / "candidate-two"
    report = assemble(REPOSITORY, inputs, first)
    assert report["overall_status"] == "not_ready"
    assert report["source"]["status"] == "not_verified"
    assert report["platforms"][0]["status"] == "pass"
    assert "patched_artifacts" not in report
    assert not (first / "patched").exists()
    assert not (first / "payload").exists()
    assert not (first / "compatibility-release.json").exists()
    markdown = (first / "release-readiness.md").read_text(encoding="utf-8")
    assert markdown.index("| `darwin-arm64`") < markdown.index("## Unverified Platforms")
    assert_checksums(first)
    provenance = json.loads((first / "provenance.json").read_bytes())
    assert all(not Path(asset["path"]).is_absolute() for asset in provenance["assets"])
    assemble(REPOSITORY, inputs, second)
    assert digest(first / "source" / "csa-0.1.0.tar.gz") == digest(
        second / "source" / "csa-0.1.0.tar.gz"
    )

    corrupt = root / "corrupt-platform.tgz"
    with tarfile.open(npm_platform, "r:gz") as source:
        manifest = json.loads(source.extractfile("package/package.json").read())
    manifest["csa"]["sha256"] = "0" * 64
    write_tar(
        corrupt,
        {
            "package/package.json": json.dumps(manifest).encode(),
            "package/bin/csa.exe": manager.read_bytes(),
        },
    )
    corrupt_input = json.loads(inputs.read_bytes())
    corrupt_input["platforms"]["win32-x64"]["npm_tarball"] = str(corrupt)
    corrupt_input_path = root / "corrupt-input.json"
    corrupt_input_path.write_text(json.dumps(corrupt_input), encoding="utf-8")
    corrupt_output = root / "candidate-corrupt"
    try:
        assemble(REPOSITORY, corrupt_input_path, corrupt_output)
    except ReleaseError:
        pass
    else:
        raise AssertionError("corrupt npm binary binding was accepted")
    assert not corrupt_output.exists()


def test_ci_input(root: Path) -> None:
    fixture = root / "ci-fixture"
    fixture.mkdir()
    manager, meta, npm_platform, _ = package_fixtures(fixture)
    artifact_root = root / "ci-artifacts"
    artifact_root.mkdir()
    isolated = {name: root / name for name in ["home", "codex-home", "npm-prefix", "activation"]}
    for path in isolated.values():
        path.mkdir()
    with patch.dict(
        os.environ,
        {
            "RUNNER_TEMP": str(root),
            "HOME": str(isolated["home"]),
            "CODEX_HOME": str(isolated["codex-home"]),
            "NPM_CONFIG_PREFIX": str(isolated["npm-prefix"]),
        },
    ):
        evidence = platform_artifact(
            REPOSITORY,
            "win32-x64",
            manager,
            meta,
            npm_platform,
            root,
            artifact_root / "win32-x64",
        )
    assert evidence["isolation"]["ephemeral_runner"] is True
    assert evidence["isolation"]["activation"] == str(isolated["activation"].resolve())
    output = root / "ci-release-input.json"
    value = build_input(
        REPOSITORY,
        artifact_root,
        output,
        "a" * 40,
        "refs/tags/v0.1.0",
        "https://example.invalid/csa",
    )
    assert value["platforms"]["win32-x64"]["status"] == "pass"
    assert value["platforms"]["linux-x64"]["status"] == "not_verified"
    assert "patched_artifacts" not in value


def test_immutability(root: Path) -> None:
    source = REPOSITORY / "payload" / "codex" / "rust-v0.147.0-native-join-p1"
    empty = root / "empty-baseline"
    candidate = root / "candidate-payload"
    empty.mkdir()
    candidate.mkdir()
    shutil.copytree(source, candidate / source.name)
    report = check_immutability(empty, candidate)
    assert report["added_entries"][0]["compat_id"] == source.name

    baseline = root / "baseline-payload"
    shutil.copytree(candidate, baseline)
    assert check_immutability(baseline, candidate)["result"] == "pass"
    with (candidate / source.name / "manifest.toml").open("a", encoding="utf-8") as manifest:
        manifest.write("\n")
    try:
        check_immutability(baseline, candidate)
    except AuditError:
        pass
    else:
        raise AssertionError("mutation of an immutable entry was accepted")


def test_contract_shape() -> None:
    payload = REPOSITORY / "payload" / "codex" / "rust-v0.148.0-native-join-p1"
    contract = load_contract(payload / "test-contract.json", payload.name)
    assert len(contract["generation"]) == 2
    assert len(contract["tests"]) == 7
    assert contract["build"]["env"]["CARGO_BUILD_JOBS"] == "4"
    expected = f"Python {sys_platform.python_version()}"
    execution = execute_version(Path(sys.executable).resolve(), expected)
    assert Path(execution["argv"][0]).is_absolute()
    try:
        execute_version(Path(sys.executable).resolve(), "wrong version")
    except ContractError:
        pass
    else:
        raise AssertionError("absolute-path version mismatch was accepted")


def test_release_stream_contracts() -> None:
    watcher = (REPOSITORY / ".github" / "workflows" / "watch-codex-release.yml").read_text(
        encoding="utf-8"
    )
    assert "GIT_CONFIG_KEY_0: core.autocrlf" in watcher
    assert watcher.count('cron: "0 * * * *"') == 1
    assert watcher.count("Codex source must not live inside the CSA repository") == 1
    assert watcher.count('--branch "$env:UPSTREAM_TAG" --single-branch') == 1
    assert 'git add -- "payload/codex/$env:COMPAT_ID"' in watcher

    patched_workflow = (
        REPOSITORY / ".github" / "workflows" / "release-patched-codex.yml"
    ).read_text(encoding="utf-8")
    assert patched_workflow.count("Codex source must not live inside the CSA repository") == 1
    assert 'default: "rust-v0.148.0-native-join-p1"' in patched_workflow

    manager_workflow = (
        REPOSITORY / ".github" / "workflows" / "release-csa.yml"
    ).read_text(
        encoding="utf-8"
    )
    assert "GIT_CONFIG_KEY_0: core.autocrlf" in manager_workflow
    assert manager_workflow.count("needs: validate") == 2
    assert "needs: [validate, quality, build]" in manager_workflow
    assert "csa-release-${{ matrix.id }}" in manager_workflow

    cache_action = (
        REPOSITORY / ".github" / "actions" / "setup-codex-rust-cache" / "action.yml"
    ).read_text(encoding="utf-8")
    assert "CARGO_BUILD_JOBS=$([Environment]::ProcessorCount)" in cache_action
    assert "steps.cargo-home.outputs.day" not in cache_action
    assert "inputs.target" not in cache_action
    assert "inputs.profile" not in cache_action

    ci_workflow = (REPOSITORY / ".github" / "workflows" / "ci.yml").read_text(
        encoding="utf-8"
    )
    assert 'branches: ["main"]' in ci_workflow
    assert "cancel-in-progress: true" in ci_workflow
    for workflow, expected in (
        (ci_workflow, 2),
        (manager_workflow, 1),
        (patched_workflow, 1),
        (watcher, 2),
    ):
        assert workflow.count("retention-days: 1") == expected
    schema = json.loads(
        (REPOSITORY / "release" / "release-inputs.schema.json").read_bytes()
    )
    assert "patched_artifacts" not in schema["required"]
    assert "patched_artifacts" not in schema["properties"]


def git(root: Path, *args: str) -> str:
    result = __import__("subprocess").run(
        ["git", *args], cwd=root, check=True, capture_output=True, text=True
    )
    return result.stdout.strip()


def port_fixture(root: Path) -> tuple[Path, Path, str]:
    source = root / "upstream"
    source.mkdir()
    git(source, "init", "-q")
    git(source, "config", "user.name", "CSA Test")
    git(source, "config", "user.email", "csa@example.invalid")
    (source / "codex-rs" / "core" / "src").mkdir(parents=True)
    (source / "codex-rs" / "Cargo.toml").write_text(
        '[workspace]\n[workspace.package]\nversion = "2.0.0"\n', encoding="utf-8"
    )
    patches = []
    present = {}
    payload = root / "base" / "rust-v1.0.0-native-join-p1"
    (payload / "patches").mkdir(parents=True)
    for index in range(1, 6):
        relative = f"codex-rs/core/src/layer_{index}.rs"
        original = f"old_{index}\n"
        (source / relative).write_text(original, encoding="utf-8")
        present[relative] = hashlib.sha256(original.encode()).hexdigest()
        patch_bytes = (
            f"diff --git a/{relative} b/{relative}\n"
            f"--- a/{relative}\n+++ b/{relative}\n@@ -1 +1 @@\n-old_{index}\n+new_{index}\n"
        ).encode()
        relative_patch = f"patches/000{index}-layer-{index}.patch"
        (payload / relative_patch).write_bytes(patch_bytes)
        patches.append(
            {"path": relative_patch, "sha256": hashlib.sha256(patch_bytes).hexdigest()}
        )
    git(source, "add", ".")
    git(source, "commit", "-qm", "fixture")
    git(source, "tag", "-a", "rust-v2.0.0", "-m", "fixture release")
    commit = git(source, "rev-parse", "HEAD")
    source_hashes = {
        "schema": 1,
        "algorithm": "sha256",
        "content": "git_blob",
        "commit": "a" * 40,
        "present": present,
        "absent": [],
    }
    source_hash_bytes = json.dumps(source_hashes, indent=2, sort_keys=True).encode() + b"\n"
    (payload / "expected").mkdir()
    (payload / "expected" / "source-hashes.json").write_bytes(source_hash_bytes)
    (payload / "test-contract.json").write_text(
        json.dumps({"schema": 1, "compat_id": payload.name}) + "\n", encoding="utf-8"
    )
    manifest = {
        "schema": 1,
        "compat_id": payload.name,
        "codex_version": "1.0.0",
        "upstream_tag": "rust-v1.0.0",
        "upstream_commit": "a" * 40,
        "patch_api": 1,
        "patch_set_version": 1,
        "rust_toolchain": "1.95.0",
        "rustc_commit": "b" * 40,
        "build_target": "x86_64-pc-windows-msvc",
        "source_hashes": "expected/source-hashes.json",
        "source_hashes_sha256": hashlib.sha256(source_hash_bytes).hexdigest(),
        "preimage_absent": [],
        "patches": patches,
        "preimage": present,
        "artifacts": {
            "x86_64-pc-windows-msvc": {
                "url": "unpublished://fixture/codex.exe",
                "filename": "codex.exe",
                "sha256": "c" * 64,
                "size": 1,
            }
        },
    }
    manifest_path = payload / "manifest.toml"
    manifest_path.write_text(render_manifest(manifest), encoding="utf-8")
    return manifest_path, source, commit


def test_compatibility_release_tools(root: Path) -> None:
    root.mkdir()
    assert stable_version("rust-v2.0.0") == "2.0.0"
    try:
        stable_version("rust-v2.0.0-rc.1")
    except CompatibilityReleaseError:
        pass
    else:
        raise AssertionError("prerelease tag was accepted as stable")

    manifest, source, commit = port_fixture(root)
    candidate = root / "rust-v2.0.0-native-join-p1"
    result = port(manifest.resolve(), source.resolve(), "rust-v2.0.0", commit, candidate.resolve())
    assert result["compat_id"] == candidate.name
    artifact = root / "codex.exe"
    artifact.write_bytes(b"deterministic patched artifact")
    finalized = finalize((candidate / "manifest.toml").resolve(), artifact.resolve())
    assert finalized["artifact"]["sha256"] == hashlib.sha256(artifact.read_bytes()).hexdigest()

    assets = root / "compat-assets"
    packed = pack(
        (candidate / "manifest.toml").resolve(), artifact.resolve(), "d" * 40, assets.resolve()
    )
    assert packed["release_tag"] == f"compat-{candidate.name}"
    descriptor = json.loads((assets / "compatibility-release.json").read_bytes())
    assert descriptor["source_commit"] == "d" * 40
    checksum_names = {
        line.split("  ", 1)[1]
        for line in (assets / "SHA256SUMS").read_text(encoding="ascii").splitlines()
    }
    assert checksum_names == {path.name for path in assets.iterdir() if path.name != "SHA256SUMS"}
    assert all(not name.endswith(".tgz") and not name.startswith("csa-") for name in checksum_names)

    first = blocker_body(
        "",
        "rust-v2.0.0",
        commit,
        "https://example.invalid/run",
        "patch",
        "hunk",
        "payload/codex/rust-v1.0.0-native-join-p1/manifest.toml",
    )
    updated = blocker_body(first, "rust-v2.1.0", "e" * 40, "https://example.invalid/run-2")
    assert updated.count("csa-blocker-target:start") == 1
    assert "rust-v2.1.0" in updated and "hunk" in updated
    assert f"Failed target: `rust-v2.0.0` / `{commit}`" in updated
    assert "python scripts/compat_release.py port" in updated
    assert "[IO.Path]::GetTempPath()" in updated
    assert "$PWD/codex-source" not in updated

    class FakeApi:
        issue_body = ""
        release = None
        pulls = []

        def get(self, path: str, *, optional: bool = False):
            if path.endswith("/releases/latest"):
                return {"tag_name": "rust-v9.8.7", "draft": False, "prerelease": False}
            if "/releases/tags/" in path:
                return self.release
            if "/git/ref/tags/compat-" in path:
                return None
            if "/issues?" in path:
                return (
                    [{"number": 17, "body": self.issue_body}]
                    if self.issue_body != ""
                    else []
                )
            if "/pulls?" in path:
                return self.pulls
            raise AssertionError(f"unexpected fake API path: {path}")

        def peel_tag(self, repository: str, tag: str) -> str:
            if repository == "openai/codex":
                assert tag == "rust-v9.8.7"
                return "f" * 40
            assert repository == "dslzl/CSA" and tag == "compat-rust-v9.8.7-native-join-p1"
            return "d" * 40

    fake = FakeApi()
    detection = detect(REPOSITORY.resolve(), fake)
    assert detection["action"] == "patch"
    fake.issue_body = "failure for an older target"
    detection = detect(REPOSITORY.resolve(), fake)
    assert detection["action"] == "blocked" and detection["issue_needs_update"] is True
    fake.issue_body = f"<!-- csa-upstream: rust-v9.8.7 {'f' * 40} -->"
    detection = detect(REPOSITORY.resolve(), fake)
    assert detection["action"] == "blocked" and detection["issue_needs_update"] is False
    fake.issue_body = ""
    fake.pulls = [{"head": {"ref": "automation/compat-rust-v9.8.7-native-join-p1"}}]
    assert detect(REPOSITORY.resolve(), fake)["action"] == "candidate_open"
    fake.pulls = []
    with patch("compat_release.exact_local_entry", return_value=True):
        assert detect(REPOSITORY.resolve(), fake)["action"] == "publish"
    fake.release = {
        "tag_name": "compat-rust-v9.8.7-native-join-p1",
        "draft": False,
        "prerelease": False,
    }
    assert detect(REPOSITORY.resolve(), fake)["action"] == "released"


def main() -> int:
    with tempfile.TemporaryDirectory(prefix="csa-release-tools-") as directory:
        root = Path(directory)
        test_assembler(root)
        test_ci_input(root)
        test_immutability(root)
        test_contract_shape()
        test_release_stream_contracts()
        test_compatibility_release_tools(root / "compat-release")
    print(
        json.dumps(
            {
                "schema": 1,
                "result": "pass",
                "assembler": "pass",
                "atomic_corruption_rejection": "pass",
                "deterministic_source_bundle": "pass",
                "ci_input": "pass",
                "compatibility_immutability": "pass",
                "patch_contract_shape": "pass",
                "release_stream_contracts": "pass",
                "absolute_path_version_execution": "pass",
                "compatibility_release_pack": "pass",
                "compatibility_port": "pass",
                "upstream_detection": "pass",
                "blocker_issue_body": "pass",
            },
            indent=2,
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
