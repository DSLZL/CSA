#!/usr/bin/env python3
"""Fail-closed verifier for one immutable Codex patch payload."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path, PurePosixPath


SHA1 = re.compile(r"[0-9a-f]{40}\Z")
SHA256 = re.compile(r"[0-9a-f]{64}\Z")
COMPAT_ID = re.compile(r"[a-z0-9][a-z0-9._-]+\Z")
VERSION = re.compile(r"[0-9]+\.[0-9]+\.[0-9]+\Z")
TARGET = re.compile(r"[A-Za-z0-9_.-]+\Z")
ROOT_KEYS = {
    "schema",
    "compat_id",
    "codex_version",
    "upstream_tag",
    "upstream_commit",
    "patch_api",
    "patch_set_version",
    "rust_toolchain",
    "rustc_commit",
    "build_target",
    "source_hashes",
    "source_hashes_sha256",
    "preimage_absent",
    "patches",
    "preimage",
    "artifacts",
}


class VerificationError(RuntimeError):
    pass


def _relative(value: object, label: str) -> str:
    if not isinstance(value, str) or not value:
        raise VerificationError(f"{label} must be a non-empty relative path")
    path = PurePosixPath(value)
    if (
        path.is_absolute()
        or path == PurePosixPath(".")
        or path.as_posix() != value
        or ".." in path.parts
        or "\\" in value
        or any(":" in part for part in path.parts)
    ):
        raise VerificationError(f"{label} must be a normalized relative POSIX path")
    return value


def _digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _run(
    argv: list[str],
    cwd: Path,
    *,
    check: bool = True,
    env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[bytes]:
    result = subprocess.run(
        argv,
        cwd=cwd,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        shell=False,
    )
    if check and result.returncode:
        detail = result.stderr.decode("utf-8", "replace").strip()
        raise VerificationError(f"argv failed ({result.returncode}): {argv!r}: {detail}")
    return result


def _load_manifest(path: Path) -> dict[str, object]:
    try:
        manifest = tomllib.loads(path.read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise VerificationError(f"cannot read manifest: {error}") from error
    _validate_manifest(manifest)
    return manifest


def _validate_manifest(manifest: dict[str, object]) -> None:
    unknown = set(manifest) - ROOT_KEYS
    missing = ROOT_KEYS - set(manifest)
    if unknown or missing:
        raise VerificationError(f"manifest keys mismatch; missing={sorted(missing)}, unknown={sorted(unknown)}")
    if (
        type(manifest["schema"]) is not int
        or manifest["schema"] != 1
        or type(manifest["patch_api"]) is not int
        or manifest["patch_api"] != 1
    ):
        raise VerificationError("unsupported manifest schema or patch_api")
    if type(manifest["patch_set_version"]) is not int or manifest["patch_set_version"] < 1:
        raise VerificationError("patch_set_version must be a positive integer")
    for key, pattern in (
        ("compat_id", COMPAT_ID),
        ("codex_version", VERSION),
        ("rust_toolchain", VERSION),
        ("upstream_commit", SHA1),
        ("rustc_commit", SHA1),
        ("source_hashes_sha256", SHA256),
    ):
        value = manifest[key]
        if not isinstance(value, str) or not pattern.fullmatch(value):
            raise VerificationError(f"invalid {key}")
    if not isinstance(manifest["upstream_tag"], str) or not manifest["upstream_tag"]:
        raise VerificationError("invalid upstream_tag")
    if not isinstance(manifest["build_target"], str) or not TARGET.fullmatch(manifest["build_target"]):
        raise VerificationError("invalid build_target")
    _relative(manifest["source_hashes"], "source_hashes")

    patches = manifest["patches"]
    if not isinstance(patches, list) or len(patches) != 5:
        raise VerificationError("exactly five ordered patches are required")
    patch_paths: list[str] = []
    for index, patch in enumerate(patches):
        if not isinstance(patch, dict) or set(patch) != {"path", "sha256"}:
            raise VerificationError(f"invalid patches[{index}]")
        patch_paths.append(_relative(patch["path"], f"patches[{index}].path"))
        if not isinstance(patch["sha256"], str) or not SHA256.fullmatch(patch["sha256"]):
            raise VerificationError(f"invalid patches[{index}].sha256")
    if patch_paths != sorted(patch_paths) or len(set(patch_paths)) != 5:
        raise VerificationError("patch paths must be unique and lexically ordered")

    preimage = manifest["preimage"]
    absent = manifest["preimage_absent"]
    if not isinstance(preimage, dict) or not preimage:
        raise VerificationError("preimage must be a non-empty table")
    if not isinstance(absent, list):
        raise VerificationError("preimage_absent must be an array")
    for source_path, digest in preimage.items():
        if not source_path.startswith("codex-rs/") or _relative(source_path, "preimage key") != source_path:
            raise VerificationError(f"invalid preimage path: {source_path!r}")
        if not isinstance(digest, str) or not SHA256.fullmatch(digest):
            raise VerificationError(f"invalid preimage digest: {source_path}")
    for source_path in absent:
        if not isinstance(source_path, str) or not source_path.startswith("codex-rs/"):
            raise VerificationError(f"invalid absent preimage path: {source_path!r}")
        _relative(source_path, "preimage_absent entry")
    if len(absent) != len(set(absent)):
        raise VerificationError("preimage_absent must contain unique paths")
    if set(preimage).intersection(absent):
        raise VerificationError("present and absent preimages overlap")

    artifacts = manifest["artifacts"]
    target = manifest["build_target"]
    if not isinstance(artifacts, dict) or set(artifacts) != {target}:
        raise VerificationError("artifacts must contain only the exact build_target")
    artifact = artifacts[target]
    if not isinstance(artifact, dict) or set(artifact) != {"url", "filename", "sha256", "size"}:
        raise VerificationError("invalid artifact entry")
    if not isinstance(artifact["url"], str) or not re.match(r"^(https|artifact|unpublished)://", artifact["url"]):
        raise VerificationError("artifact url must be explicit")
    if not isinstance(artifact["filename"], str) or not artifact["filename"]:
        raise VerificationError("artifact filename must be non-empty")
    if not isinstance(artifact["sha256"], str) or not SHA256.fullmatch(artifact["sha256"]):
        raise VerificationError("invalid artifact sha256")
    if type(artifact["size"]) is not int or artifact["size"] < 1:
        raise VerificationError("artifact size must be positive")


def _touched_paths(patches: list[Path]) -> set[str]:
    touched: set[str] = set()
    header = re.compile(r"diff --git a/(.+) b/(.+)\Z")
    for patch in patches:
        for line in patch.read_text(encoding="utf-8").splitlines():
            match = header.fullmatch(line)
            if match:
                if match.group(1) != match.group(2):
                    raise VerificationError(f"renames are unsupported: {line}")
                touched.add(_relative(match.group(1), "patch source path"))
    if not touched:
        raise VerificationError("patch set touches no files")
    return touched


def verify(manifest_path: Path, source: Path, apply: bool, artifact_path: Path | None) -> dict[str, object]:
    if not manifest_path.is_absolute() or not source.is_absolute():
        raise VerificationError("manifest and source paths must be absolute")
    manifest = _load_manifest(manifest_path)
    if not source.is_dir():
        raise VerificationError(f"source is not a directory: {source}")
    if _run(["git", "status", "--porcelain=v1", "--untracked-files=all"], source).stdout:
        raise VerificationError("source worktree must be clean")

    commit = _run(["git", "rev-parse", "HEAD"], source).stdout.decode().strip()
    if commit != manifest["upstream_commit"]:
        raise VerificationError(f"exact commit mismatch: expected {manifest['upstream_commit']}, got {commit}")
    tag_commit = _run(["git", "rev-parse", f"refs/tags/{manifest['upstream_tag']}^{{commit}}"], source).stdout.decode().strip()
    if tag_commit != commit:
        raise VerificationError("upstream tag does not peel to the exact commit")
    cargo = tomllib.loads((source / "codex-rs" / "Cargo.toml").read_text(encoding="utf-8"))
    if cargo.get("workspace", {}).get("package", {}).get("version") != manifest["codex_version"]:
        raise VerificationError("Codex workspace version mismatch")

    payload_root = manifest_path.parent
    hash_path = payload_root / _relative(manifest["source_hashes"], "source_hashes")
    hash_bytes = hash_path.read_bytes()
    if _digest(hash_bytes) != manifest["source_hashes_sha256"]:
        raise VerificationError("source-hashes file digest mismatch")
    hashes = json.loads(hash_bytes)
    expected_hash_keys = {"schema", "algorithm", "content", "commit", "present", "absent"}
    if not isinstance(hashes, dict) or set(hashes) != expected_hash_keys:
        raise VerificationError("invalid source-hashes document")
    if (
        type(hashes["schema"]) is not int
        or hashes["schema"] != 1
        or hashes["algorithm"] != "sha256"
        or hashes["content"] != "git_blob"
    ):
        raise VerificationError("unsupported source-hashes contract")
    if hashes["commit"] != commit or hashes["present"] != manifest["preimage"] or hashes["absent"] != manifest["preimage_absent"]:
        raise VerificationError("source-hashes and manifest preimages differ")

    for relative, expected in manifest["preimage"].items():
        if not (source / PurePosixPath(relative)).is_file():
            raise VerificationError(f"preimage file missing: {relative}")
        blob = _run(["git", "show", f"{commit}:{relative}"], source).stdout
        if _digest(blob) != expected:
            raise VerificationError(f"preimage hash mismatch: {relative}")
    for relative in manifest["preimage_absent"]:
        if (source / PurePosixPath(relative)).exists():
            raise VerificationError(f"expected absent preimage exists: {relative}")
        if _run(["git", "cat-file", "-e", f"{commit}:{relative}"], source, check=False).returncode == 0:
            raise VerificationError(f"expected absent path exists in commit: {relative}")

    patch_paths: list[Path] = []
    for patch in manifest["patches"]:
        path = payload_root / _relative(patch["path"], "patch path")
        data = path.read_bytes()
        if _digest(data) != patch["sha256"]:
            raise VerificationError(f"patch hash mismatch: {patch['path']}")
        patch_paths.append(path)
    if _touched_paths(patch_paths) != set(manifest["preimage"]) | set(manifest["preimage_absent"]):
        raise VerificationError("preimages do not exactly cover patch-touched paths")

    with tempfile.TemporaryDirectory(prefix="codex-patch-index-") as temp_dir:
        env = os.environ.copy()
        env["GIT_INDEX_FILE"] = str(Path(temp_dir) / "index")
        _run(["git", "read-tree", commit], source, env=env)
        for path in patch_paths:
            _run(
                ["git", "apply", "--cached", "--check", "--whitespace=error-all", str(path)],
                source,
                env=env,
            )
            _run(["git", "apply", "--cached", "--whitespace=error-all", str(path)], source, env=env)
    if apply:
        for path in patch_paths:
            _run(["git", "apply", "--check", "--whitespace=error-all", str(path)], source)
            _run(["git", "apply", "--whitespace=error-all", str(path)], source)

    artifact_result = None
    if artifact_path is not None:
        if not artifact_path.is_absolute() or not artifact_path.is_file():
            raise VerificationError("artifact must be an existing absolute file")
        artifact = manifest["artifacts"][manifest["build_target"]]
        if artifact_path.name != artifact["filename"]:
            raise VerificationError("artifact filename mismatch")
        data = artifact_path.read_bytes()
        if len(data) != artifact["size"] or _digest(data) != artifact["sha256"]:
            raise VerificationError("artifact hash or size mismatch")
        artifact_result = {"path": str(artifact_path), "size": len(data), "sha256": _digest(data)}

    return {
        "compat_id": manifest["compat_id"],
        "manifest": str(manifest_path),
        "source": str(source),
        "commit": commit,
        "patches": [str(path) for path in patch_paths],
        "applied": apply,
        "artifact": artifact_result,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--apply", action="store_true")
    parser.add_argument("--artifact", type=Path)
    args = parser.parse_args()
    try:
        result = verify(args.manifest, args.source, args.apply, args.artifact)
    except (VerificationError, OSError, UnicodeError, json.JSONDecodeError, tomllib.TOMLDecodeError) as error:
        print(f"verification failed: {error}", file=sys.stderr)
        return 2
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
