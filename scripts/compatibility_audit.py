#!/usr/bin/env python3
"""Audit immutable compatibility entries and report Codex source drift."""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import tempfile
import tomllib
from pathlib import Path, PurePosixPath
from typing import Any

from verify_patch_payload import _digest, _load_payload, _payload_file, _run


class AuditError(RuntimeError):
    pass


def files(root: Path) -> dict[str, str]:
    result = {}
    for path in sorted(root.rglob("*")):
        if path.is_dir():
            continue
        if path.is_symlink() or not path.is_file():
            raise AuditError(f"unsupported compatibility file: {path}")
        result[path.relative_to(root).as_posix()] = _digest(path.read_bytes())
    return result


def validate_new_entry(root: Path) -> dict[str, Any]:
    manifest_path = root / "manifest.toml"
    payload = _load_payload(manifest_path)
    manifest = payload.manifest
    if manifest["compat_id"] != root.name:
        raise AuditError(f"compat_id must match its new directory: {root.name}")
    expected_files = {"manifest.toml"}
    for patch in manifest["patches"]:
        patch_path = _payload_file(payload, patch["path"])
        if not patch_path.is_file() or _digest(patch_path.read_bytes()) != patch["sha256"]:
            raise AuditError(f"new entry patch mismatch: {patch['path']}")
    source_hashes = _payload_file(payload, manifest["source_hashes"])
    if (
        not source_hashes.is_file()
        or _digest(source_hashes.read_bytes()) != manifest["source_hashes_sha256"]
    ):
        raise AuditError("new entry source-hashes mismatch")
    try:
        contract = json.loads(_payload_file(payload, "test-contract.json").read_bytes())
    except (OSError, json.JSONDecodeError) as error:
        raise AuditError(f"cannot read new entry test contract: {error}") from error
    if not isinstance(contract, dict) or contract.get("compat_id") != root.name:
        raise AuditError("new entry test contract identity mismatch")
    for path in payload.files.values():
        if path.is_relative_to(root):
            expected_files.add(path.relative_to(root).as_posix())
    actual_files = set(files(root))
    if actual_files != expected_files:
        raise AuditError(
            f"new entry file set mismatch; missing={sorted(expected_files - actual_files)}, "
            f"unknown={sorted(actual_files - expected_files)}"
        )
    return {
        "compat_id": root.name,
        "codex_version": manifest["codex_version"],
        "target": manifest["build_target"],
    }


