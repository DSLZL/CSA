#!/usr/bin/env python3
"""Create validated platform artifacts and CI release-input manifests."""

from __future__ import annotations

import argparse
import json
import os
import shutil
import tempfile
from pathlib import Path

from assemble_release_candidate import (
    ReleaseError,
    digest,
    load_json,
    load_matrix,
    validate_meta_tarball,
    validate_platform_tarball,
)


def require_absolute(path: Path, label: str, *, exists: bool = True) -> Path:
    if not path.is_absolute():
        raise ReleaseError(f"{label} must be absolute")
    if path.is_symlink():
        raise ReleaseError(f"{label} must not be a symlink")
    if exists:
        path = path.resolve(strict=True)
    if exists and not path.is_file():
        raise ReleaseError(f"{label} must be a regular file")
    return path


def copy(source: Path, destination: Path) -> None:
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source, destination)


def platform_artifact(
    repository: Path,
    platform_id: str,
    manager: Path,
    meta_tarball: Path,
    npm_tarball: Path,
    isolation_root: Path,
    output: Path,
) -> dict[str, object]:
    if not repository.is_absolute():
        raise ReleaseError("repository must be absolute")
    repository = repository.resolve(strict=True)
    if not repository.is_dir():
        raise ReleaseError("repository must be a directory")
    manager = require_absolute(manager, "manager")
    meta_tarball = require_absolute(meta_tarball, "meta tarball")
    npm_tarball = require_absolute(npm_tarball, "platform tarball")
    isolation_root = isolation_root.resolve(strict=True)
    if not output.is_absolute() or output.exists() or not output.parent.is_dir():
        raise ReleaseError("output must be a new absolute directory under an existing parent")
    try:
        output.relative_to(isolation_root)
    except ValueError as error:
        raise ReleaseError("CI platform output must stay inside the isolation root") from error
    matrix, platforms = load_matrix(repository)
    if platform_id not in platforms:
        raise ReleaseError(f"unknown platform id: {platform_id}")
    platform = platforms[platform_id]
    if manager.name != platform["binary"]:
        raise ReleaseError("manager filename does not match the support matrix")
    validate_meta_tarball(meta_tarball, matrix["manager_version"], platforms)
    manager_hash = validate_platform_tarball(
        npm_tarball, manager, matrix["manager_version"], platform
    )
    isolation = {
        "root": str(isolation_root),
        "ephemeral_runner": bool(os.environ.get("RUNNER_TEMP")),
        "global_install": False,
        "persistent_path_or_profile": False,
    }
    if isolation["ephemeral_runner"]:
        isolated_paths = {
            "home": os.environ.get("HOME"),
            "codex_home": os.environ.get("CODEX_HOME"),
            "npm_prefix": os.environ.get("NPM_CONFIG_PREFIX"),
            "activation": str(isolation_root / "activation"),
        }
        for label, value in isolated_paths.items():
            if not value:
                raise ReleaseError(f"CI isolation path is missing: {label}")
            path = Path(value).resolve(strict=True)
            if not path.is_dir():
                raise ReleaseError(f"CI isolation path is not a directory: {label}")
            try:
                path.relative_to(isolation_root)
            except ValueError as error:
                raise ReleaseError(f"CI isolation path escapes the isolation root: {label}") from error
            isolation[label] = str(path)

    temporary = Path(tempfile.mkdtemp(prefix=f".{output.name}.", dir=output.parent))
    try:
        copy(manager, temporary / "manager" / platform["binary"])
        copy(meta_tarball, temporary / "npm" / meta_tarball.name)
        copy(npm_tarball, temporary / "npm" / npm_tarball.name)
        report = {
            "schema": 1,
            "result": "pass",
            "platform": platform_id,
            "runner": platform["runner"],
            "target": platform["target"],
            "manager": {
                "file": f"manager/{platform['binary']}",
                "size": manager.stat().st_size,
                "sha256": manager_hash,
            },
            "npm": {
                "meta": f"npm/{meta_tarball.name}",
                "meta_sha256": digest(meta_tarball),
                "platform": f"npm/{npm_tarball.name}",
                "platform_sha256": digest(npm_tarball),
            },
            "checks": [
                "cargo test --all-targets",
                "cargo clippy --all-targets --all-features -- -D warnings",
                "cargo build --release",
                "launcher argv/env/cwd/stdio/exit/checksum/missing-package",
                (
                    "native process-group signal"
                    if platform["os"] != "win32"
                    else "signal not verified on Windows; inherited-process-group design only"
                ),
                "offline temporary-prefix npm install/version/uninstall",
            ],
            "isolation": isolation,
        }
        (temporary / "evidence.json").write_text(
            json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
        temporary.replace(output)
        return report
    except Exception:
        shutil.rmtree(temporary, ignore_errors=True)
        raise


def reproduction(platform: dict[str, object]) -> list[str]:
    target = platform["target"]
    platform_id = platform["id"]
    return [
        f"cargo test --all-targets --target {target}",
        f"cargo clippy --all-targets --all-features --target {target} -- -D warnings",
        f"cargo build --release --target {target}",
        "node scripts/test_npm_launcher.mjs",
        f"node scripts/stage_npm_packages.mjs --out <new-stage> --binary {platform_id}=<manager>",
        "npm pack <staged-meta> && npm pack <staged-platform>",
    ]


def build_input(
    repository: Path,
    artifacts: Path,
    output: Path,
    revision: str,
    ref: str | None,
    repository_url: str | None,
) -> dict[str, object]:
    repository = repository.resolve(strict=True)
    artifacts = artifacts.resolve(strict=True)
    if not repository.is_dir() or not artifacts.is_dir():
        raise ReleaseError("repository and artifacts must be directories")
    if not output.is_absolute() or output.exists() or not output.parent.is_dir():
        raise ReleaseError("output must be a new absolute file under an existing parent")
    matrix, platforms = load_matrix(repository)
    platform_inputs = {}
    meta_path: Path | None = None
    meta_hash: str | None = None
    for platform_id, platform in platforms.items():
        root = artifacts / platform_id
        evidence_path = root / "evidence.json"
        if not evidence_path.is_file():
            platform_inputs[platform_id] = {
                "status": "not_verified",
                "reason": "CI platform artifact was not present in the assembly job",
                "reproduce": reproduction(platform),
            }
            continue
        evidence = load_json(evidence_path)
        if evidence.get("result") != "pass" or evidence.get("platform") != platform_id:
            raise ReleaseError(f"invalid CI platform evidence: {platform_id}")
        manager = (root / evidence["manager"]["file"]).resolve(strict=True)
        current_meta = (root / evidence["npm"]["meta"]).resolve(strict=True)
        npm_tarball = (root / evidence["npm"]["platform"]).resolve(strict=True)
        if digest(manager) != evidence["manager"]["sha256"]:
            raise ReleaseError(f"CI manager hash drift: {platform_id}")
        if digest(current_meta) != evidence["npm"]["meta_sha256"]:
            raise ReleaseError(f"CI meta tarball hash drift: {platform_id}")
        if digest(npm_tarball) != evidence["npm"]["platform_sha256"]:
            raise ReleaseError(f"CI platform tarball hash drift: {platform_id}")
        if meta_hash is None:
            meta_path, meta_hash = current_meta, digest(current_meta)
        elif digest(current_meta) != meta_hash:
            raise ReleaseError("meta npm tarballs differ across platform lanes")
        platform_inputs[platform_id] = {
            "status": "pass",
            "manager": str(manager),
            "npm_tarball": str(npm_tarball),
            "evidence": str(evidence_path.resolve(strict=True)),
            "reproduce": reproduction(platform),
        }
    if meta_path is None:
        raise ReleaseError("at least one passing platform artifact is required")
    value = {
        "schema": 1,
        "release_version": matrix["manager_version"],
        "source": {"revision": revision, "ref": ref, "repository": repository_url},
        "meta_tarball": str(meta_path),
        "platforms": platform_inputs,
        "signing": {
            "status": "not_executed",
            "reason": "CI generates checksummed provenance only; cryptographic signing requires a separately authorized release lane",
        },
        "manual_gates": {
            "authenticated_native_join": "not_executed",
            "interactive_ctrl_c_windows": "not_executed",
            "production_plug": "not_executed",
            "npm_publish": "not_executed",
        },
    }
    output.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return value


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    platform = commands.add_parser("platform")
    platform.add_argument("--repository", type=Path, required=True)
    platform.add_argument("--platform", required=True)
    platform.add_argument("--manager", type=Path, required=True)
    platform.add_argument("--meta-tarball", type=Path, required=True)
    platform.add_argument("--npm-tarball", type=Path, required=True)
    platform.add_argument("--isolation-root", type=Path, required=True)
    platform.add_argument("--output", type=Path, required=True)
    release_input = commands.add_parser("input")
    release_input.add_argument("--repository", type=Path, required=True)
    release_input.add_argument("--artifacts", type=Path, required=True)
    release_input.add_argument("--output", type=Path, required=True)
    release_input.add_argument("--revision", required=True)
    release_input.add_argument("--ref")
    release_input.add_argument("--repository-url")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    try:
        if args.command == "platform":
            result = platform_artifact(
                args.repository,
                args.platform,
                args.manager,
                args.meta_tarball,
                args.npm_tarball,
                args.isolation_root,
                args.output,
            )
        else:
            result = build_input(
                args.repository,
                args.artifacts,
                args.output,
                args.revision,
                args.ref,
                args.repository_url,
            )
    except (OSError, ReleaseError, KeyError, TypeError) as error:
        print(json.dumps({"schema": 1, "error": str(error)}, indent=2), file=os.sys.stderr)
        return 2
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
