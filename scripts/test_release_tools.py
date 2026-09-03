#!/usr/bin/env python3
"""Runnable standard-library checks for CSA Manager release tooling."""

from __future__ import annotations

import io
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tarfile
import tempfile
from unittest.mock import patch

SCRIPTS = Path(__file__).resolve().parent
REPOSITORY = SCRIPTS.parent
sys.path.insert(0, str(SCRIPTS))

from assemble_release_candidate import ReleaseError, assemble, digest, load_matrix  # noqa: E402
from ci_release import build_input, platform_artifact  # noqa: E402
from generate_release_notes import ReleaseNotesError, generate  # noqa: E402


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

    meta = root / f"dslzl-csa-{version}.tgz"
    write_tar(
        meta,
        {
            "package/LICENSE": b"fixture license\n",
            "package/README.md": b"fixture readme\n",
            "package/THIRD_PARTY_NOTICES.md": b"fixture notices\n",
            "package/package.json": json.dumps(
                {
                    "name": "@dslzl/csa",
                    "version": version,
                    "bin": {"csa": "bin/csa.js"},
                    "optionalDependencies": optional,
                }
            ).encode(),
            "package/bin/csa.js": b"#!/usr/bin/env node\n",
            "package/platforms.json": b'{"schema":1}\n',
        },
    )

    npm_platform = root / f"dslzl-csa-win32-x64-{version}.tgz"
    write_tar(
        npm_platform,
        {
            "package/LICENSE": b"fixture license\n",
            "package/README.md": b"fixture readme\n",
            "package/THIRD_PARTY_NOTICES.md": b"fixture notices\n",
            "package/package.json": json.dumps(
                {
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
            ).encode(),
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
    matrix, platforms = load_matrix(REPOSITORY)
    results = {
        platform_id: (
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
        for platform_id in platforms
    }
    value = {
        "schema": 1,
        "release_version": matrix["manager_version"],
        "source": {
            "revision": "a" * 40,
            "ref": None,
            "repository": None,
        },
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
    assert_checksums(first)
    provenance = json.loads((first / "provenance.json").read_bytes())
    assert all(not Path(asset["path"]).is_absolute() for asset in provenance["assets"])
    source_manifest = json.loads((first / "source" / "source-manifest.json").read_bytes())
    source_paths = {entry["path"] for entry in source_manifest["files"]}
    assert "release/install-catalog-bootstrap-v1.json" in source_paths
    assert all(
        not path.startswith(("payload/", "tests/ui/"))
        and path != "release/compatibility-index.json"
        for path in source_paths
    )

    assemble(REPOSITORY, inputs, second)
    version = load_matrix(REPOSITORY)[0]["manager_version"]
    assert digest(first / "source" / f"csa-{version}.tar.gz") == digest(
        second / "source" / f"csa-{version}.tar.gz"
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
    isolated = {
        name: root / name
        for name in ["home", "codex-home", "npm-prefix", "activation"]
    }
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
        "refs/tags/v0.1.8",
        "https://example.invalid/csa",
    )
    assert value["platforms"]["win32-x64"]["status"] == "pass"
    assert value["platforms"]["linux-x64"]["status"] == "not_verified"
    assert "patched_artifacts" not in value


def git(root: Path, *args: str) -> str:
    result = subprocess.run(
        ["git", *args], cwd=root, check=True, capture_output=True, text=True
    )
    return result.stdout.strip()


def release_notes_commit(
    root: Path, relative: str, content: str, subject: str, body: str | None = None
) -> None:
    path = root / relative
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")
    git(root, "add", "--", relative)
    args = ["commit", "-q", "-m", subject]
    if body is not None:
        args.extend(["-m", body])
    git(root, *args)


def initialize_release_notes_repository(root: Path) -> None:
    root.mkdir()
    git(root, "init", "-q")
    git(root, "config", "user.name", "CSA Test")
    git(root, "config", "user.email", "csa@example.invalid")
    release_notes_commit(root, "src/main.rs", "baseline\n", "chore: fixture baseline")


def expect_release_notes_error(call) -> None:
    try:
        call()
    except ReleaseNotesError:
        return
    raise AssertionError("invalid release-note input was accepted")


def test_release_notes(root: Path) -> None:
    initialize_release_notes_repository(root)
    git(root, "tag", "v1.0.0")
    git(root, "tag", "compat-rust-v1.2.3-native-join-p99")

    release_notes_commit(root, "src/main.rs", "one\n", "feat(cli): choose [safe] *mode*")
    release_notes_commit(root, "src/main.rs", "two\n", "feat(cli): CHOOSE [safe] *mode*")
    release_notes_commit(
        root,
        "src/skipped.rs",
        "skip\n",
        "feat(manager): skipped feature",
        "Changelog: skip",
    )
    release_notes_commit(
        root, "src/heading.rs", "literal\n", "feat(cli): # keep heading literal"
    )
    release_notes_commit(
        root, "src/feature.rs", "feature\n", "feat(manager): add managed activation"
    )
    release_notes_commit(
        root, "src/breaking.rs", "breaking\n", "feat(cli)!: replace legacy mode"
    )
    release_notes_commit(
        root, "src/perf.rs", "fast\n", "perf(manager): speed resolution"
    )
    release_notes_commit(
        root, "src/refactor.rs", "simple\n", "refactor(manager): simplify routing"
    )
    release_notes_commit(
        root, "docs/install.md", "guide\n", "docs: explain installation"
    )
    release_notes_commit(
        root,
        ".github/workflows/release-csa.yml",
        "name: fixture\n",
        "ci: stabilize manager release",
    )
    release_notes_commit(root, "Cargo.toml", "[package]\n", "build: pin release metadata")
    release_notes_commit(root, "src/test_only.rs", "test\n", "test: hidden manager test")
    release_notes_commit(root, "src/chore.rs", "chore\n", "chore: hidden cleanup")
    git(root, "tag", "v1.1.0")

    output = root / "manager.md"
    result = generate(root, "manager", "HEAD", output, version="1.1.0")
    notes = output.read_text(encoding="utf-8")
    assert result["previous_tag"] == "v1.0.0"
    assert (
        "[v1.0.0...v1.1.0](https://github.com/DSLZL/CSA/compare/v1.0.0...v1.1.0)"
        in notes
    )
    assert not notes.startswith("# CSA")
    dynamic = notes.split("## Changelog", 1)[0]
    assert dynamic.count("Choose \\[safe\\] \\*mode\\*.") == 1
    assert "\\# keep heading literal." in dynamic
    for section in (
        "## New Features",
        "## CLI",
        "## Improvements",
        "## Build & Release",
        "## Documentation",
    ):
        assert section in notes
    assert "Breaking: Replace legacy mode." in notes
    assert notes.index("Speed resolution.") < notes.index("Simplify routing.")
    for hidden in ("skipped feature", "hidden manager test", "hidden cleanup"):
        assert hidden not in notes
    assert "## Release Information" not in notes

    first = root.parent / "release-notes-first"
    initialize_release_notes_repository(first)
    git(first, "tag", "v0.1.0")
    first_output = first / "manager.md"
    first_result = generate(first, "manager", "HEAD", first_output, version="0.1.0")
    first_notes = first_output.read_text(encoding="utf-8")
    assert first_result["previous_tag"] is None
    assert "No user-facing changes in this release." in first_notes
    assert "Initial release history through `v0.1.0`." in first_notes

    expect_release_notes_error(
        lambda: generate(root, "manager", "missing", root / "missing.md", version="1.1.0")
    )
    expect_release_notes_error(
        lambda: generate(root, "compat", "HEAD", root / "compat.md", version="1.1.0")
    )
    git(root, "tag", "v2.0.0", "v1.0.0")
    expect_release_notes_error(
        lambda: generate(root, "manager", "HEAD", root / "wrong-tag.md", version="2.0.0")
    )


def test_repository_boundary() -> None:
    workflows = {
        path.name
        for pattern in ("*.yml", "*.yaml")
        for path in (REPOSITORY / ".github" / "workflows").glob(pattern)
    }
    assert workflows == {"ci.yml", "publish-npm.yml", "release-csa.yml"}

    removed = [
        "payload/codex/.gitattributes",
        "release/compatibility-index.json",
        "release/build-profiles/windows-msvc-x64.json",
        "tests/ui/Cargo.toml",
        "release-readiness.md",
        "scripts/build_patched_codex_bundle.sh",
        "scripts/check_sccache_stats.py",
        "scripts/compat_catalog.py",
        "scripts/compat_release.py",
        "scripts/compatibility_audit.py",
        "scripts/patch_family.py",
        "scripts/run_patch_contract.py",
        "scripts/validation_evidence.py",
        "scripts/verify_patch_payload.py",
        "scripts/verify_release_asset_set.py",
    ]
    assert all(not (REPOSITORY / relative).exists() for relative in removed)

    ci = (REPOSITORY / ".github" / "workflows" / "ci.yml").read_text(encoding="utf-8")
    for stale in (
        "payload/codex",
        "release-patched-codex",
        "validate-patched-codex",
        "build-patched-codex",
        "compat_catalog.py",
        "compatibility_audit.py",
    ):
        assert stale not in ci

    action_pattern = re.compile(r"^\s*-?\s*uses:\s+([^\s#]+)", re.MULTILINE)
    actions = {
        source
        for path in (REPOSITORY / ".github").rglob("*.yml")
        for source in action_pattern.findall(path.read_text(encoding="utf-8"))
        if not source.startswith("./")
    }
    assert actions
    assert all(
        re.fullmatch(r"[^@\s]+@v?[0-9]+\.[0-9]+\.[0-9]+", source)
        for source in actions
    )


def main() -> int:
    with tempfile.TemporaryDirectory(prefix="csa-manager-release-tools-") as directory:
        root = Path(directory)
        test_assembler(root)
        test_ci_input(root)
        test_release_notes(root / "release-notes")
        test_repository_boundary()
    print(
        json.dumps(
            {
                "schema": 1,
                "result": "pass",
                "assembler": "pass",
                "atomic_corruption_rejection": "pass",
                "deterministic_source_bundle": "pass",
                "ci_input": "pass",
                "manager_release_notes": "pass",
                "producer_boundary": "pass",
            },
            indent=2,
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