def family_document(root: Path) -> dict[str, Any]:
    try:
        family = tomllib.loads((root / "family.toml").read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise AuditError(f"cannot read patch family: {error}") from error
    if not isinstance(family, dict) or not isinstance(family.get("bindings"), list):
        raise AuditError("invalid patch family")
    return family


def validate_family(root: Path) -> dict[str, Any]:
    family = family_document(root)
    if family.get("family_id") != root.name:
        raise AuditError("family_id must match its top-level directory")
    expected_files = {"family.toml"}
    bindings = []
    for binding in family["bindings"]:
        if not isinstance(binding, dict) or not isinstance(binding.get("manifest"), str):
            raise AuditError("invalid family binding")
        manifest_path = root / PurePosixPath(binding["manifest"])
        payload = _load_payload(manifest_path)
        bindings.append(validate_new_entry(manifest_path.parent))
        expected_files.add(binding["manifest"])
        for source in payload.files.values():
            try:
                expected_files.add(source.relative_to(root).as_posix())
            except ValueError as error:
                raise AuditError("family payload file escapes its root") from error
    actual_files = set(files(root))
    if actual_files != expected_files:
        raise AuditError(
            f"family file set mismatch; missing={sorted(expected_files - actual_files)}, "
            f"unknown={sorted(actual_files - expected_files)}"
        )
    return {"family_id": root.name, "bindings": bindings}


def check_family_immutability(baseline: Path, candidate: Path) -> dict[str, Any]:
    before_document = (baseline / "family.toml").read_bytes()
    after_document = (candidate / "family.toml").read_bytes()
    if not after_document.startswith(before_document):
        raise AuditError(f"patch family index is not append-only: {baseline.name}")
    before = family_document(baseline)
    after = family_document(candidate)
    before_bindings = before.pop("bindings")
    after_bindings = after.pop("bindings")
    if before != after or after_bindings[: len(before_bindings)] != before_bindings:
        raise AuditError(f"patch family metadata or existing rows changed: {baseline.name}")
    before_files = files(baseline)
    after_files = files(candidate)
    for relative, digest in before_files.items():
        if relative != "family.toml" and after_files.get(relative) != digest:
            raise AuditError(f"immutable patch-family file changed: {baseline.name}/{relative}")
    validate_family(candidate)
    return {
        "family_id": baseline.name,
        "added_bindings": after_bindings[len(before_bindings) :],
    }


def check_immutability(baseline: Path, candidate: Path) -> dict[str, Any]:
    if not baseline.is_absolute() or not candidate.is_absolute():
        raise AuditError("baseline and candidate paths must be absolute")
    baseline = baseline.resolve(strict=True)
    candidate = candidate.resolve(strict=True)
    if not baseline.is_dir() or not candidate.is_dir():
        raise AuditError("baseline and candidate must be directories")
    baseline_entries = {path.name: path for path in baseline.iterdir() if path.is_dir()}
    candidate_entries = {path.name: path for path in candidate.iterdir() if path.is_dir()}
    missing = set(baseline_entries) - set(candidate_entries)
    if missing:
        raise AuditError(f"compatibility entries were deleted: {sorted(missing)}")
    changed_families = []
    for compat_id, root in baseline_entries.items():
        candidate_root = candidate_entries[compat_id]
        if (root / "family.toml").is_file():
            changed_families.append(check_family_immutability(root, candidate_root))
        elif files(root) != files(candidate_root):
            raise AuditError(f"immutable compatibility entry changed: {compat_id}")
    added = []
    for compat_id in sorted(set(candidate_entries) - set(baseline_entries)):
        root = candidate_entries[compat_id]
        added.append(validate_family(root) if (root / "family.toml").is_file() else validate_new_entry(root))
    return {
        "schema": 1,
        "result": "pass",
        "unchanged_entries": sorted(baseline_entries),
        "added_entries": added,
        "changed_families": changed_families,
    }


def git_blob(source: Path, commit: str, relative: str) -> bytes | None:
    result = _run(["git", "show", f"{commit}:{relative}"], source, check=False)
    return result.stdout if result.returncode == 0 else None


def patch_preflight(source: Path, commit: str, manifest_path: Path, manifest: dict[str, Any]) -> dict[str, Any]:
    payload = _load_payload(manifest_path)
    with tempfile.TemporaryDirectory(prefix="codex-drift-index-") as temp_dir:
        env = os.environ.copy()
        env["GIT_INDEX_FILE"] = str(Path(temp_dir) / "index")
        _run(["git", "read-tree", commit], source, env=env)
        for patch in manifest["patches"]:
            path = _payload_file(payload, patch["path"])
            check = _run(
                ["git", "apply", "--cached", "--check", "--whitespace=error-all", str(path)],
                source,
                check=False,
                env=env,
            )
            if check.returncode:
                return {
                    "result": "fail",
                    "patch": patch["path"],
                    "error": check.stderr.decode("utf-8", "replace").strip(),
                }
            apply = _run(
                ["git", "apply", "--cached", "--whitespace=error-all", str(path)],
                source,
                check=False,
                env=env,
            )
            if apply.returncode:
                return {
                    "result": "fail",
                    "patch": patch["path"],
                    "error": apply.stderr.decode("utf-8", "replace").strip(),
                }
    return {"result": "pass", "patches": len(manifest["patches"])}


def report_drift(manifest_path: Path, source: Path, tag: str) -> dict[str, Any]:
    if not manifest_path.is_absolute() or not source.is_absolute() or not tag:
        raise AuditError("manifest/source must be absolute and tag must be non-empty")
    manifest_path = manifest_path.resolve(strict=True)
    source = source.resolve(strict=True)
    manifest = _load_payload(manifest_path).manifest
    if _run(["git", "status", "--porcelain=v1", "--untracked-files=all"], source).stdout:
        raise AuditError("candidate source worktree must be clean")
    commit = _run(["git", "rev-parse", "HEAD"], source).stdout.decode().strip()
    tag_commit = _run(["git", "rev-parse", f"refs/tags/{tag}^{{commit}}"], source).stdout.decode().strip()
    if tag_commit != commit:
        raise AuditError("candidate tag does not peel to candidate HEAD")
    try:
        cargo = tomllib.loads((source / "codex-rs" / "Cargo.toml").read_text(encoding="utf-8"))
        version = cargo["workspace"]["package"]["version"]
    except (KeyError, OSError, tomllib.TOMLDecodeError) as error:
        raise AuditError(f"cannot read candidate Codex version: {error}") from error

    path_results = []
    for relative, expected in manifest["preimage"].items():
        blob = git_blob(source, commit, relative)
        actual = _digest(blob) if blob is not None else None
        path_results.append(
            {
                "path": relative,
                "baseline": "present",
                "status": "unchanged" if actual == expected else ("missing" if actual is None else "changed"),
                "expected_sha256": expected,
                "actual_sha256": actual,
            }
        )
    for relative in manifest["preimage_absent"]:
        blob = git_blob(source, commit, relative)
        path_results.append(
            {
                "path": relative,
                "baseline": "absent",
                "status": "unchanged" if blob is None else "unexpected_present",
                "actual_sha256": _digest(blob) if blob is not None else None,
            }
        )
    preflight = patch_preflight(source, commit, manifest_path, manifest)
    exact_identity = (
        version == manifest["codex_version"]
        and tag == manifest["upstream_tag"]
        and commit == manifest["upstream_commit"]
    )
    unchanged = exact_identity and all(item["status"] == "unchanged" for item in path_results)
    result = "unchanged" if unchanged and preflight["result"] == "pass" else "port_required"
    return {
        "schema": 1,
        "result": result,
        "baseline": {
            "compat_id": manifest["compat_id"],
            "version": manifest["codex_version"],
            "tag": manifest["upstream_tag"],
            "commit": manifest["upstream_commit"],
        },
        "candidate": {"version": version, "tag": tag, "commit": commit},
        "exact_identity": exact_identity,
        "paths": path_results,
        "patch_preflight": preflight,
        "implementation_contract_erratum_required": result != "unchanged",
        "next_step": (
            "reuse immutable entry"
            if result == "unchanged"
            else "create a new compatibility directory; do not edit the baseline entry"
        ),
    }


def write_new(path: Path, value: object) -> None:
    if not path.is_absolute() or not path.parent.is_dir():
        raise AuditError("output must be an absolute new file under an existing directory")
    with path.open("xb") as output:
        output.write((json.dumps(value, indent=2, sort_keys=True) + "\n").encode())


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    immutable = commands.add_parser("immutability")
    immutable.add_argument("--baseline", type=Path, required=True)
    immutable.add_argument("--candidate", type=Path, required=True)
    drift = commands.add_parser("drift")
    drift.add_argument("--manifest", type=Path, required=True)
    drift.add_argument("--source", type=Path, required=True)
    drift.add_argument("--tag", required=True)
    drift.add_argument("--output", type=Path)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    try:
        if args.command == "immutability":
            result = check_immutability(args.baseline, args.candidate)
        else:
            result = report_drift(args.manifest, args.source, args.tag)
            if args.output:
                write_new(args.output, result)
    except (AuditError, OSError, subprocess.SubprocessError, RuntimeError) as error:
        print(json.dumps({"schema": 1, "error": str(error)}, indent=2), file=os.sys.stderr)
        return 2
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
