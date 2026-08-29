#!/usr/bin/env python3
"""Resolve and validate CSA patched-Codex compatibility build inputs.

The compatibility index is routing data only. Upstream identity, toolchain,
artifact identity, and patch facts remain authoritative in the compatibility
manifest. Runtime and build infrastructure identities are held in separately
reviewed JSON locks.
"""

from __future__ import annotations

import argparse
import base64
import datetime as dt
import hashlib
import json
import os
import re
import shlex
import subprocess
import sys
import tempfile
import tomllib
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path, PurePosixPath
from typing import Any, NoReturn
from urllib.parse import urlparse

SAFE_ID = re.compile(r"[A-Za-z0-9._-]+\Z")
LOWER_SHA256 = re.compile(r"[0-9a-f]{64}\Z")
LOWER_SHA1 = re.compile(r"[0-9a-f]{40}\Z")
CODEX_VERSION = re.compile(r"(\d+)\.(\d+)\.(\d+)\Z")
PATCH_REVISION = re.compile(r"-p(\d+)\Z")
INSTALL_CATALOG_NAME = "install-catalog-v1.json"
INSTALL_CATALOG_MAX_ENTRIES = 1_000
CSA_REPOSITORY = "DSLZL/CSA"
OUTPUT_KEYS = (
    "compat_id",
    "release_tag",
    "codex_version",
    "upstream_tag",
    "upstream_commit",
    "manifest_path",
    "manifest_sha256",
    "rust_toolchain",
    "rustc_commit",
    "build_target",
    "artifact_filename",
    "artifact_asset",
    "artifact_sha256",
    "artifact_size",
    "build_profile_path",
    "build_profile_sha256",
    "runtime_lock_path",
    "runtime_lock_sha256",
    "acceptance_path",
    "acceptance_sha256",
    "accepted_artifact_sha256",
    "accepted_artifact_size",
    "cargo_xwin_version",
    "sccache_version",
    "xwin_version",
    "llvm_version",
    "release_enabled",
    "lifecycle",
)


class CatalogError(RuntimeError):
    """Fail-closed catalog validation error."""


def fail(message: str) -> NoReturn:
    raise CatalogError(message)


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _reject_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    value: dict[str, Any] = {}
    for key, item in pairs:
        if key in value:
            fail(f"duplicate JSON key: {key}")
        value[key] = item
    return value


