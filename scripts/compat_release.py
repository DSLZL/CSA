#!/usr/bin/env python3
"""Detect, port, finalize, and package formal CSA compatibility releases."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import tomllib
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Any

from compatibility_audit import validate_new_entry
from verify_patch_payload import (
    VerificationError,
    _digest,
    _load_manifest,
    _load_payload,
    _payload_file,
    _run,
    verify,
)


OPENAI_REPOSITORY = "openai/codex"
CSA_REPOSITORY = "dslzl/CSA"
BUILD_TARGET = "x86_64-pc-windows-msvc"
DESCRIPTOR_NAME = "compatibility-release.json"
CHECKSUMS_NAME = "SHA256SUMS"
BLOCKER_LABEL = "upstream-patch-blocked"
SHA1 = re.compile(r"[0-9a-f]{40}\Z")
SHA256 = re.compile(r"[0-9a-f]{64}\Z")
VERSION_TAG = re.compile(r"rust-v(\d+)\.(\d+)\.(\d+)\Z")
COMPAT_ID = re.compile(r"rust-v\d+\.\d+\.\d+-native-join-p(\d+)\Z")
TARGET_START = "<!-- csa-blocker-target:start -->"
TARGET_END = "<!-- csa-blocker-target:end -->"
FAILURE_START = "<!-- csa-blocker-failure:start -->"
FAILURE_END = "<!-- csa-blocker-failure:end -->"


class CompatibilityReleaseError(RuntimeError):
    pass


def json_bytes(value: object) -> bytes:
    return (json.dumps(value, indent=2, sort_keys=True) + "\n").encode()


def file_digest(path: Path) -> str:
    hasher = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            hasher.update(chunk)
    return hasher.hexdigest()


def stable_version(tag: object) -> str:
    if not isinstance(tag, str):
        raise CompatibilityReleaseError("official release tag must be a string")
    match = VERSION_TAG.fullmatch(tag)
    if not match:
        raise CompatibilityReleaseError("official stable tag must be exact rust-vX.Y.Z")
    return ".".join(match.groups())


def compatibility_id(version: str, patch_set_version: int = 1) -> str:
    if stable_version(f"rust-v{version}") != version or patch_set_version < 1:
        raise CompatibilityReleaseError("invalid compatibility identity")
    return f"rust-v{version}-native-join-p{patch_set_version}"


def asset_name(compat_id: str, relative: str) -> str:
    if not COMPAT_ID.fullmatch(compat_id):
        raise CompatibilityReleaseError("invalid compat_id for release asset")
    parts = relative.split("/")
    if (
        not relative
        or any(not part or part in {".", ".."} for part in parts)
        or any(not re.fullmatch(r"[A-Za-z0-9._-]+", part) for part in parts)
    ):
        raise CompatibilityReleaseError(f"invalid compatibility file path: {relative}")
    return f"{compat_id}--{'--'.join(parts)}"


class GitHubApi:
    def __init__(self, token: str | None = None) -> None:
        self.token = token

    def get(self, path: str, *, optional: bool = False) -> Any:
        url = f"https://api.github.com{path}"
        headers = {
            "Accept": "application/vnd.github+json",
            "X-GitHub-Api-Version": "2022-11-28",
            "User-Agent": "csa-upstream-release-watch",
        }
        if self.token:
            headers["Authorization"] = f"Bearer {self.token}"
        request = urllib.request.Request(url, headers=headers)
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                body = response.read(4 * 1024 * 1024 + 1)
        except urllib.error.HTTPError as error:
            if optional and error.code == 404:
                return None
            raise CompatibilityReleaseError(f"GitHub API {path} failed with HTTP {error.code}") from error
        except OSError as error:
            raise CompatibilityReleaseError(f"GitHub API {path} failed: {error}") from error
        if len(body) > 4 * 1024 * 1024:
            raise CompatibilityReleaseError(f"GitHub API response is too large: {path}")
        try:
            return json.loads(body)
        except json.JSONDecodeError as error:
            raise CompatibilityReleaseError(f"GitHub API returned invalid JSON: {path}") from error

    def peel_tag(self, repository: str, tag: str) -> str:
        reference = self.get(f"/repos/{repository}/git/ref/tags/{tag}")
        if (
            not isinstance(reference, dict)
            or reference.get("ref") != f"refs/tags/{tag}"
            or not isinstance(reference.get("object"), dict)
        ):
            raise CompatibilityReleaseError(f"invalid tag reference: {repository}@{tag}")
        current = reference["object"]
        for depth in range(5):
            kind, sha = current.get("type"), current.get("sha")
            if not isinstance(sha, str) or not SHA1.fullmatch(sha):
                raise CompatibilityReleaseError(f"invalid tag object: {repository}@{tag}")
            if kind == "commit":
                return sha
            if kind != "tag":
                raise CompatibilityReleaseError(f"tag does not peel to a commit: {repository}@{tag}")
            annotated = self.get(f"/repos/{repository}/git/tags/{sha}")
            if (
                not isinstance(annotated, dict)
                or not isinstance(annotated.get("object"), dict)
                or (depth == 0 and annotated.get("tag") != tag)
            ):
                raise CompatibilityReleaseError(f"invalid annotated tag: {repository}@{tag}")
            current = annotated["object"]
        raise CompatibilityReleaseError(f"tag indirection is too deep: {repository}@{tag}")


def latest_payload_manifest(repository: Path) -> Path:
    candidates: list[tuple[tuple[int, int, int, int, int], Path]] = []
    for path in (repository / "payload" / "codex").rglob("manifest.toml"):
        try:
            payload = _load_payload(path)
        except VerificationError:
            continue
        manifest = payload.manifest
        match = COMPAT_ID.fullmatch(manifest["compat_id"])
        version = VERSION_TAG.fullmatch(manifest["upstream_tag"])
        if match and version and manifest["build_target"] == BUILD_TARGET:
            key = tuple(map(int, version.groups())) + (
                int(match.group(1)),
                int(payload.source_schema == 2),
            )
            candidates.append((key, path))
    if not candidates:
        raise CompatibilityReleaseError("no Windows x64 compatibility payload can seed a port")
    best = max(key for key, _ in candidates)
    matches = [path for key, path in candidates if key == best]
    if len(matches) != 1:
        raise CompatibilityReleaseError("multiple family bindings claim the latest exact identity")
    return matches[0]


def exact_local_entry(repository: Path, compat_id: str, tag: str, commit: str) -> bool:
    root = repository / "payload" / "codex"
    paths = [
        *root.glob(f"*/bindings/{compat_id}/manifest.toml"),
        root / compat_id / "manifest.toml",
    ]
    existing = [path for path in paths if path.is_file()]
    if not existing:
        return False
    if len(existing) - int((root / compat_id / "manifest.toml").is_file()) > 1:
        raise CompatibilityReleaseError("multiple patch families claim the same compatibility ID")
    for path in existing:
        manifest = _load_manifest(path)
        if (
            manifest["compat_id"] != compat_id
            or manifest["upstream_tag"] != tag
            or manifest["upstream_commit"] != commit
            or manifest["codex_version"] != stable_version(tag)
            or manifest["build_target"] != BUILD_TARGET
        ):
            raise CompatibilityReleaseError(
                "local compatibility entry differs from the latest upstream identity"
            )
    return True


def detect(repository: Path, api: GitHubApi) -> dict[str, Any]:
    if not repository.is_absolute():
        raise CompatibilityReleaseError("repository must be an absolute path")
    repository = repository.resolve(strict=True)
    latest = api.get(f"/repos/{OPENAI_REPOSITORY}/releases/latest")
    if (
        not isinstance(latest, dict)
        or latest.get("draft") is not False
        or latest.get("prerelease") is not False
    ):
        raise CompatibilityReleaseError("GitHub latest is not a formal OpenAI Codex release")
    tag = latest.get("tag_name")
    version = stable_version(tag)
    commit = api.peel_tag(OPENAI_REPOSITORY, tag)
    base_manifest = latest_payload_manifest(repository)
    base = _load_manifest(base_manifest)
    compat_id = (
        base["compat_id"]
        if base["upstream_tag"] == tag and base["upstream_commit"] == commit
        else compatibility_id(version, base["patch_set_version"])
    )
    release_tag = f"compat-{compat_id}"
    release = api.get(f"/repos/{CSA_REPOSITORY}/releases/tags/{release_tag}", optional=True)
    if release is not None:
        if (
            not isinstance(release, dict)
            or release.get("tag_name") != release_tag
            or release.get("draft") is not False
            or release.get("prerelease") is not False
        ):
            raise CompatibilityReleaseError("existing compatibility release is not formal or exact")
        api.peel_tag(CSA_REPOSITORY, release_tag)
        action = "released"
        issue_number = None
        issue_needs_update = False
    else:
        existing_tag = api.get(
            f"/repos/{CSA_REPOSITORY}/git/ref/tags/{release_tag}", optional=True
        )
        if existing_tag is not None:
            tag_commit = api.peel_tag(CSA_REPOSITORY, release_tag)
            repository_head = _run(["git", "rev-parse", "HEAD"], repository).stdout.decode().strip()
            if tag_commit != repository_head:
                raise CompatibilityReleaseError(
                    "unpublished compatibility tag does not point to the current default-branch commit"
                )
        local_entry = exact_local_entry(repository, compat_id, tag, commit)
        if existing_tag is not None and not local_entry:
            raise CompatibilityReleaseError(
                "an unpublished compatibility tag exists without its reviewed default-branch payload"
            )
        issues = api.get(
            f"/repos/{CSA_REPOSITORY}/issues?state=open&labels={BLOCKER_LABEL}&per_page=100"
        )
        if not isinstance(issues, list):
            raise CompatibilityReleaseError("GitHub blocker query did not return an array")
        blockers = [
            issue
            for issue in issues
            if isinstance(issue, dict) and "pull_request" not in issue
        ]
        if len(blockers) > 1:
            raise CompatibilityReleaseError("more than one open upstream patch blocker exists")
        if blockers:
            issue = blockers[0]
            issue_number = issue.get("number")
            body = issue.get("body") or ""
            marker = f"<!-- csa-upstream: {tag} {commit} -->"
            action = "blocked"
            issue_needs_update = marker not in body
        elif local_entry:
            action = "publish"
            issue_number = None
            issue_needs_update = False
        else:
            branch = f"automation/compat-{compat_id}"
            head = urllib.parse.quote(f"dslzl:{branch}", safe="")
            pulls = api.get(f"/repos/{CSA_REPOSITORY}/pulls?state=open&head={head}&per_page=100")
            if not isinstance(pulls, list):
                raise CompatibilityReleaseError("GitHub candidate PR query did not return an array")
            candidate_open = any(
                isinstance(pull, dict)
                and isinstance(pull.get("head"), dict)
                and pull["head"].get("ref") == branch
                for pull in pulls
            )
            action = "candidate_open" if candidate_open else "patch"
            issue_number = None
            issue_needs_update = False
    return {
        "schema": 1,
        "action": action,
        "upstream_version": version,
        "upstream_tag": tag,
        "upstream_commit": commit,
        "compat_id": compat_id,
        "compat_release_tag": release_tag,
        "base_manifest": base_manifest.relative_to(repository).as_posix(),
        "issue_number": issue_number,
        "issue_needs_update": issue_needs_update,
    }


def write_github_output(path: Path, value: dict[str, Any]) -> None:
    with path.open("a", encoding="utf-8", newline="\n") as output:
        for key, item in value.items():
            if isinstance(item, bool):
                rendered = str(item).lower()
            elif item is None:
                rendered = ""
            elif isinstance(item, (str, int)) and "\n" not in str(item):
                rendered = str(item)
            else:
                continue
            output.write(f"{key}={rendered}\n")


def git_blob(source: Path, commit: str, relative: str) -> bytes | None:
    result = _run(["git", "show", f"{commit}:{relative}"], source, check=False)
    return result.stdout if result.returncode == 0 else None


def toml_string(value: str) -> str:
    return json.dumps(value, ensure_ascii=True)


def render_manifest(manifest: dict[str, Any]) -> str:
    artifact = manifest["artifacts"][manifest["build_target"]]
    lines = [
        "schema = 1",
        f"compat_id = {toml_string(manifest['compat_id'])}",
        f"codex_version = {toml_string(manifest['codex_version'])}",
        f"upstream_tag = {toml_string(manifest['upstream_tag'])}",
        f"upstream_commit = {toml_string(manifest['upstream_commit'])}",
        f"patch_api = {manifest['patch_api']}",
        f"patch_set_version = {manifest['patch_set_version']}",
        f"rust_toolchain = {toml_string(manifest['rust_toolchain'])}",
        f"rustc_commit = {toml_string(manifest['rustc_commit'])}",
        f"build_target = {toml_string(manifest['build_target'])}",
        f"source_hashes = {toml_string(manifest['source_hashes'])}",
        f"source_hashes_sha256 = {toml_string(manifest['source_hashes_sha256'])}",
        "preimage_absent = [",
        *[f"  {toml_string(path)}," for path in manifest["preimage_absent"]],
        "]",
        "",
    ]
    for patch in manifest["patches"]:
        lines.extend(
            [
                "[[patches]]",
                f"path = {toml_string(patch['path'])}",
                f"sha256 = {toml_string(patch['sha256'])}",
                "",
            ]
        )
    lines.append("[preimage]")
    lines.extend(
        f"{toml_string(path)} = {toml_string(digest)}"
        for path, digest in sorted(manifest["preimage"].items())
    )
    lines.extend(
        [
            "",
            f"[artifacts.{toml_string(manifest['build_target'])}]",
            f"url = {toml_string(artifact['url'])}",
            f"filename = {toml_string(artifact['filename'])}",
            f"sha256 = {toml_string(artifact['sha256'])}",
            f"size = {artifact['size']}",
            "",
        ]
    )
    return "\n".join(lines)


def render_binding_manifest(
    manifest: dict[str, Any], family_id: str, files: dict[str, str]
) -> str:
    legacy = render_manifest(manifest).splitlines()
    legacy[0] = "schema = 2"
    legacy.insert(1, f"family_id = {toml_string(family_id)}")
    legacy.extend(
        [
            "[files]",
            *[
                f"{toml_string(logical)} = {toml_string(physical)}"
                for logical, physical in sorted(files.items())
            ],
            "",
        ]
    )
    return "\n".join(legacy)


def family_file_map(payload: object) -> dict[str, str]:
    return {
        logical: path.relative_to(payload.payload_root).as_posix()
        for logical, path in payload.files.items()
    }


def family_with_digest(
    data: bytes, compat_id: str, manifest_path: str, digest: str
) -> bytes:
    family = tomllib.loads(data.decode("utf-8"))
    rows = [
        row
        for row in family.get("bindings", [])
        if row.get("compat_id") == compat_id and row.get("manifest") == manifest_path
    ]
    if len(rows) != 1:
        raise CompatibilityReleaseError("family index does not select the exact binding once")
    old = rows[0].get("sha256")
    needle = f'sha256 = "{old}"'.encode()
    if not isinstance(old, str) or data.count(needle) != 1:
        raise CompatibilityReleaseError("family binding digest is not uniquely replaceable")
    return data.replace(needle, f'sha256 = "{digest}"'.encode(), 1)


def validate_source(source: Path, tag: str, commit: str, version: str) -> None:
    if _run(["git", "status", "--porcelain=v1", "--untracked-files=all"], source).stdout:
        raise CompatibilityReleaseError("upstream source must be clean")
    head = _run(["git", "rev-parse", "HEAD"], source).stdout.decode().strip()
    peeled = _run(["git", "rev-parse", f"refs/tags/{tag}^{{commit}}"], source).stdout.decode().strip()
    if head != commit or peeled != commit:
        raise CompatibilityReleaseError("upstream source HEAD/tag differs from the detected commit")
    try:
        cargo = tomllib.loads((source / "codex-rs" / "Cargo.toml").read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise CompatibilityReleaseError(f"cannot read upstream workspace version: {error}") from error
    if cargo.get("workspace", {}).get("package", {}).get("version") != version:
        raise CompatibilityReleaseError("upstream workspace version differs from the detected release")


def port(base_manifest: Path, source: Path, tag: str, commit: str, output: Path) -> dict[str, Any]:
    for path, label in ((base_manifest, "base manifest"), (source, "source"), (output, "output")):
        if not path.is_absolute():
            raise CompatibilityReleaseError(f"{label} must be absolute")
    base_manifest = base_manifest.resolve(strict=True)
    source = source.resolve(strict=True)
    if output.exists() or not output.parent.is_dir():
        raise CompatibilityReleaseError("output must be a new directory under an existing parent")
    version = stable_version(tag)
    if not SHA1.fullmatch(commit):
        raise CompatibilityReleaseError("upstream commit must be lowercase 40-hex")
    validate_source(source, tag, commit, version)
    payload = _load_payload(base_manifest)
    manifest = payload.manifest
    compat_id = compatibility_id(version, manifest["patch_set_version"])
    if output.name != compat_id:
        raise CompatibilityReleaseError("output directory must equal the new compat_id")
    if payload.source_schema == 2 and output.parent.resolve(strict=True) != (
        payload.payload_root / "bindings"
    ).resolve(strict=True):
        raise CompatibilityReleaseError("family binding output must be under its bindings directory")

    temporary = Path(tempfile.mkdtemp(prefix=f".{compat_id}.", dir=output.parent))
    family_path = payload.payload_root / "family.toml"
    family_next = family_path.with_name("family.toml.next")
    if payload.source_schema == 2 and family_next.exists():
        raise CompatibilityReleaseError("staged family index already exists")
    old_family: bytes | None = None
    family_replaced = False
    try:
        manifest["compat_id"] = compat_id
        manifest["codex_version"] = version
        manifest["upstream_tag"] = tag
        manifest["upstream_commit"] = commit
        touched = sorted(set(manifest["preimage"]) | set(manifest["preimage_absent"]))
        present: dict[str, str] = {}
        absent: list[str] = []
        for relative in touched:
            blob = git_blob(source, commit, relative)
            if blob is None:
                absent.append(relative)
            else:
                present[relative] = _digest(blob)
        manifest["preimage"] = present
        manifest["preimage_absent"] = absent

        for patch in manifest["patches"]:
            source_patch = _payload_file(payload, patch["path"])
            if file_digest(source_patch) != patch["sha256"]:
                raise CompatibilityReleaseError(f"base patch hash mismatch: {patch['path']}")
            if payload.source_schema == 1:
                destination = temporary / patch["path"]
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(source_patch, destination)

        contract = json.loads(_payload_file(payload, "test-contract.json").read_bytes())
        contract["compat_id"] = compat_id
        (temporary / "test-contract.json").write_bytes(json_bytes(contract))
        hashes = {
            "schema": 1,
            "algorithm": "sha256",
            "content": "git_blob",
            "commit": commit,
            "present": present,
            "absent": absent,
        }
        hash_path = temporary / manifest["source_hashes"]
        hash_path.parent.mkdir(parents=True, exist_ok=True)
        hash_path.write_bytes(json_bytes(hashes))
        manifest["source_hashes_sha256"] = file_digest(hash_path)
        artifact = manifest["artifacts"][manifest["build_target"]]
        artifact["sha256"] = "0" * 64
        artifact["size"] = 1
        artifact["url"] = (
            f"https://github.com/{CSA_REPOSITORY}/releases/download/compat-{compat_id}/"
            f"{asset_name(compat_id, artifact['filename'])}"
        )
        if payload.source_schema == 2:
            files = family_file_map(payload)
            files[manifest["source_hashes"]] = (
                f"bindings/{compat_id}/{manifest['source_hashes']}"
            )
            files["test-contract.json"] = f"bindings/{compat_id}/test-contract.json"
            binding = render_binding_manifest(manifest, payload.family_id, files).encode()
            (temporary / "manifest.toml").write_bytes(binding)

            old_family = family_path.read_bytes()
            family = tomllib.loads(old_family.decode("utf-8"))
            if any(row.get("compat_id") == compat_id for row in family["bindings"]):
                raise CompatibilityReleaseError("family already declares the compatibility binding")
            relative_manifest = f"bindings/{compat_id}/manifest.toml"
            row = (
                "[[bindings]]\n"
                f"compat_id = {toml_string(compat_id)}\n"
                f"manifest = {toml_string(relative_manifest)}\n"
                f"sha256 = {toml_string(_digest(binding))}\n"
            ).encode()
            separator = b"\n" if old_family.endswith(b"\n") else b"\n\n"
            family_next.write_bytes(old_family + separator + row)
        else:
            (temporary / "manifest.toml").write_text(
                render_manifest(manifest), encoding="utf-8", newline="\n"
            )

        temporary.replace(output)
        if payload.source_schema == 2:
            family_next.replace(family_path)
            family_replaced = True
        verify((output / "manifest.toml").resolve(), source, False, None)
    except Exception:
        shutil.rmtree(temporary, ignore_errors=True)
        shutil.rmtree(output, ignore_errors=True)
        if family_replaced and old_family is not None:
            family_path.write_bytes(old_family)
        family_next.unlink(missing_ok=True)
        raise
    return {
        "schema": payload.source_schema,
        "result": "ported",
        "compat_id": compat_id,
        "manifest": str(output / "manifest.toml"),
        "upstream_tag": tag,
        "upstream_commit": commit,
    }


def finalize(manifest_path: Path, artifact_path: Path) -> dict[str, Any]:
    if not manifest_path.is_absolute() or not artifact_path.is_absolute():
        raise CompatibilityReleaseError("manifest and artifact must be absolute")
    manifest_path = manifest_path.resolve(strict=True)
    artifact_path = artifact_path.resolve(strict=True)
    payload = _load_payload(manifest_path)
    manifest = payload.manifest
    artifact = manifest["artifacts"][manifest["build_target"]]
    if artifact_path.name != artifact["filename"]:
        raise CompatibilityReleaseError("built artifact filename differs from the manifest")
    artifact["size"] = artifact_path.stat().st_size
    artifact["sha256"] = file_digest(artifact_path)
    artifact["url"] = (
        f"https://github.com/{CSA_REPOSITORY}/releases/download/compat-{manifest['compat_id']}/"
        f"{asset_name(manifest['compat_id'], artifact['filename'])}"
    )
    staged = manifest_path.with_name("manifest.toml.next")
    if staged.exists():
        raise CompatibilityReleaseError("staged manifest already exists")
    family_path = payload.payload_root / "family.toml"
    family_next = family_path.with_name("family.toml.next")
    if payload.source_schema == 2 and family_next.exists():
        raise CompatibilityReleaseError("staged family index already exists")
    old_manifest = manifest_path.read_bytes()
    old_family: bytes | None = None
    family_replaced = False
    try:
        if payload.source_schema == 2:
            binding = render_binding_manifest(
                manifest, payload.family_id, family_file_map(payload)
            ).encode()
            staged.write_bytes(binding)
            old_family = family_path.read_bytes()
            relative = manifest_path.relative_to(payload.payload_root).as_posix()
            family_next.write_bytes(
                family_with_digest(old_family, manifest["compat_id"], relative, _digest(binding))
            )
            tomllib.loads(binding.decode("utf-8"))
        else:
            staged.write_text(render_manifest(manifest), encoding="utf-8", newline="\n")
            _load_manifest(staged)
        staged.replace(manifest_path)
        if payload.source_schema == 2:
            family_next.replace(family_path)
            family_replaced = True
        _load_payload(manifest_path)
    except Exception:
        staged.unlink(missing_ok=True)
        family_next.unlink(missing_ok=True)
        manifest_path.write_bytes(old_manifest)
        if family_replaced and old_family is not None:
            family_path.write_bytes(old_family)
        raise
    return {
        "schema": payload.source_schema,
        "result": "finalized",
        "compat_id": manifest["compat_id"],
        "artifact": {
            "path": str(artifact_path),
            "size": artifact["size"],
            "sha256": artifact["sha256"],
        },
    }


def payload_bytes(manifest_path: Path) -> list[tuple[str, bytes]]:
    payload = _load_payload(manifest_path)
    manifest = payload.manifest
    values = [
        ("manifest.toml", render_manifest(manifest).encode()),
        (manifest["source_hashes"], _payload_file(payload, manifest["source_hashes"]).read_bytes()),
        ("test-contract.json", _payload_file(payload, "test-contract.json").read_bytes()),
        *[
            (patch["path"], _payload_file(payload, patch["path"]).read_bytes())
            for patch in manifest["patches"]
        ],
    ]
    if len({relative for relative, _ in values}) != len(values):
        raise CompatibilityReleaseError("compatibility payload contains duplicate paths")
    return sorted(values)


def pack(manifest_path: Path, artifact_path: Path, source_commit: str, output: Path) -> dict[str, Any]:
    for path, label in ((manifest_path, "manifest"), (artifact_path, "artifact"), (output, "output")):
        if not path.is_absolute():
            raise CompatibilityReleaseError(f"{label} must be absolute")
    manifest_path = manifest_path.resolve(strict=True)
    artifact_path = artifact_path.resolve(strict=True)
    if not SHA1.fullmatch(source_commit):
        raise CompatibilityReleaseError("CSA source commit must be lowercase 40-hex")
    if output.exists() or not output.parent.is_dir():
        raise CompatibilityReleaseError("output must be a new directory under an existing parent")
    validate_new_entry(manifest_path.parent)
    manifest = _load_manifest(manifest_path)
    artifact = manifest["artifacts"][manifest["build_target"]]
    if (
        artifact_path.name != artifact["filename"]
        or artifact_path.stat().st_size != artifact["size"]
        or file_digest(artifact_path) != artifact["sha256"]
    ):
        raise CompatibilityReleaseError("patched artifact differs from the finalized manifest")

    compat_id = manifest["compat_id"]
    temporary = Path(tempfile.mkdtemp(prefix=f".{output.name}.", dir=output.parent))
    try:
        payload = []
        names = set()
        for relative, contents in payload_bytes(manifest_path):
            name = asset_name(compat_id, relative)
            if name in names:
                raise CompatibilityReleaseError(f"release asset name collision: {name}")
            names.add(name)
            destination = temporary / name
            destination.write_bytes(contents)
            payload.append(
                {
                    "path": relative,
                    "asset": name,
                    "size": destination.stat().st_size,
                    "sha256": file_digest(destination),
                }
            )
        artifact_name = asset_name(compat_id, artifact["filename"])
        if artifact_name in names:
            raise CompatibilityReleaseError(f"release asset name collision: {artifact_name}")
        artifact_destination = temporary / artifact_name
        shutil.copyfile(artifact_path, artifact_destination)
        descriptor = {
            "schema": 1,
            "repository": CSA_REPOSITORY,
            "release_tag": f"compat-{compat_id}",
            "source_commit": source_commit,
            "compat_id": compat_id,
            "upstream": {
                "repository": OPENAI_REPOSITORY,
                "version": manifest["codex_version"],
                "tag": manifest["upstream_tag"],
                "commit": manifest["upstream_commit"],
            },
            "build_target": manifest["build_target"],
            "payload": payload,
            "artifact": {
                "path": artifact["filename"],
                "asset": artifact_name,
                "size": artifact_destination.stat().st_size,
                "sha256": file_digest(artifact_destination),
            },
        }
        (temporary / DESCRIPTOR_NAME).write_bytes(json_bytes(descriptor))
        checksums = []
        for path in sorted(temporary.iterdir()):
            if path.is_file():
                checksums.append(f"{file_digest(path).upper()}  {path.name}")
        (temporary / CHECKSUMS_NAME).write_text("\n".join(checksums) + "\n", encoding="ascii")
        temporary.replace(output)
    except Exception:
        shutil.rmtree(temporary, ignore_errors=True)
        raise
    return {
        "schema": 1,
        "result": "packed",
        "compat_id": compat_id,
        "release_tag": f"compat-{compat_id}",
        "source_commit": source_commit,
        "assets": len(list(output.iterdir())),
        "output": str(output),
    }


def replace_block(body: str, start: str, end: str, replacement: str) -> str:
    if start in body and end in body and body.index(start) < body.index(end):
        before, rest = body.split(start, 1)
        _, after = rest.split(end, 1)
        return f"{before}{replacement}{after}".strip() + "\n"
    return f"{replacement}\n\n{body}".strip() + "\n"


def blocker_body(
    existing: str,
    tag: str,
    commit: str,
    run_url: str,
    stage: str | None = None,
    log: str | None = None,
    base_manifest: str | None = None,
) -> str:
    stable_version(tag)
    if not SHA1.fullmatch(commit):
        raise CompatibilityReleaseError("blocker target commit must be lowercase 40-hex")
    target = "\n".join(
        [
            TARGET_START,
            f"<!-- csa-upstream: {tag} {commit} -->",
            "## Latest blocked target",
            "",
            f"- Upstream tag: `{tag}`",
            f"- Upstream commit: `{commit}`",
            f"- Latest observation: {run_url}",
            TARGET_END,
        ]
    )
    body = replace_block(existing, TARGET_START, TARGET_END, target)
    if stage is not None:
        manifest_match = re.fullmatch(
            r"payload/codex/(?:([A-Za-z0-9._-]+)/bindings/)?"
            r"(rust-v\d+\.\d+\.\d+-native-join-p(\d+))/manifest\.toml",
            base_manifest or "",
        )
        if not manifest_match:
            raise CompatibilityReleaseError("failure record requires a safe base manifest path")
        family_id, _, patch_set = manifest_match.groups()
        compat_id = compatibility_id(stable_version(tag), int(patch_set))
        output = (
            f"$PWD/payload/codex/{family_id}/bindings/{compat_id}"
            if family_id
            else f"$PWD/payload/codex/{compat_id}"
        )
        excerpt = (log or "no failure log was captured")[-12000:]
        failure = "\n".join(
            [
                FAILURE_START,
                "## Automatic patch failure",
                "",
                f"- Failed target: `{tag}` / `{commit}`",
                f"- Stage: `{stage}`",
                f"- Failed run: {run_url}",
                "- Repair: adapt the compatibility payload to the latest target and merge a reviewed PR containing `Fixes #<this issue>`.",
                "",
                "Reproduce from the CSA repository root:",
                "",
                "```powershell",
                "$sourceRoot = Join-Path ([IO.Path]::GetTempPath()) (\"csa-codex-\" + [guid]::NewGuid())",
                f"git clone --filter=blob:none --no-checkout --branch {tag} --single-branch https://github.com/openai/codex.git \"$sourceRoot\"",
                f"git -C \"$sourceRoot\" checkout --detach {commit}",
                "python scripts/compat_release.py port `",
                f"  --base-manifest \"$PWD/{base_manifest}\" `",
                "  --source \"$sourceRoot\" `",
                f"  --tag {tag} `",
                f"  --commit {commit} `",
                f"  --output \"{output}\"",
                "```",
                "",
                "```text",
                excerpt.replace("```", "` ` `"),
                "```",
                FAILURE_END,
            ]
        )
        body = replace_block(body, FAILURE_START, FAILURE_END, failure)
    return body


def write_new(path: Path, data: bytes) -> None:
    if not path.is_absolute() or path.exists() or not path.parent.is_dir():
        raise CompatibilityReleaseError("output must be an absolute new file under an existing parent")
    with path.open("xb") as output:
        output.write(data)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)

    detect_parser = commands.add_parser("detect")
    detect_parser.add_argument("--repository", type=Path, required=True)
    detect_parser.add_argument("--output", type=Path, required=True)
    detect_parser.add_argument("--github-output", type=Path)

    port_parser = commands.add_parser("port")
    port_parser.add_argument("--base-manifest", type=Path, required=True)
    port_parser.add_argument("--source", type=Path, required=True)
    port_parser.add_argument("--tag", required=True)
    port_parser.add_argument("--commit", required=True)
    port_parser.add_argument("--output", type=Path, required=True)

    finalize_parser = commands.add_parser("finalize")
    finalize_parser.add_argument("--manifest", type=Path, required=True)
    finalize_parser.add_argument("--artifact", type=Path, required=True)

    pack_parser = commands.add_parser("pack")
    pack_parser.add_argument("--manifest", type=Path, required=True)
    pack_parser.add_argument("--artifact", type=Path, required=True)
    pack_parser.add_argument("--source-commit", required=True)
    pack_parser.add_argument("--output", type=Path, required=True)

    blocker_parser = commands.add_parser("blocker")
    blocker_parser.add_argument("--existing", type=Path)
    blocker_parser.add_argument("--tag", required=True)
    blocker_parser.add_argument("--commit", required=True)
    blocker_parser.add_argument("--run-url", required=True)
    blocker_parser.add_argument("--stage")
    blocker_parser.add_argument("--base-manifest")
    blocker_parser.add_argument("--log", type=Path)
    blocker_parser.add_argument("--output", type=Path, required=True)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    try:
        if args.command == "detect":
            result = detect(args.repository, GitHubApi(os.environ.get("GITHUB_TOKEN")))
            write_new(args.output, json_bytes(result))
            if args.github_output:
                write_github_output(args.github_output, result)
        elif args.command == "port":
            result = port(args.base_manifest, args.source, args.tag, args.commit, args.output)
        elif args.command == "finalize":
            result = finalize(args.manifest, args.artifact)
        elif args.command == "pack":
            result = pack(args.manifest, args.artifact, args.source_commit, args.output)
        else:
            existing = args.existing.read_text(encoding="utf-8") if args.existing else ""
            log = args.log.read_text(encoding="utf-8", errors="replace") if args.log else None
            body = blocker_body(
                existing,
                args.tag,
                args.commit,
                args.run_url,
                args.stage,
                log,
                args.base_manifest,
            )
            write_new(args.output, body.encode())
            result = {"schema": 1, "result": "rendered", "output": str(args.output)}
    except (
        CompatibilityReleaseError,
        VerificationError,
        OSError,
        subprocess.SubprocessError,
        json.JSONDecodeError,
    ) as error:
        print(json.dumps({"schema": 1, "error": str(error)}, indent=2), file=sys.stderr)
        return 2
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
