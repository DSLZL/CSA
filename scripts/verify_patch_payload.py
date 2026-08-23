#!/usr/bin/env python3
"""Fail-closed verifier for one immutable Codex patch payload."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import stat
import subprocess
import sys
import tempfile
import tomllib
from dataclasses import dataclass
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
FAMILY_BINDING_KEYS = ROOT_KEYS | {"family_id", "files"}
FAMILY_KEYS = {"schema", "family_id", "patch_api", "patch_set_version", "bindings"}
FAMILY_ENTRY_KEYS = {"compat_id", "manifest", "sha256"}


class VerificationError(RuntimeError):
    pass


@dataclass(frozen=True)
class LoadedPayload:
    manifest: dict[str, object]
    manifest_path: Path
    payload_root: Path
    files: dict[str, Path]
    source_schema: int
    family_id: str | None = None


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


def _read_toml(path: Path, label: str) -> tuple[dict[str, object], bytes]:
    try:
        data = path.read_bytes()
        value = tomllib.loads(data.decode("utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise VerificationError(f"cannot read {label}: {error}") from error
    except UnicodeDecodeError as error:
        raise VerificationError(f"cannot decode {label} as UTF-8: {error}") from error
    if not isinstance(value, dict):
        raise VerificationError(f"{label} must be a TOML table")
    return value, data


def _regular_relative(root: Path, relative: str, label: str) -> Path:
    current = root
    for part in PurePosixPath(relative).parts:
        current /= part
        try:
            metadata = current.lstat()
        except OSError as error:
            raise VerificationError(f"cannot inspect {label}: {error}") from error
        if stat.S_ISLNK(metadata.st_mode):
            raise VerificationError(f"{label} may not use symlinks")
    if not stat.S_ISREG(metadata.st_mode):
        raise VerificationError(f"{label} is not a regular file")
    resolved = current.resolve(strict=True)
    try:
        resolved.relative_to(root)
    except ValueError as error:
        raise VerificationError(f"{label} escapes its root") from error
    return resolved


def _payload_paths(manifest: dict[str, object]) -> set[str]:
    return {
        str(manifest["source_hashes"]),
        "test-contract.json",
        *(str(patch["path"]) for patch in manifest["patches"]),
    }


def _family_root(manifest_path: Path, compat_id: str, family_id: str) -> Path:
    compat_root = manifest_path.parent
    bindings_root = compat_root.parent
    family_root = bindings_root.parent
    if (
        manifest_path.name != "manifest.toml"
        or compat_root.name != compat_id
        or bindings_root.name != "bindings"
        or family_root.name != family_id
    ):
        raise VerificationError(
            "schema-2 manifest must be payload/codex/<family>/bindings/<compat_id>/manifest.toml"
        )
    return family_root


def _load_family(
    family_root: Path,
    family_id: str,
    manifest: dict[str, object],
    manifest_path: Path,
    manifest_bytes: bytes,
) -> None:
    family_path = _regular_relative(family_root, "family.toml", "patch family")
    family, _ = _read_toml(family_path, "patch family")
    unknown = set(family) - FAMILY_KEYS
    missing = FAMILY_KEYS - set(family)
    if unknown or missing:
        raise VerificationError(
            f"family keys mismatch; missing={sorted(missing)}, unknown={sorted(unknown)}"
        )
    if (
        type(family["schema"]) is not int
        or family["schema"] != 2
        or family["family_id"] != family_id
        or type(family["patch_api"]) is not int
        or family["patch_api"] != manifest["patch_api"]
        or type(family["patch_set_version"]) is not int
        or family["patch_set_version"] != manifest["patch_set_version"]
    ):
        raise VerificationError("patch family identity or API differs from its binding")
    bindings = family["bindings"]
    if not isinstance(bindings, list) or not bindings:
        raise VerificationError("patch family must contain at least one exact binding")
    seen_ids: set[str] = set()
    seen_paths: set[str] = set()
    selected = 0
    for index, binding in enumerate(bindings):
        if not isinstance(binding, dict) or set(binding) != FAMILY_ENTRY_KEYS:
            raise VerificationError(f"invalid bindings[{index}]")
        compat_id = binding["compat_id"]
        relative = _relative(binding["manifest"], f"bindings[{index}].manifest")
        digest = binding["sha256"]
        if (
            not isinstance(compat_id, str)
            or not COMPAT_ID.fullmatch(compat_id)
            or not isinstance(digest, str)
            or not SHA256.fullmatch(digest)
            or compat_id in seen_ids
            or relative in seen_paths
            or relative != f"bindings/{compat_id}/manifest.toml"
        ):
            raise VerificationError(f"invalid or duplicate bindings[{index}]")
        seen_ids.add(compat_id)
        seen_paths.add(relative)
        binding_path = _regular_relative(family_root, relative, f"family binding {relative}")
        try:
            binding_data = binding_path.read_bytes()
        except OSError as error:
            raise VerificationError(f"cannot read family binding {relative}: {error}") from error
        if _digest(binding_data) != digest:
            raise VerificationError(f"family binding digest mismatch: {relative}")
        if binding_path == manifest_path:
            selected += 1
            if compat_id != manifest["compat_id"] or binding_data != manifest_bytes:
                raise VerificationError("family index selects a different exact binding")
    if selected != 1:
        raise VerificationError("family index must select the exact binding once")


def _load_payload(path: Path) -> LoadedPayload:
    try:
        metadata = path.lstat()
    except OSError as error:
        raise VerificationError(f"cannot inspect manifest: {error}") from error
    if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISREG(metadata.st_mode):
        raise VerificationError("manifest must be a regular file, not a symlink")
    manifest_path = path.resolve(strict=True)
    raw, manifest_bytes = _read_toml(manifest_path, "manifest")
    schema = raw.get("schema")
    if type(schema) is not int:
        raise VerificationError("unsupported manifest schema or patch_api")
    if schema == 1:
        _validate_manifest(raw)
        if manifest_path.parent.name != raw["compat_id"]:
            raise VerificationError("compat_id must equal the payload directory name")
        files = {
            relative: manifest_path.parent / PurePosixPath(relative)
            for relative in _payload_paths(raw)
        }
        return LoadedPayload(raw, manifest_path, manifest_path.parent, files, 1)
    if schema != 2:
        raise VerificationError("unsupported manifest schema or patch_api")

    unknown = set(raw) - FAMILY_BINDING_KEYS
    missing = FAMILY_BINDING_KEYS - set(raw)
    if unknown or missing:
        raise VerificationError(
            f"manifest keys mismatch; missing={sorted(missing)}, unknown={sorted(unknown)}"
        )
    family_id = raw["family_id"]
    file_map = raw["files"]
    if not isinstance(family_id, str) or not COMPAT_ID.fullmatch(family_id):
        raise VerificationError("invalid family_id")
    if not isinstance(file_map, dict):
        raise VerificationError("files must be a logical-to-physical path table")

    manifest = {key: value for key, value in raw.items() if key in ROOT_KEYS}
    manifest["schema"] = 1
    _validate_manifest(manifest)
    family_root = _family_root(manifest_path, str(manifest["compat_id"]), family_id)
    _load_family(family_root, family_id, manifest, manifest_path, manifest_bytes)

    expected = _payload_paths(manifest)
    if set(file_map) != expected:
        raise VerificationError(
            f"binding files mismatch; missing={sorted(expected - set(file_map))}, "
            f"unknown={sorted(set(file_map) - expected)}"
        )
    files: dict[str, Path] = {}
    physical: set[str] = set()
    for logical, source in file_map.items():
        if not isinstance(logical, str):
            raise VerificationError("binding logical paths must be strings")
        logical = _relative(logical, "files key")
        relative = _relative(source, f"files[{logical!r}]")
        if relative in physical:
            raise VerificationError("one binding file may not serve multiple logical paths")
        physical.add(relative)
        files[logical] = family_root / PurePosixPath(relative)
    return LoadedPayload(manifest, manifest_path, family_root, files, 2, family_id)


def _load_manifest(path: Path) -> dict[str, object]:
    return _load_payload(path).manifest


def _payload_file(payload: LoadedPayload, logical: str) -> Path:
    try:
        path = payload.files[logical]
    except KeyError as error:
        raise VerificationError(f"payload does not declare file: {logical}") from error
    relative = path.relative_to(payload.payload_root).as_posix()
    return _regular_relative(payload.payload_root, relative, f"payload file {logical}")


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
    if type(manifest["patch_set_version"]) is not int or manifest["patch_set_version"] not in {1, 2, 3, 4, 5, 6}:
        raise VerificationError("patch_set_version must be 1, 2, 3, 4, 5, or 6")
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
    expected_patch_count = {1: 5, 2: 6, 3: 11, 4: 12, 5: 13, 6: 14}[
        manifest["patch_set_version"]
    ]
    if not isinstance(patches, list) or len(patches) != expected_patch_count:
        raise VerificationError(
            f"patch set {manifest['patch_set_version']} requires exactly {expected_patch_count} ordered patches"
        )
    patch_paths: list[str] = []
    for index, patch in enumerate(patches):
        if not isinstance(patch, dict) or set(patch) != {"path", "sha256"}:
            raise VerificationError(f"invalid patches[{index}]")
        patch_paths.append(_relative(patch["path"], f"patches[{index}].path"))
        if not isinstance(patch["sha256"], str) or not SHA256.fullmatch(patch["sha256"]):
            raise VerificationError(f"invalid patches[{index}].sha256")
    if patch_paths != sorted(patch_paths) or len(set(patch_paths)) != len(patch_paths):
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
    payload = _load_payload(manifest_path)
    manifest = payload.manifest
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

    hash_path = _payload_file(payload, _relative(manifest["source_hashes"], "source_hashes"))
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
        path = _payload_file(payload, _relative(patch["path"], "patch path"))
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