def load_json(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(
            path.read_text(encoding="utf-8"),
            object_pairs_hook=_reject_duplicate_keys,
        )
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        fail(f"cannot read valid JSON {path}: {error}")
    if not isinstance(value, dict):
        fail(f"JSON root must be an object: {path}")
    return value


def json_bytes(value: object) -> bytes:
    return (json.dumps(value, indent=2, ensure_ascii=False) + "\n").encode("utf-8")


def write_json_atomic(path: Path, value: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary_name = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    temporary = Path(temporary_name)
    try:
        with os.fdopen(fd, "wb") as stream:
            stream.write(json_bytes(value))
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def write_new_or_equal(path: Path, value: object) -> None:
    contents = json_bytes(value)
    if path.exists():
        if path.read_bytes() != contents:
            fail(f"refusing to overwrite a different authority file: {path}")
        return
    path.parent.mkdir(parents=True, exist_ok=True)
    write_json_atomic(path, value)


def require_string(value: Any, label: str, *, pattern: re.Pattern[str] | None = None) -> str:
    if not isinstance(value, str) or not value:
        fail(f"{label} must be a non-empty string")
    if "\n" in value or "\r" in value or "\x00" in value:
        fail(f"{label} contains forbidden control characters")
    if pattern is not None and pattern.fullmatch(value) is None:
        fail(f"{label} has an invalid value: {value!r}")
    return value


def require_bool(value: Any, label: str) -> bool:
    if not isinstance(value, bool):
        fail(f"{label} must be a boolean")
    return value


def require_int(value: Any, label: str, *, minimum: int = 0) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value < minimum:
        fail(f"{label} must be an integer >= {minimum}")
    return value


def require_exact_keys(
    value: Any,
    allowed: set[str],
    required: set[str],
    label: str,
) -> dict[str, Any]:
    if not isinstance(value, dict):
        fail(f"{label} must be an object")
    missing = sorted(required.difference(value))
    unknown = sorted(set(value).difference(allowed))
    if missing:
        fail(f"{label} is missing required keys: {', '.join(missing)}")
    if unknown:
        fail(f"{label} contains unknown keys: {', '.join(unknown)}")
    return value


def _install_catalog_sort_key(entry: dict[str, Any]) -> tuple[int, int, int, int, str]:
    match = CODEX_VERSION.fullmatch(entry["codex_version"])
    assert match is not None
    major, minor, patch = (int(value) for value in match.groups())
    return (-major, -minor, -patch, -entry["patch_revision"], entry["compat_id"])


def validate_install_catalog(
    value: Any,
    *,
    expected_repository: str | None = None,
    expected_source_release_tag: str | None = None,
    expected_source_commit: str | None = None,
) -> dict[str, Any]:
    catalog = require_exact_keys(
        value,
        {"schema", "repository", "source_release_tag", "source_commit", "entries"},
        {"schema", "repository", "source_release_tag", "source_commit", "entries"},
        INSTALL_CATALOG_NAME,
    )
    if require_int(catalog.get("schema"), f"{INSTALL_CATALOG_NAME}.schema", minimum=1) != 1:
        fail("unsupported install catalog schema")
    repository = require_string(catalog.get("repository"), f"{INSTALL_CATALOG_NAME}.repository")
    source_tag = require_string(
        catalog.get("source_release_tag"), f"{INSTALL_CATALOG_NAME}.source_release_tag"
    )
    source_commit = require_string(
        catalog.get("source_commit"), f"{INSTALL_CATALOG_NAME}.source_commit", pattern=LOWER_SHA1
    )
    if expected_repository is not None and repository.casefold() != expected_repository.casefold():
        fail("install catalog repository differs from the expected repository")
    if expected_source_release_tag is not None and source_tag != expected_source_release_tag:
        fail("install catalog source release tag differs from the containing release")
    if expected_source_commit is not None and source_commit != expected_source_commit:
        fail("install catalog source commit differs from the containing release")

    entries = catalog.get("entries")
    if not isinstance(entries, list) or not 1 <= len(entries) <= INSTALL_CATALOG_MAX_ENTRIES:
        fail(f"install catalog entries must contain 1..{INSTALL_CATALOG_MAX_ENTRIES} items")
    seen_ids: set[str] = set()
    seen_tags: set[str] = set()
    normalized: list[dict[str, Any]] = []
    for index, raw in enumerate(entries):
        label = f"{INSTALL_CATALOG_NAME}.entries[{index}]"
        entry = require_exact_keys(
            raw,
            {
                "compat_id", "release_tag", "release_commit", "codex_version",
                "build_target", "patch_revision", "recorded_on",
            },
            {
                "compat_id", "release_tag", "release_commit", "codex_version",
                "build_target", "patch_revision", "recorded_on",
            },
            label,
        )
        compat_id = require_string(entry.get("compat_id"), f"{label}.compat_id", pattern=SAFE_ID)
        release_tag = require_string(entry.get("release_tag"), f"{label}.release_tag")
        release_commit = require_string(
            entry.get("release_commit"), f"{label}.release_commit", pattern=LOWER_SHA1
        )
        codex_version = require_string(entry.get("codex_version"), f"{label}.codex_version")
        version_match = CODEX_VERSION.fullmatch(codex_version)
        if version_match is None or ".".join(str(int(part)) for part in version_match.groups()) != codex_version:
            fail(f"{label}.codex_version must be canonical numeric X.Y.Z")
        build_target = require_string(entry.get("build_target"), f"{label}.build_target", pattern=SAFE_ID)
        patch_revision = require_int(entry.get("patch_revision"), f"{label}.patch_revision")
        revision_match = PATCH_REVISION.search(compat_id)
        if revision_match is None or int(revision_match.group(1)) != patch_revision:
            fail(f"{label}.patch_revision differs from the compatibility ID")
        if release_tag != f"compat-{compat_id}":
            fail(f"{label}.release_tag differs from the compatibility ID")
        recorded_on = require_string(entry.get("recorded_on"), f"{label}.recorded_on")
        try:
            if dt.date.fromisoformat(recorded_on).isoformat() != recorded_on:
                raise ValueError(recorded_on)
        except ValueError:
            fail(f"{label}.recorded_on must be a valid YYYY-MM-DD date")
        if compat_id in seen_ids or release_tag in seen_tags:
            fail("install catalog repeats a compatibility ID or release tag")
        seen_ids.add(compat_id)
        seen_tags.add(release_tag)
        normalized.append(
            {
                "compat_id": compat_id,
                "release_tag": release_tag,
                "release_commit": release_commit,
                "codex_version": codex_version,
                "build_target": build_target,
                "patch_revision": patch_revision,
                "recorded_on": recorded_on,
            }
        )
    if normalized != sorted(normalized, key=_install_catalog_sort_key):
        fail("install catalog entries are not in deterministic newest-first order")
    source_entries = [entry for entry in normalized if entry["release_tag"] == source_tag]
    if len(source_entries) != 1 or source_entries[0]["release_commit"] != source_commit:
        fail("install catalog source identity is not represented by exactly one entry")
    return {
        "schema": 1,
        "repository": repository,
        "source_release_tag": source_tag,
        "source_commit": source_commit,
        "entries": normalized,
    }


def safe_repo_file(repository: Path, value: Any, label: str, prefix: str) -> tuple[str, Path]:
    raw = require_string(value, label)
    pure = PurePosixPath(raw)
    if pure.is_absolute() or not pure.parts or any(part in ("", ".", "..") for part in pure.parts):
        fail(f"{label} must be a safe repository-relative POSIX path: {raw!r}")
    if pure.parts[0] != prefix:
        fail(f"{label} must live below {prefix}/: {raw!r}")
    candidate = (repository / Path(*pure.parts)).resolve()
    root = repository.resolve()
    try:
        candidate.relative_to(root)
    except ValueError:
        fail(f"{label} escapes the repository: {raw!r}")
    if not candidate.is_file():
        fail(f"{label} does not exist: {raw!r}")
    return pure.as_posix(), candidate


def load_manifest(repository: Path, path: Path) -> tuple[dict[str, Any], int | None, str | None]:
    """Load direct schema-1 manifests, falling back to CSA's p2 family loader."""
    try:
        raw = tomllib.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, tomllib.TOMLDecodeError) as error:
        fail(f"cannot read compatibility manifest {path}: {error}")
    if all(key in raw for key in ("compat_id", "codex_version", "upstream_tag", "upstream_commit", "artifacts")):
        schema = raw.get("schema")
        return raw, schema if isinstance(schema, int) else None, raw.get("family_id")

    scripts = repository / "scripts"
    sys.path.insert(0, str(scripts))
    try:
        from verify_patch_payload import _load_payload  # type: ignore

        payload = _load_payload(path)
    except Exception as error:  # noqa: BLE001 - preserve loader error in fail-closed message
        fail(f"cannot resolve family compatibility manifest {path}: {error}")
    finally:
        try:
            sys.path.remove(str(scripts))
        except ValueError:
            pass
    manifest = payload.manifest
    if not isinstance(manifest, dict):
        fail(f"resolved manifest is not an object: {path}")
    return manifest, getattr(payload, "source_schema", None), getattr(payload, "family_id", None)


def validate_sri(value: Any, label: str) -> str:
    text = require_string(value, label)
    try:
        algorithm, encoded = text.split("-", 1)
        decoded = base64.b64decode(encoded, validate=True)
    except (ValueError, base64.binascii.Error) as error:
        fail(f"{label} is not valid SRI: {error}")
    if algorithm != "sha512" or len(decoded) != 64:
        fail(f"{label} must be a sha512 SRI value")
    return text


def validate_download(value: Any, label: str, allowed_hosts: set[str]) -> dict[str, str]:
    if not isinstance(value, dict):
        fail(f"{label} must be an object")
    url = require_string(value.get("url"), f"{label}.url")
    sha256 = require_string(value.get("sha256"), f"{label}.sha256", pattern=LOWER_SHA256)
    parsed = urlparse(url)
    if parsed.scheme != "https" or parsed.hostname not in allowed_hosts or parsed.username or parsed.password:
        fail(f"{label}.url is outside the allowed HTTPS hosts: {url}")
    return {"url": url, "sha256": sha256}


def validate_archive_member(value: Any, label: str) -> str:
    raw = require_string(value, label)
    pure = PurePosixPath(raw)
    if pure.is_absolute() or not pure.parts or "\\" in raw or any(part in ("", ".", "..") for part in pure.parts):
        fail(f"{label} must be a safe relative archive path")
    return raw


def validate_profile(path: Path, target: str, manifest: dict[str, Any], artifact_filename: str) -> dict[str, Any]:
    profile = load_json(path)
    require_exact_keys(
        profile,
        {"schema", "id", "host", "target", "product", "rust", "tools", "xwin", "llvm", "build"},
        {"schema", "id", "host", "target", "product", "rust", "tools", "xwin", "llvm", "build"},
        str(path),
    )
    if profile.get("schema") != 1:
        fail(f"unsupported build profile schema in {path}")
    profile_id = require_string(profile.get("id"), f"{path}.id", pattern=SAFE_ID)
    if require_string(profile.get("target"), f"{path}.target") != target:
        fail(f"build profile target differs from selected target: {path}")
    if require_string(profile.get("host"), f"{path}.host") != "x86_64-unknown-linux-gnu":
        fail(f"only the reviewed Linux x64 cross-build host is supported: {path}")

    product = profile.get("product")
    product = require_exact_keys(
        product,
        {"cargo_package", "cargo_bin", "artifact_filename"},
        {"cargo_package", "cargo_bin", "artifact_filename"},
        f"{path}.product",
    )
    if require_string(product.get("cargo_package"), f"{path}.product.cargo_package") != "codex-cli":
        fail("patched release profile must build only cargo package codex-cli")
    if require_string(product.get("cargo_bin"), f"{path}.product.cargo_bin") != "codex":
        fail("patched release profile must build only binary codex")
    if require_string(product.get("artifact_filename"), f"{path}.product.artifact_filename") != artifact_filename:
        fail("build profile artifact filename differs from compatibility manifest")

    rust = profile.get("rust")
    rust = require_exact_keys(
        rust,
        {"toolchain", "rustc_commit", "rustup_version", "rustup_init"},
        {"toolchain", "rustc_commit", "rustup_version", "rustup_init"},
        f"{path}.rust",
    )
    rust_toolchain = require_string(rust.get("toolchain"), f"{path}.rust.toolchain")
    rustc_commit = require_string(rust.get("rustc_commit"), f"{path}.rust.rustc_commit", pattern=LOWER_SHA1)
    if rust_toolchain != require_string(manifest.get("rust_toolchain"), "manifest.rust_toolchain"):
        fail("build profile Rust toolchain differs from compatibility manifest")
    if rustc_commit != require_string(manifest.get("rustc_commit"), "manifest.rustc_commit", pattern=LOWER_SHA1):
        fail("build profile rustc commit differs from compatibility manifest")
    rustup = validate_download(rust.get("rustup_init"), f"{path}.rust.rustup_init", {"static.rust-lang.org"})
    rustup["version"] = require_string(rust.get("rustup_version"), f"{path}.rust.rustup_version")

    tools = profile.get("tools")
    tools = require_exact_keys(tools, {"cargo_xwin", "sccache"}, {"cargo_xwin", "sccache"}, f"{path}.tools")
    cargo_xwin_value = require_exact_keys(
        tools.get("cargo_xwin"),
        {"version", "url", "sha256", "archive_member"},
        {"version", "url", "sha256", "archive_member"},
        f"{path}.tools.cargo_xwin",
    )
    cargo_xwin = validate_download(cargo_xwin_value, f"{path}.tools.cargo_xwin", {"github.com"})
    cargo_xwin["version"] = require_string(cargo_xwin_value.get("version"), f"{path}.tools.cargo_xwin.version")
    cargo_xwin["archive_member"] = validate_archive_member(
        cargo_xwin_value.get("archive_member"), f"{path}.tools.cargo_xwin.archive_member"
    )
    sccache_value = require_exact_keys(
        tools.get("sccache"),
        {"version", "url", "sha256", "archive_member"},
        {"version", "url", "sha256", "archive_member"},
        f"{path}.tools.sccache",
    )
    sccache = validate_download(sccache_value, f"{path}.tools.sccache", {"github.com"})
    sccache["version"] = require_string(sccache_value.get("version"), f"{path}.tools.sccache.version")
    sccache["archive_member"] = validate_archive_member(
        sccache_value.get("archive_member"), f"{path}.tools.sccache.archive_member"
    )

    xwin = profile.get("xwin")
    xwin = require_exact_keys(xwin, {"version", "arch", "variant"}, {"version", "arch", "variant"}, f"{path}.xwin")
    xwin_normalized = {
        "version": require_string(xwin.get("version"), f"{path}.xwin.version"),
        "arch": require_string(xwin.get("arch"), f"{path}.xwin.arch"),
        "variant": require_string(xwin.get("variant"), f"{path}.xwin.variant"),
    }

    llvm = profile.get("llvm")
    llvm = require_exact_keys(
        llvm,
        {"version", "major", "apt_key_url", "apt_key_fingerprint", "apt_repository"},
        {"version", "major", "apt_key_url", "apt_key_fingerprint", "apt_repository"},
        f"{path}.llvm",
    )
    llvm_normalized = {
        "version": require_string(llvm.get("version"), f"{path}.llvm.version"),
        "major": require_int(llvm.get("major"), f"{path}.llvm.major", minimum=1),
        "apt_key_url": require_string(llvm.get("apt_key_url"), f"{path}.llvm.apt_key_url"),
        "apt_key_fingerprint": require_string(
            llvm.get("apt_key_fingerprint"), f"{path}.llvm.apt_key_fingerprint"
        ),
        "apt_repository": require_string(llvm.get("apt_repository"), f"{path}.llvm.apt_repository"),
    }
    parsed_key = urlparse(llvm_normalized["apt_key_url"])
    if parsed_key.scheme != "https" or parsed_key.hostname != "apt.llvm.org":
        fail("LLVM apt key URL must use https://apt.llvm.org")
    if "apt.llvm.org" not in llvm_normalized["apt_repository"]:
        fail("LLVM apt repository must use apt.llvm.org")

    build = profile.get("build")
    build = require_exact_keys(
        build,
        {"cargo_build_jobs", "cargo_incremental", "sccache_cache_size"},
        {"cargo_build_jobs", "cargo_incremental", "sccache_cache_size"},
        f"{path}.build",
    )
    build_normalized = {
        "cargo_build_jobs": require_int(build.get("cargo_build_jobs"), f"{path}.build.cargo_build_jobs", minimum=1),
        "cargo_incremental": require_int(build.get("cargo_incremental"), f"{path}.build.cargo_incremental", minimum=0),
        "sccache_cache_size": require_string(build.get("sccache_cache_size"), f"{path}.build.sccache_cache_size"),
    }
    if build_normalized["cargo_incremental"] != 0:
        fail("CI/release build profile must keep CARGO_INCREMENTAL=0")

    return {
        "schema": 1,
        "id": profile_id,
        "host": profile["host"],
        "target": target,
        "product": {
            "cargo_package": product["cargo_package"],
            "cargo_bin": product["cargo_bin"],
            "artifact_filename": artifact_filename,
        },
        "rust": {
            "toolchain": rust_toolchain,
            "rustc_commit": rustc_commit,
            "rustup_init": rustup,
        },
        "tools": {"cargo_xwin": cargo_xwin, "sccache": sccache},
        "xwin": xwin_normalized,
        "llvm": llvm_normalized,
        "build": build_normalized,
    }


def validate_runtime_lock(path: Path, compat_id: str, codex_version: str, target: str) -> dict[str, Any]:
    lock = load_json(path)
    require_exact_keys(
        lock,
        {"schema", "compat_id", "codex_version", "target", "package", "archive_url", "integrity", "required_files"},
        {"schema", "compat_id", "codex_version", "target", "package", "archive_url", "integrity", "required_files"},
        str(path),
    )
    if lock.get("schema") != 1:
        fail(f"unsupported runtime lock schema in {path}")
    if require_string(lock.get("compat_id"), f"{path}.compat_id", pattern=SAFE_ID) != compat_id:
        fail("runtime lock compatibility ID differs from catalog entry")
    if require_string(lock.get("codex_version"), f"{path}.codex_version") != codex_version:
        fail("runtime lock Codex version differs from manifest")
    if require_string(lock.get("target"), f"{path}.target") != target:
        fail("runtime lock target differs from selected target")
    package = require_string(lock.get("package"), f"{path}.package")
    if package != "@openai/codex":
        fail("runtime lock package must be @openai/codex")
    archive_url = require_string(lock.get("archive_url"), f"{path}.archive_url")
    parsed = urlparse(archive_url)
    expected_suffix = f"/codex-{codex_version}-win32-x64.tgz"
    if parsed.scheme != "https" or parsed.hostname != "registry.npmjs.org" or not parsed.path.endswith(expected_suffix):
        fail("runtime lock archive URL does not match the exact official Windows x64 Codex package")
    integrity = validate_sri(lock.get("integrity"), f"{path}.integrity")
    required_files = lock.get("required_files")
    if not isinstance(required_files, list) or not required_files:
        fail(f"{path}.required_files must be a non-empty array")
    normalized: list[str] = []
    for index, item in enumerate(required_files):
        raw = require_string(item, f"{path}.required_files[{index}]")
        pure = PurePosixPath(raw)
        if pure.is_absolute() or any(part in ("", ".", "..") for part in pure.parts):
            fail(f"unsafe runtime archive member: {raw!r}")
        if not raw.startswith("package/vendor/x86_64-pc-windows-msvc/"):
            fail(f"runtime member is outside the reviewed Windows target layout: {raw!r}")
        normalized.append(raw)
    if len(set(normalized)) != len(normalized):
        fail("runtime lock repeats archive members")
    return {
        "schema": 1,
        "compat_id": compat_id,
        "codex_version": codex_version,
        "target": target,
        "package": package,
        "archive_url": archive_url,
        "integrity": integrity,
        "required_files": normalized,
    }


def validate_acceptance(
    path: Path,
    compat_id: str,
    target: str,
    artifact_filename: str,
    manifest_sha256: str,
    profile_sha256: str,
    runtime_sha256: str,
) -> dict[str, Any]:
    acceptance = load_json(path)
    require_exact_keys(
        acceptance,
        {
            "schema", "status", "compat_id", "target", "artifact_filename", "artifact_sha256",
            "artifact_size", "manifest_sha256", "build_profile_sha256", "runtime_lock_sha256",
            "recorded_at", "accepted_by", "evidence",
        },
        {
            "schema", "status", "compat_id", "target", "artifact_filename", "artifact_sha256",
            "artifact_size", "manifest_sha256", "build_profile_sha256", "runtime_lock_sha256", "evidence",
        },
        str(path),
    )
    if acceptance.get("schema") != 1:
        fail(f"unsupported acceptance schema in {path}")
    checks = {
        "compat_id": compat_id,
        "target": target,
        "artifact_filename": artifact_filename,
        "manifest_sha256": manifest_sha256,
        "build_profile_sha256": profile_sha256,
        "runtime_lock_sha256": runtime_sha256,
    }
    for key, expected in checks.items():
        actual = require_string(acceptance.get(key), f"{path}.{key}")
        if actual != expected:
            fail(f"acceptance {key} differs from the reviewed build identity")
    require_string(acceptance.get("artifact_sha256"), f"{path}.artifact_sha256", pattern=LOWER_SHA256)
    require_int(acceptance.get("artifact_size"), f"{path}.artifact_size", minimum=1)
    if require_string(acceptance.get("status"), f"{path}.status") != "accepted":
        fail("acceptance record is not in accepted state")
    evidence = acceptance.get("evidence")
    if not isinstance(evidence, dict):
        fail(f"{path}.evidence must be an object")
    return acceptance


def resolve(
    repository: Path,
    selector: str,
    target: str,
    *,
    require_acceptance: bool = False,
    require_release: bool = False,
) -> dict[str, Any]:
    repository = repository.resolve()
    index_path = repository / "release" / "compatibility-index.json"
    index = load_json(index_path)
    require_exact_keys(
        index,
        {"schema", "current", "compatibilities"},
        {"schema", "current", "compatibilities"},
        "compatibility index",
    )
    if index.get("schema") != 1:
        fail("release/compatibility-index.json has an unsupported schema")
    current = index.get("current")
    entries = index.get("compatibilities")
    if not isinstance(current, dict) or not isinstance(entries, dict):
        fail("compatibility index must contain current and compatibilities objects")

    selector = require_string(selector, "selector")
    target = require_string(target, "target")
    if selector == "current":
        compat_id = require_string(current.get(target), f"current[{target}]", pattern=SAFE_ID)
    else:
        compat_id = require_string(selector, "selector", pattern=SAFE_ID)
    entry = entries.get(compat_id)
    if not isinstance(entry, dict):
        fail(f"unknown compatibility selector: {compat_id}")
    require_exact_keys(
        entry,
        {
            "lifecycle", "build_enabled", "release_enabled", "manifest",
            "manifest_sha256", "targets",
        },
        {
            "lifecycle", "build_enabled", "release_enabled", "manifest",
            "manifest_sha256", "targets",
        },
        f"compatibility entry {compat_id}",
    )
    lifecycle = require_string(entry.get("lifecycle"), f"{compat_id}.lifecycle")
    if lifecycle not in {"candidate", "accepted", "published", "legacy"}:
        fail(f"unsupported compatibility lifecycle: {compat_id}/{lifecycle}")
    build_enabled = require_bool(entry.get("build_enabled"), f"{compat_id}.build_enabled")
    release_enabled = require_bool(entry.get("release_enabled"), f"{compat_id}.release_enabled")
    if not build_enabled:
        fail(f"compatibility is not enabled for heavy builds: {compat_id}")
    if require_release and not release_enabled:
        fail(f"compatibility is not enabled for formal release: {compat_id}")

    manifest_rel, manifest_path = safe_repo_file(
        repository, entry.get("manifest"), f"{compat_id}.manifest", "payload"
    )
    manifest_sha256 = sha256_file(manifest_path)
    expected_manifest_sha256 = require_string(
        entry.get("manifest_sha256"), f"{compat_id}.manifest_sha256", pattern=LOWER_SHA256
    )
    if manifest_sha256 != expected_manifest_sha256:
        fail(f"catalog manifest binding drifted for {compat_id}")
    manifest, source_schema, family_id = load_manifest(repository, manifest_path)
    if require_string(manifest.get("compat_id"), "manifest.compat_id", pattern=SAFE_ID) != compat_id:
        fail("catalog compatibility ID differs from the resolved manifest")
    codex_version = require_string(manifest.get("codex_version"), "manifest.codex_version")
    upstream_tag = require_string(manifest.get("upstream_tag"), "manifest.upstream_tag")
    upstream_commit = require_string(manifest.get("upstream_commit"), "manifest.upstream_commit", pattern=LOWER_SHA1)
    if upstream_tag != f"rust-v{codex_version}":
        fail("manifest upstream tag is not rust-v<codex_version>")
    manifest_target = require_string(manifest.get("build_target"), "manifest.build_target")
    if manifest_target != target:
        fail(f"selected target {target} differs from manifest target {manifest_target}")
    artifacts = manifest.get("artifacts")
    if not isinstance(artifacts, dict) or not isinstance(artifacts.get(target), dict):
        fail(f"manifest does not define an artifact for {target}")
    artifact = artifacts[target]
    artifact_filename = require_string(artifact.get("filename"), "manifest.artifact.filename")
    artifact_sha256 = require_string(artifact.get("sha256"), "manifest.artifact.sha256", pattern=LOWER_SHA256)
    artifact_size = require_int(artifact.get("size"), "manifest.artifact.size", minimum=1)
    artifact_url = require_string(artifact.get("url"), "manifest.artifact.url")
    parsed_artifact_url = urlparse(artifact_url)
    release_tag = f"compat-{compat_id}"
    expected_prefix = f"/dslzl/csa/releases/download/{release_tag}/"
    expected_unpublished = f"unpublished://csa/{compat_id}/{target}/{artifact_filename}"
    if artifact_url == expected_unpublished and not release_enabled:
        artifact_asset = f"{compat_id}--{artifact_filename}"
    else:
        if (
            parsed_artifact_url.scheme != "https"
            or parsed_artifact_url.hostname != "github.com"
            or not parsed_artifact_url.path.lower().startswith(expected_prefix.lower())
        ):
            fail("manifest artifact URL is outside the exact CSA compatibility release")
        artifact_asset = PurePosixPath(parsed_artifact_url.path).name
    if not artifact_asset or not SAFE_ID.fullmatch(artifact_asset):
        fail("manifest artifact URL has an unsafe release asset name")

    targets = entry.get("targets")
    if not isinstance(targets, dict) or not isinstance(targets.get(target), dict):
        fail(f"catalog entry does not route target {target}: {compat_id}")
    target_entry = targets[target]
    require_exact_keys(
        target_entry,
        {
            "build_profile", "build_profile_sha256", "runtime_lock",
            "runtime_lock_sha256", "acceptance", "acceptance_sha256",
        },
        {
            "build_profile", "build_profile_sha256", "runtime_lock",
            "runtime_lock_sha256", "acceptance", "acceptance_sha256",
        },
        f"target route {compat_id}/{target}",
    )
    profile_rel, profile_path = safe_repo_file(
        repository, target_entry.get("build_profile"), f"{compat_id}.targets[{target}].build_profile", "release"
    )
    runtime_rel, runtime_path = safe_repo_file(
        repository, target_entry.get("runtime_lock"), f"{compat_id}.targets[{target}].runtime_lock", "release"
    )
    profile_sha256 = sha256_file(profile_path)
    runtime_sha256 = sha256_file(runtime_path)
    if profile_sha256 != require_string(
        target_entry.get("build_profile_sha256"),
        f"{compat_id}.targets[{target}].build_profile_sha256",
        pattern=LOWER_SHA256,
    ):
        fail(f"catalog build-profile binding drifted for {compat_id}/{target}")
    if runtime_sha256 != require_string(
        target_entry.get("runtime_lock_sha256"),
        f"{compat_id}.targets[{target}].runtime_lock_sha256",
        pattern=LOWER_SHA256,
    ):
        fail(f"catalog runtime-lock binding drifted for {compat_id}/{target}")
    profile = validate_profile(profile_path, target, manifest, artifact_filename)
    runtime = validate_runtime_lock(runtime_path, compat_id, codex_version, target)
    acceptance_rel = ""
    acceptance_sha256 = ""
    acceptance: dict[str, Any] | None = None
    acceptance_value = target_entry.get("acceptance")
    if acceptance_value is not None:
        acceptance_rel, acceptance_path = safe_repo_file(
            repository, acceptance_value, f"{compat_id}.targets[{target}].acceptance", "release"
        )
        acceptance_sha256 = sha256_file(acceptance_path)
        expected_acceptance_sha256 = require_string(
            target_entry.get("acceptance_sha256"),
            f"{compat_id}.targets[{target}].acceptance_sha256",
            pattern=LOWER_SHA256,
        )
        if acceptance_sha256 != expected_acceptance_sha256:
            fail(f"catalog acceptance binding drifted for {compat_id}/{target}")
        acceptance = validate_acceptance(
            acceptance_path,
            compat_id,
            target,
            artifact_filename,
            manifest_sha256,
            profile_sha256,
            runtime_sha256,
        )
    else:
        if target_entry.get("acceptance_sha256") is not None:
            fail(f"acceptance_sha256 must be null when acceptance is absent: {compat_id}/{target}")
        if require_acceptance:
            fail(f"acceptance verification requires a committed acceptance record: {compat_id}/{target}")

    result: dict[str, Any] = {
        "schema": 1,
        "repository_root": str(repository),
        "catalog_path": "release/compatibility-index.json",
        "catalog_sha256": sha256_file(index_path),
        "compat_id": compat_id,
        "selector": selector,
        "lifecycle": lifecycle,
        "build_enabled": build_enabled,
        "release_enabled": release_enabled,
        "release_tag": release_tag,
        "manifest_path": manifest_rel,
        "manifest_sha256": manifest_sha256,
        "manifest_source_schema": source_schema,
        "manifest_family_id": family_id,
        "codex_version": codex_version,
        "upstream_tag": upstream_tag,
        "upstream_commit": upstream_commit,
        "rust_toolchain": require_string(manifest.get("rust_toolchain"), "manifest.rust_toolchain"),
        "rustc_commit": require_string(manifest.get("rustc_commit"), "manifest.rustc_commit", pattern=LOWER_SHA1),
        "build_target": target,
        "artifact_filename": artifact_filename,
        "artifact_asset": artifact_asset,
        "artifact_sha256": artifact_sha256,
        "artifact_size": artifact_size,
        "artifact_url": artifact_url,
        "build_profile_path": profile_rel,
        "build_profile_sha256": profile_sha256,
        "build_profile": profile,
        "runtime_lock_path": runtime_rel,
        "runtime_lock_sha256": runtime_sha256,
        "runtime": runtime,
        "acceptance_path": acceptance_rel,
        "acceptance_sha256": acceptance_sha256,
        "acceptance": acceptance,
        "accepted_artifact_sha256": acceptance.get("artifact_sha256", "") if acceptance else "",
        "accepted_artifact_size": acceptance.get("artifact_size", 0) if acceptance else 0,
        "cargo_xwin_version": profile["tools"]["cargo_xwin"]["version"],
        "sccache_version": profile["tools"]["sccache"]["version"],
        "xwin_version": profile["xwin"]["version"],
        "llvm_version": profile["llvm"]["version"],
    }
    return result


def scalar(value: Any) -> str:
    if isinstance(value, bool):
        return "true" if value else "false"
    if value is None:
        return ""
    return str(value)


def write_github_output(path: Path, result: dict[str, Any]) -> None:
    with path.open("a", encoding="utf-8", newline="\n") as stream:
        for key in OUTPUT_KEYS:
            stream.write(f"{key}={scalar(result.get(key, ''))}\n")


def write_bash_env(path: Path, result: dict[str, Any]) -> None:
    repository = Path(result["repository_root"])
    values = {
        "CSA_COMPAT_ID": result["compat_id"],
        "CSA_RELEASE_TAG": result["release_tag"],
        "CSA_CODEX_VERSION": result["codex_version"],
        "CSA_UPSTREAM_TAG": result["upstream_tag"],
        "CSA_UPSTREAM_COMMIT": result["upstream_commit"],
        "CSA_MANIFEST": str((repository / result["manifest_path"]).resolve()),
        "CSA_MANIFEST_SHA256": result["manifest_sha256"],
        "CSA_BUILD_TARGET": result["build_target"],
        "CSA_ARTIFACT_FILENAME": result["artifact_filename"],
        "CSA_ARTIFACT_SHA256": result["artifact_sha256"],
        "CSA_ARTIFACT_SIZE": result["artifact_size"],
        "CSA_BUILD_PROFILE": str((repository / result["build_profile_path"]).resolve()),
        "CSA_BUILD_PROFILE_SHA256": result["build_profile_sha256"],
        "CSA_RUNTIME_LOCK": str((repository / result["runtime_lock_path"]).resolve()),
        "CSA_RUNTIME_LOCK_SHA256": result["runtime_lock_sha256"],
        "CSA_ACCEPTANCE": (
            str((repository / result["acceptance_path"]).resolve()) if result["acceptance_path"] else ""
        ),
        "CSA_ACCEPTED_ARTIFACT_SHA256": result["accepted_artifact_sha256"],
        "CSA_ACCEPTED_ARTIFACT_SIZE": result["accepted_artifact_size"],
    }
    with path.open("a", encoding="utf-8", newline="\n") as stream:
        for key, value in values.items():
            stream.write(f"export {key}={shlex.quote(str(value))}\n")


def validate_all(repository: Path) -> list[dict[str, Any]]:
    index = load_json(repository / "release" / "compatibility-index.json")
    entries = index.get("compatibilities")
    if not isinstance(entries, dict):
        fail("compatibility index has no compatibilities object")
    results: list[dict[str, Any]] = []
    for compat_id in sorted(entries):
        entry = entries[compat_id]
        if not isinstance(entry, dict) or not isinstance(entry.get("targets"), dict):
            fail(f"invalid compatibility entry: {compat_id}")
        for target in sorted(entry["targets"]):
            results.append(resolve(repository, compat_id, target))
    current = index.get("current", {})
    if not isinstance(current, dict):
        fail("current must be an object")
    for target in sorted(current):
        current_result = resolve(repository, "current", target)
        if current_result["release_enabled"]:
            resolve(repository, "current", target, require_acceptance=True, require_release=True)
    return results


def _git_tag_commit(repository: Path, tag: str) -> str | None:
    completed = subprocess.run(
        ["git", "-C", str(repository), "rev-parse", "--verify", f"{tag}^{{commit}}"],
        check=False,
        capture_output=True,
        text=True,
    )
    if completed.returncode != 0:
        return None
    commit = completed.stdout.strip()
    if LOWER_SHA1.fullmatch(commit) is None:
        fail(f"Git returned an invalid commit for {tag}")
    return commit


def build_install_catalog(
    repository: Path,
    formal_tags_path: Path,
    current_release_tag: str,
    current_source_commit: str,
    target: str,
) -> dict[str, Any]:
    repository = repository.resolve(strict=True)
    current_release_tag = require_string(current_release_tag, "current release tag")
    if not current_release_tag.startswith("compat-") or SAFE_ID.fullmatch(current_release_tag[7:]) is None:
        fail("current release tag must be compat-<safe-compat-id>")
    current_source_commit = require_string(
        current_source_commit, "current source commit", pattern=LOWER_SHA1
    )
    target = require_string(target, "target", pattern=SAFE_ID)
    try:
        tags = [line.strip() for line in formal_tags_path.read_text(encoding="utf-8").splitlines() if line.strip()]
    except (OSError, UnicodeError) as error:
        fail(f"cannot read formal release tags: {error}")
    if len(tags) != len(set(tags)):
        fail("formal release tag input contains duplicates")
    formal_tags = set(tags)
    formal_tags.add(current_release_tag)

    existing_current = _git_tag_commit(repository, current_release_tag)
    if existing_current is not None and existing_current != current_source_commit:
        fail("current release tag already resolves to a different commit")

    index = load_json(repository / "release" / "compatibility-index.json")
    entries = index.get("compatibilities")
    if not isinstance(entries, dict):
        fail("compatibility index has no compatibilities object")
    catalog_entries: list[dict[str, Any]] = []
    for compat_id, index_entry in entries.items():
        release_tag = f"compat-{compat_id}"
        if (
            not isinstance(index_entry, dict)
            or index_entry.get("release_enabled") is not True
            or release_tag not in formal_tags
        ):
            continue
        resolved = resolve(
            repository,
            compat_id,
            target,
            require_acceptance=True,
            require_release=True,
        )
        release_commit = (
            current_source_commit
            if release_tag == current_release_tag
            else _git_tag_commit(repository, release_tag)
        )
        if release_commit is None:
            fail(f"formal compatibility release tag is missing locally: {release_tag}")
        acceptance = resolved.get("acceptance")
        if not isinstance(acceptance, dict):
            fail(f"formal compatibility has no acceptance record: {compat_id}")
        revision_match = PATCH_REVISION.search(compat_id)
        if revision_match is None:
            fail(f"compatibility ID has no numeric patch revision: {compat_id}")
        catalog_entries.append(
            {
                "compat_id": compat_id,
                "release_tag": release_tag,
                "release_commit": release_commit,
                "codex_version": resolved["codex_version"],
                "build_target": target,
                "patch_revision": int(revision_match.group(1)),
                "recorded_on": require_string(
                    acceptance.get("recorded_at"), f"acceptance recorded_at for {compat_id}"
                ),
            }
        )
    catalog_entries.sort(key=_install_catalog_sort_key)
    catalog = {
        "schema": 1,
        "repository": CSA_REPOSITORY,
        "source_release_tag": current_release_tag,
        "source_commit": current_source_commit,
        "entries": catalog_entries,
    }
    return validate_install_catalog(
        catalog,
        expected_repository=CSA_REPOSITORY,
        expected_source_release_tag=current_release_tag,
        expected_source_commit=current_source_commit,
    )



def new_repo_file(repository: Path, value: Path, label: str, prefix: str) -> tuple[str, Path]:
    path = value if value.is_absolute() else repository / value
    path = path.resolve()
    try:
        relative = path.relative_to(repository).as_posix()
    except ValueError:
        fail(f"{label} must stay inside the repository")
    pure = PurePosixPath(relative)
    if not pure.parts or pure.parts[0] != prefix or any(part in {"", ".", ".."} for part in pure.parts):
        fail(f"{label} must be a normalized path below {prefix}/")
    return relative, path


def official_npm_integrity(codex_version: str) -> str:
    package = urllib.parse.quote("@openai/codex", safe="")
    url = f"https://registry.npmjs.org/{package}/{codex_version}-win32-x64"
    request = urllib.request.Request(
        url,
        headers={"Accept": "application/json", "User-Agent": "csa-compat-catalog/1"},
    )
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            if response.geturl() != url:
                fail("official npm metadata request redirected unexpectedly")
            payload = json.load(response)
    except (OSError, ValueError, urllib.error.URLError, json.JSONDecodeError) as error:
        fail(f"cannot query official npm package metadata: {error}")
    if not isinstance(payload, dict) or not isinstance(payload.get("dist"), dict):
        fail("official npm metadata has no dist object")
    return validate_sri(payload["dist"].get("integrity"), "official npm integrity")


def stage_candidate(
    repository: Path,
    manifest_value: Path,
    target: str,
    runtime_lock_value: Path,
    npm_integrity_value: str,
) -> dict[str, Any]:
    index_path = repository / "release" / "compatibility-index.json"
    index = load_json(index_path)
    if index.get("schema") != 1 or not isinstance(index.get("compatibilities"), dict):
        fail("cannot stage a candidate into an invalid compatibility index")
    entries: dict[str, Any] = index["compatibilities"]

    manifest_path = manifest_value if manifest_value.is_absolute() else repository / manifest_value
    manifest_path = manifest_path.resolve(strict=True)
    try:
        manifest_relative = manifest_path.relative_to(repository).as_posix()
    except ValueError:
        fail("candidate manifest must stay inside the repository")
    if not manifest_relative.startswith("payload/"):
        fail("candidate manifest must live below payload/")
    manifest, _, _ = load_manifest(repository, manifest_path)
    compat_id = require_string(manifest.get("compat_id"), "candidate manifest.compat_id", pattern=SAFE_ID)
    if compat_id in entries:
        fail(f"compatibility is already registered: {compat_id}")
    codex_version = require_string(manifest.get("codex_version"), "candidate manifest.codex_version")
    manifest_target = require_string(manifest.get("build_target"), "candidate manifest.build_target")
    if manifest_target != target:
        fail("candidate target differs from the manifest")

    current = index.get("current")
    if not isinstance(current, dict) or target not in current:
        fail(f"no reviewed build profile route exists for target {target}")
    current_entry = entries.get(current[target])
    if not isinstance(current_entry, dict):
        fail(f"current route is invalid for target {target}")
    current_targets = current_entry.get("targets")
    if not isinstance(current_targets, dict) or not isinstance(current_targets.get(target), dict):
        fail(f"current target route is invalid for {target}")
    profile_source = current_targets[target]
    profile_rel, profile_path = safe_repo_file(
        repository,
        profile_source.get("build_profile"),
        f"current.targets[{target}].build_profile",
        "release",
    )
    profile_sha = sha256_file(profile_path)
    if profile_sha != require_string(
        profile_source.get("build_profile_sha256"),
        f"current.targets[{target}].build_profile_sha256",
        pattern=LOWER_SHA256,
    ):
        fail("current build-profile binding drifted")
    artifact = manifest.get("artifacts", {}).get(target)
    if not isinstance(artifact, dict):
        fail(f"candidate manifest has no artifact contract for {target}")
    artifact_filename = require_string(artifact.get("filename"), "candidate artifact.filename")
    validate_profile(profile_path, target, manifest, artifact_filename)

    runtime_relative, runtime_path = new_repo_file(
        repository, runtime_lock_value, "candidate runtime lock", "release"
    )
    if runtime_path.exists():
        fail(f"candidate runtime lock already exists: {runtime_path}")
    integrity = (
        official_npm_integrity(codex_version)
        if npm_integrity_value == "registry"
        else validate_sri(npm_integrity_value, "candidate npm integrity")
    )
    runtime = {
        "schema": 1,
        "compat_id": compat_id,
        "codex_version": codex_version,
        "target": target,
        "package": "@openai/codex",
        "archive_url": (
            "https://registry.npmjs.org/@openai/codex/-/"
            f"codex-{codex_version}-win32-x64.tgz"
        ),
        "integrity": integrity,
        "required_files": [
            "package/vendor/x86_64-pc-windows-msvc/bin/codex-code-mode-host.exe",
            "package/vendor/x86_64-pc-windows-msvc/codex-resources/codex-command-runner.exe",
            "package/vendor/x86_64-pc-windows-msvc/codex-resources/codex-windows-sandbox-setup.exe",
            "package/vendor/x86_64-pc-windows-msvc/codex-path/rg.exe",
        ],
    }
    write_new_or_equal(runtime_path, runtime)
    runtime_sha = sha256_file(runtime_path)
    entry = {
        "lifecycle": "candidate",
        "build_enabled": True,
        "release_enabled": False,
        "manifest": manifest_relative,
        "manifest_sha256": sha256_file(manifest_path),
        "targets": {
            target: {
                "build_profile": profile_rel,
                "build_profile_sha256": profile_sha,
                "runtime_lock": runtime_relative,
                "runtime_lock_sha256": runtime_sha,
                "acceptance": None,
                "acceptance_sha256": None,
            }
        },
    }
    entries[compat_id] = entry
    index["compatibilities"] = dict(sorted(entries.items()))
    write_json_atomic(index_path, index)
    return {
        "schema": 1,
        "result": "candidate_registered",
        "compat_id": compat_id,
        "manifest": manifest_relative,
        "runtime_lock": runtime_relative,
        "target": target,
    }


def create_candidate_record(
    repository: Path,
    resolution_path: Path,
    artifact_value: Path,
    output: Path,
    provider: str,
    pipeline: str | None,
    job: str | None,
    source_commit: str | None,
) -> dict[str, Any]:
    resolution = load_json(resolution_path.resolve(strict=True))
    if resolution.get("schema") != 1:
        fail("candidate resolution has an unsupported schema")
    compat_id = require_string(resolution.get("compat_id"), "candidate compat_id", pattern=SAFE_ID)
    target = require_string(resolution.get("build_target"), "candidate target")
    artifact = artifact_value.resolve(strict=True)
    expected_name = require_string(resolution.get("artifact_filename"), "candidate artifact filename")
    if not artifact.is_file() or artifact.name != expected_name:
        fail("candidate artifact filename differs from the resolved compatibility")
    record = {
        "schema": 1,
        "compat_id": compat_id,
        "target": target,
        "manifest_sha256": require_string(
            resolution.get("manifest_sha256"), "candidate manifest_sha256", pattern=LOWER_SHA256
        ),
        "runtime_lock_sha256": require_string(
            resolution.get("runtime_lock_sha256"), "candidate runtime_lock_sha256", pattern=LOWER_SHA256
        ),
        "build_profile_sha256": require_string(
            resolution.get("build_profile_sha256"), "candidate build_profile_sha256", pattern=LOWER_SHA256
        ),
        "upstream": {
            "tag": require_string(resolution.get("upstream_tag"), "candidate upstream tag"),
            "commit": require_string(
                resolution.get("upstream_commit"), "candidate upstream commit", pattern=LOWER_SHA1
            ),
        },
        "artifact": {
            "filename": artifact.name,
            "sha256": sha256_file(artifact),
            "size": artifact.stat().st_size,
        },
        "provenance": {
            key: value
            for key, value in {
                "provider": provider,
                "pipeline": pipeline,
                "job": job,
                "source_commit": source_commit,
            }.items()
            if value is not None
        },
    }
    if output.exists():
        fail(f"candidate record output already exists: {output}")
    write_json_atomic(output, record)
    return record


def accept_candidate(
    repository: Path,
    selector: str,
    target: str,
    candidate_record_path: Path,
    artifact_value: Path,
    acceptance_value: Path,
    evidence_path: Path,
    make_current: bool,
    accepted_by: str | None,
) -> dict[str, Any]:
    if selector in {"current", "all"}:
        fail("accept requires one exact compatibility ID")
    index_path = repository / "release" / "compatibility-index.json"
    index = load_json(index_path)
    entries = index.get("compatibilities")
    if not isinstance(entries, dict) or not isinstance(entries.get(selector), dict):
        fail(f"unknown compatibility selector: {selector}")
    entry = entries[selector]
    if entry.get("lifecycle") != "candidate" or entry.get("release_enabled") is not False:
        fail("only a non-releasable candidate can be accepted")
    routes = entry.get("targets")
    if not isinstance(routes, dict) or not isinstance(routes.get(target), dict):
        fail(f"candidate does not route target {target}")
    route = routes[target]
    if route.get("acceptance") is not None or route.get("acceptance_sha256") is not None:
        fail("candidate already has an acceptance authority")

    candidate = load_json(candidate_record_path.resolve(strict=True))
    if candidate.get("schema") != 1 or candidate.get("compat_id") != selector or candidate.get("target") != target:
        fail("candidate record identity differs")
    old_manifest_sha = require_string(entry.get("manifest_sha256"), "candidate manifest binding", pattern=LOWER_SHA256)
    if candidate.get("manifest_sha256") != old_manifest_sha:
        fail("candidate record was produced from a different manifest revision")
    for key, catalog_key in (
        ("runtime_lock_sha256", "runtime_lock_sha256"),
        ("build_profile_sha256", "build_profile_sha256"),
    ):
        expected = require_string(route.get(catalog_key), f"candidate route {catalog_key}", pattern=LOWER_SHA256)
        if candidate.get(key) != expected:
            fail(f"candidate record differs from reviewed {catalog_key}")

    manifest_rel, manifest_path = safe_repo_file(
        repository, entry.get("manifest"), f"{selector}.manifest", "payload"
    )
    manifest, _, _ = load_manifest(repository, manifest_path)
    if manifest.get("compat_id") != selector or manifest.get("build_target") != target:
        fail("manifest identity differs from the candidate")
    artifact_contract = manifest.get("artifacts", {}).get(target)
    if not isinstance(artifact_contract, dict):
        fail("manifest has no target artifact")
    artifact = artifact_value.resolve(strict=True)
    actual_artifact = {
        "filename": artifact.name,
        "sha256": sha256_file(artifact),
        "size": artifact.stat().st_size,
    }
    expected_filename = require_string(artifact_contract.get("filename"), "artifact filename")
    if actual_artifact["filename"] != expected_filename:
        fail("accepted artifact filename differs from the manifest")
    if candidate.get("artifact") != actual_artifact:
        fail("accepted artifact differs from the candidate record")

    evidence = load_json(evidence_path.resolve(strict=True))
    if not evidence:
        fail("acceptance evidence must not be empty")
    profile_rel, profile_path = safe_repo_file(
        repository, route.get("build_profile"), "candidate build profile", "release"
    )
    runtime_rel, runtime_path = safe_repo_file(
        repository, route.get("runtime_lock"), "candidate runtime lock", "release"
    )
    profile_sha = sha256_file(profile_path)
    runtime_sha = sha256_file(runtime_path)
    if profile_sha != route.get("build_profile_sha256") or runtime_sha != route.get("runtime_lock_sha256"):
        fail("candidate build/runtime authority drifted before acceptance")

    acceptance_relative, acceptance_path = new_repo_file(
        repository, acceptance_value, "acceptance record", "release"
    )
    acceptance = {
        "schema": 1,
        "status": "accepted",
        "compat_id": selector,
        "target": target,
        "artifact_filename": actual_artifact["filename"],
        "artifact_sha256": actual_artifact["sha256"],
        "artifact_size": actual_artifact["size"],
        "manifest_sha256": sha256_file(manifest_path),
        "build_profile_sha256": profile_sha,
        "runtime_lock_sha256": runtime_sha,
        "recorded_at": dt.datetime.now(dt.timezone.utc).date().isoformat(),
        "accepted_by": accepted_by,
        "evidence": evidence,
    }
    write_new_or_equal(acceptance_path, acceptance)
    entry["manifest"] = manifest_rel
    entry["manifest_sha256"] = acceptance["manifest_sha256"]
    entry["lifecycle"] = "accepted"
    entry["release_enabled"] = True
    route["build_profile"] = profile_rel
    route["runtime_lock"] = runtime_rel
    route["acceptance"] = acceptance_relative
    route["acceptance_sha256"] = sha256_file(acceptance_path)
    if make_current:
        current = index.get("current")
        if not isinstance(current, dict):
            fail("compatibility index current route is invalid")
        current[target] = selector
    write_json_atomic(index_path, index)
    return {
        "schema": 1,
        "result": "accepted",
        "compat_id": selector,
        "target": target,
        "acceptance": acceptance_relative,
        "current": index.get("current"),
    }


def workflow_static_guard(paths: list[Path]) -> dict[str, Any]:
    patterns = (
        re.compile(r"rust-v\d+\.\d+\.\d+-native-join-p\d+"),
        re.compile(r"sha512-[A-Za-z0-9+/=]{40,}"),
        re.compile(
            r"(?im)^\s*(?:upstream_commit|CSA_UPSTREAM_COMMIT|UPSTREAM_COMMIT)\s*[:=]\s*['\"]?[0-9a-f]{40}"
        ),
    )
    checked: list[str] = []
    for path in paths:
        text = path.read_text(encoding="utf-8")
        for pattern in patterns:
            match = pattern.search(text)
            if match:
                fail(f"workflow repeats compatibility authority in {path}: {match.group(0)!r}")
        checked.append(str(path))
    return {"schema": 1, "status": "pass", "files": checked}


def command_resolve(args: argparse.Namespace) -> None:
    result = resolve(
        Path(args.repository),
        args.selector,
        args.target,
        require_acceptance=args.require_acceptance,
        require_release=args.require_release,
    )
    text = json.dumps(result, indent=2, sort_keys=True) + "\n"
    if args.output:
        Path(args.output).write_text(text, encoding="utf-8", newline="\n")
    else:
        sys.stdout.write(text)
    if args.github_output:
        write_github_output(Path(args.github_output), result)
    if args.bash_env:
        write_bash_env(Path(args.bash_env), result)


def command_validate(args: argparse.Namespace) -> None:
    results = validate_all(Path(args.repository).resolve())
    output: dict[str, Any] = {
        "schema": 1,
        "status": "pass",
        "resolved_targets": len(results),
    }
    if args.workflow:
        output["workflow_guard"] = workflow_static_guard(
            [Path(value).resolve(strict=True) for value in args.workflow]
        )
    print(json.dumps(output, sort_keys=True))


def command_list(args: argparse.Namespace) -> None:
    repository = Path(args.repository).resolve()
    index = load_json(repository / "release" / "compatibility-index.json")
    entries = index.get("compatibilities")
    if not isinstance(entries, dict):
        fail("compatibility index has no compatibilities object")
    selected: list[str] = []
    for compat_id in sorted(entries):
        entry = entries[compat_id]
        if not isinstance(entry, dict):
            continue
        enabled_key = "release_enabled" if args.release else "build_enabled"
        targets = entry.get("targets")
        if entry.get(enabled_key) is True and isinstance(targets, dict) and args.target in targets:
            selected.append(compat_id)
    if args.format == "json":
        print(json.dumps(selected))
    else:
        print("\n".join(selected))


def command_install_catalog(args: argparse.Namespace) -> None:
    catalog = build_install_catalog(
        Path(args.repository),
        args.formal_tags,
        args.current_release_tag,
        args.current_source_commit,
        args.target,
    )
    write_json_atomic(args.output, catalog)
    print(
        json.dumps(
            {
                "schema": 1,
                "status": "written",
                "output": str(args.output),
                "entries": len(catalog["entries"]),
            },
            sort_keys=True,
        )
    )


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)

    resolve_parser = subparsers.add_parser("resolve", help="resolve one exact compatibility build")
    resolve_parser.add_argument("--repository", default=".")
    resolve_parser.add_argument("--selector", default="current")
    resolve_parser.add_argument("--target", default="x86_64-pc-windows-msvc")
    resolve_parser.add_argument("--require-acceptance", action="store_true")
    resolve_parser.add_argument("--require-release", action="store_true")
    resolve_parser.add_argument("--output")
    resolve_parser.add_argument("--github-output")
    resolve_parser.add_argument("--bash-env")
    resolve_parser.set_defaults(handler=command_resolve)

    validate_parser = subparsers.add_parser("validate", help="validate the complete catalog")
    validate_parser.add_argument("--repository", default=".")
    validate_parser.add_argument("--workflow", action="append", default=[])
    validate_parser.set_defaults(handler=command_validate)

    list_parser = subparsers.add_parser("list", help="list enabled compatibility IDs")
    list_parser.add_argument("--repository", default=".")
    list_parser.add_argument("--target", default="x86_64-pc-windows-msvc")
    list_parser.add_argument("--release", action="store_true")
    list_parser.add_argument("--format", choices=("lines", "json"), default="lines")
    list_parser.set_defaults(handler=command_list)

    install_catalog_parser = subparsers.add_parser(
        "install-catalog", help="build the untrusted display catalog for interactive installs"
    )
    install_catalog_parser.add_argument("--repository", default=".")
    install_catalog_parser.add_argument("--formal-tags", type=Path, required=True)
    install_catalog_parser.add_argument("--current-release-tag", required=True)
    install_catalog_parser.add_argument("--current-source-commit", required=True)
    install_catalog_parser.add_argument("--target", default="x86_64-pc-windows-msvc")
    install_catalog_parser.add_argument("--output", type=Path, required=True)
    install_catalog_parser.set_defaults(handler=command_install_catalog)

    stage_parser = subparsers.add_parser(
        "stage-candidate", help="register one newly ported compatibility as a non-releasable candidate"
    )
    stage_parser.add_argument("--repository", default=".")
    stage_parser.add_argument("--manifest", type=Path, required=True)
    stage_parser.add_argument("--target", default="x86_64-pc-windows-msvc")
    stage_parser.add_argument("--runtime-lock", type=Path, required=True)
    stage_parser.add_argument("--npm-integrity", default="registry")

    candidate_parser = subparsers.add_parser(
        "candidate", help="record a GitHub Actions or local candidate artifact identity"
    )
    candidate_parser.add_argument("--repository", default=".")
    candidate_parser.add_argument("--resolution", type=Path, required=True)
    candidate_parser.add_argument("--artifact", type=Path, required=True)
    candidate_parser.add_argument("--output", type=Path, required=True)
    candidate_parser.add_argument("--provider", choices=("github-actions", "local"), required=True)
    candidate_parser.add_argument("--pipeline")
    candidate_parser.add_argument("--job")
    candidate_parser.add_argument("--source-commit")

    accept_parser = subparsers.add_parser(
        "accept", help="bind a locally accepted candidate artifact and enable formal release"
    )
    accept_parser.add_argument("--repository", default=".")
    accept_parser.add_argument("--selector", required=True)
    accept_parser.add_argument("--target", default="x86_64-pc-windows-msvc")
    accept_parser.add_argument("--candidate-record", type=Path, required=True)
    accept_parser.add_argument("--artifact", type=Path, required=True)
    accept_parser.add_argument("--acceptance", type=Path, required=True)
    accept_parser.add_argument("--evidence", type=Path, required=True)
    accept_parser.add_argument("--make-current", action="store_true")
    accept_parser.add_argument("--accepted-by")

    guard_parser = subparsers.add_parser(
        "guard-workflows", help="fail if CI YAML reintroduces compatibility/version authority"
    )
    guard_parser.add_argument("paths", nargs="+", type=Path)
    return parser


def main() -> int:
    parser = build_parser()
    args = parser.parse_args()
    try:
        if args.command in {"resolve", "validate", "list", "install-catalog"}:
            args.handler(args)
        else:
            repository = Path(args.repository).resolve(strict=True) if hasattr(args, "repository") else None
            if args.command == "stage-candidate":
                assert repository is not None
                result = stage_candidate(
                    repository,
                    args.manifest,
                    args.target,
                    args.runtime_lock,
                    args.npm_integrity,
                )
            elif args.command == "candidate":
                assert repository is not None
                result = create_candidate_record(
                    repository,
                    args.resolution,
                    args.artifact,
                    args.output,
                    args.provider,
                    args.pipeline,
                    args.job,
                    args.source_commit,
                )
            elif args.command == "accept":
                assert repository is not None
                result = accept_candidate(
                    repository,
                    args.selector,
                    args.target,
                    args.candidate_record,
                    args.artifact,
                    args.acceptance,
                    args.evidence,
                    args.make_current,
                    args.accepted_by,
                )
            elif args.command == "guard-workflows":
                result = workflow_static_guard([path.resolve(strict=True) for path in args.paths])
            else:
                raise AssertionError(args.command)
            print(json.dumps(result, indent=2, sort_keys=True))
    except (CatalogError, OSError, ValueError, KeyError, TypeError, json.JSONDecodeError) as error:
        print(f"ERROR: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
