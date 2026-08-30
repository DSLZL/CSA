#!/usr/bin/env python3
"""Analyze and verify exact CSA patch-family reuse."""

from __future__ import annotations

import argparse
import json
import re
import sys
import tomllib
from pathlib import Path, PurePosixPath
from typing import Any

from verify_patch_payload import (
    LoadedPayload,
    VerificationError,
    _digest,
    _load_payload,
    _payload_file,
    _relative,
    _run,
    _staged_patch_index,
    verify,
)


REVIEWED_CLASSIFICATIONS = {"COMMON_CORE_WITH_ADAPTER", "VERSION_SPECIFIC"}
DIFF_HEADER = re.compile(r"diff --git a/(.+) b/(.+)\Z")
FAMILY_ID = re.compile(r"[a-z0-9][a-z0-9._-]+\Z")


class PatchFamilyError(RuntimeError):
    pass


def json_bytes(value: object) -> bytes:
    return (json.dumps(value, indent=2, sort_keys=True) + "\n").encode("utf-8")


def line_count(data: bytes | None) -> int:
    if not data:
        return 0
    return data.count(b"\n") + int(not data.endswith(b"\n"))


def blob_record(data: bytes | None) -> dict[str, object] | None:
    if data is None:
        return None
    return {"sha256": _digest(data), "size": len(data), "lines": line_count(data)}


def patch_paths(path: Path) -> list[str]:
    touched: list[str] = []
    for line in path.read_text(encoding="utf-8").splitlines():
        match = DIFF_HEADER.fullmatch(line)
        if not match:
            continue
        left, right = match.groups()
        if left != right:
            raise PatchFamilyError(f"renames are unsupported: {path}: {line}")
        touched.append(_relative(left, "patch source path"))
    if not touched or len(touched) != len(set(touched)):
        raise PatchFamilyError(f"patch must touch unique paths: {path}")
    return touched


def payload_patches(payload: LoadedPayload) -> tuple[list[Path], dict[str, list[str]]]:
    physical: list[Path] = []
    owners: dict[str, list[str]] = {}
    for patch in payload.manifest["patches"]:
        logical = str(patch["path"])
        path = _payload_file(payload, logical)
        physical.append(path)
        for touched in patch_paths(path):
            owners.setdefault(touched, []).append(logical)
    return physical, owners


def git_blob(source: Path, spec: str, env: dict[str, str] | None = None) -> bytes | None:
    result = _run(["git", "show", spec], source, env=env, check=False)
    return result.stdout if result.returncode == 0 else None


def snapshot(manifest_path: Path, source: Path) -> dict[str, object]:
    manifest_path = manifest_path.resolve(strict=True)
    source = source.resolve(strict=True)
    verify(manifest_path, source, False, None)
    payload = _load_payload(manifest_path)
    manifest = payload.manifest
    patches, owners = payload_patches(payload)
    commit = str(manifest["upstream_commit"])
    records: dict[str, object] = {}
    with _staged_patch_index(source, commit, patches) as env:
        for relative in sorted(owners):
            upstream = git_blob(source, f"{commit}:{relative}")
            postimage = git_blob(source, f":{relative}", env)
            canonical = _run(
                [
                    "git",
                    "diff",
                    "--cached",
                    "--binary",
                    "--full-index",
                    "--no-ext-diff",
                    "--",
                    relative,
                ],
                source,
                env=env,
            ).stdout
            records[relative] = {
                "upstream": blob_record(upstream),
                "postimage": blob_record(postimage),
                "canonical_diff_sha256": _digest(canonical),
                "logical_patches": sorted(owners[relative]),
            }
    patch_records = []
    for patch, path in zip(manifest["patches"], patches, strict=True):
        patch_records.append(
            {
                "logical_path": patch["path"],
                "physical_path": path.relative_to(payload.payload_root).as_posix(),
                "sha256": patch["sha256"],
                "touched_paths": patch_paths(path),
            }
        )
    return {
        "compat_id": manifest["compat_id"],
        "family_id": payload.family_id,
        "schema": payload.source_schema,
        "codex_version": manifest["codex_version"],
        "upstream_tag": manifest["upstream_tag"],
        "upstream_commit": commit,
        "manifest_sha256": _digest(manifest_path.read_bytes()),
        "patches": patch_records,
        "paths": records,
    }


def load_decisions(path: Path) -> dict[str, str]:
    try:
        value = json.loads(path.read_bytes())
    except (OSError, json.JSONDecodeError) as error:
        raise PatchFamilyError(f"cannot read decisions: {error}") from error
    if not isinstance(value, dict) or set(value) != {"schema", "paths"} or value["schema"] != 1:
        raise PatchFamilyError("decisions must contain schema=1 and paths")
    paths = value["paths"]
    if not isinstance(paths, dict):
        raise PatchFamilyError("decisions paths must be an object")
    result: dict[str, str] = {}
    for relative, classification in paths.items():
        _relative(relative, "decision path")
        if not isinstance(classification, str) or classification not in REVIEWED_CLASSIFICATIONS:
            raise PatchFamilyError(f"invalid reviewed classification for {relative}")
        result[relative] = classification
    return result


def automatic_classification(left: dict[str, Any], right: dict[str, Any]) -> str | None:
    left_upstream = left.get("upstream")
    right_upstream = right.get("upstream")
    left_post = left.get("postimage")
    right_post = right.get("postimage")
    if (
        left_upstream is None
        and right_upstream is None
        and left_post is not None
        and right_post is not None
        and left_post["sha256"] == right_post["sha256"]
    ):
        return "COMMON_ADDITION"
    if (
        left_upstream is not None
        and right_upstream is not None
        and left["canonical_diff_sha256"] == right["canonical_diff_sha256"]
    ):
        return "COMMON_EXACT_PATCH"
    return None


def patch_rows(paths: list[dict[str, Any]], left: dict[str, Any], right: dict[str, Any]) -> list[dict[str, object]]:
    by_patch: dict[str, set[str]] = {}
    for side in (left, right):
        for patch in side["patches"]:
            by_patch.setdefault(patch["logical_path"], set()).update(patch["touched_paths"])
    path_classes = {row["path"]: row["classification"] for row in paths}
    result = []
    for logical, touched in sorted(by_patch.items()):
        classes = sorted({path_classes[path] for path in touched})
        classification = classes[0] if len(classes) == 1 else "COMMON_CORE_WITH_ADAPTER"
        recommendation = (
            "split by path ownership"
            if len(classes) > 1
            else {
                "COMMON_ADDITION": "move to shared/additions",
                "COMMON_EXACT_PATCH": "defer shared/patches until a second p10 binding proves reuse",
                "COMMON_CORE_WITH_ADAPTER": "split CSA core from the upstream adapter",
                "VERSION_SPECIFIC": "keep binding-local",
            }[classification]
        )
        result.append(
            {
                "logical_path": logical,
                "touched_paths": sorted(touched),
                "classification": classification,
                "recommendation": recommendation,
            }
        )
    return result


def analyze(
    family_id: str,
    left_manifest: Path,
    left_source: Path,
    right_manifest: Path,
    right_source: Path,
    decisions_path: Path,
) -> dict[str, object]:
    if not FAMILY_ID.fullmatch(family_id):
        raise PatchFamilyError("invalid family_id")
    left = snapshot(left_manifest, left_source)
    right = snapshot(right_manifest, right_source)
    decisions = load_decisions(decisions_path)
    rows: list[dict[str, Any]] = []
    ambiguous: set[str] = set()
    all_paths = sorted(set(left["paths"]) | set(right["paths"]))
    missing: list[str] = []
    for relative in all_paths:
        left_record = left["paths"].get(relative, {})
        right_record = right["paths"].get(relative, {})
        classification = automatic_classification(left_record, right_record)
        method = "exact"
        if classification is None:
            ambiguous.add(relative)
            classification = decisions.get(relative)
            if classification is None:
                missing.append(relative)
                continue
            location = "bindings/<compat-id>/patches/"
            note = (
                "same CSA behavior; exact upstream adapter required"
                if classification == "COMMON_CORE_WITH_ADAPTER"
                else "path is patched by only one compared binding"
            )
            method = "reviewed"
        else:
            location = (
                "shared/additions/"
                if classification == "COMMON_ADDITION"
                else "shared/patches/"
            )
            note = "byte-identical exact evidence"
        rows.append(
            {
                "path": relative,
                "left": left_record or None,
                "right": right_record or None,
                "classification": classification,
                "classification_method": method,
                "proposed_location": location,
                "note": note,
            }
        )
    if missing:
        raise PatchFamilyError("missing reviewed classifications:\n" + "\n".join(missing))
    unknown = set(decisions) - ambiguous
    if unknown:
        raise PatchFamilyError(f"stale decisions for exact or unknown paths: {sorted(unknown)}")
    shared = sum(
        int(row["right"] is not None and row["right"].get("postimage") is not None)
        for row in rows
        if row["classification"] in {"COMMON_ADDITION", "COMMON_EXACT_PATCH"}
    )
    shared_loc = sum(
        int(row["right"]["postimage"]["lines"])
        for row in rows
        if row["classification"] in {"COMMON_ADDITION", "COMMON_EXACT_PATCH"}
        and row["right"] is not None
        and row["right"].get("postimage") is not None
    )
    adapter_loc = sum(
        int(row["right"]["postimage"]["lines"])
        for row in rows
        if row["classification"] not in {"COMMON_ADDITION", "COMMON_EXACT_PATCH"}
        and row["right"] is not None
        and row["right"].get("postimage") is not None
    )
    total_loc = shared_loc + adapter_loc
    return {
        "schema": 1,
        "family_id": family_id,
        "left": {key: value for key, value in left.items() if key != "paths"},
        "right": {key: value for key, value in right.items() if key != "paths"},
        "paths": rows,
        "patches": patch_rows(rows, left, right),
        "metrics": {
            "shared_files": shared,
            "shared_loc": shared_loc,
            "adapter_files": len(rows) - shared,
            "adapter_loc": adapter_loc,
            "shared_code_ratio": round(shared_loc / total_loc, 6) if total_loc else 0,
        },
    }


def markdown(report: dict[str, Any]) -> str:
    lines = [
        "# P9 Common Extraction Report",
        "",
        f"Target family: `{report['family_id']}`",
        "",
        "## Compared Payloads",
        "",
        "| Side | Compatibility | Upstream | Commit | Manifest SHA-256 |",
        "|---|---|---|---|---|",
        f"| Left | `{report['left']['compat_id']}` | `{report['left']['upstream_tag']}` | "
        f"`{report['left']['upstream_commit']}` | `{report['left']['manifest_sha256']}` |",
        f"| Right | `{report['right']['compat_id']}` | `{report['right']['upstream_tag']}` | "
        f"`{report['right']['upstream_commit']}` | `{report['right']['manifest_sha256']}` |",
        "",
        "## Patch Classification",
        "",
        "| Logical patch | Classification | Recommendation |",
        "|---|---|---|",
    ]
    for patch in report["patches"]:
        lines.append(
            f"| `{patch['logical_path']}` | `{patch['classification']}` | {patch['recommendation']} |"
        )
    lines.extend(
        [
            "",
            "## Path Evidence",
            "",
            "| Path | Logical owners (left / right) | Left postimage | Right postimage | Equal | Classification | Proposed location | Review |",
            "|---|---|---|---|---|---|---|---|",
        ]
    )
    for row in report["paths"]:
        left_record = row["left"]
        right_record = row["right"]
        left_postimage = left_record["postimage"] if left_record else None
        right_postimage = right_record["postimage"] if right_record else None
        left = left_postimage["sha256"] if left_postimage else "absent"
        right = (
            right_postimage["sha256"] if right_postimage else "absent"
        )
        left_owners = ", ".join(left_record["logical_patches"]) if left_record else "absent"
        right_owners = ", ".join(right_record["logical_patches"]) if right_record else "absent"
        equal = "yes" if left_postimage == right_postimage else "no"
        lines.append(
            f"| `{row['path']}` | `{left_owners}` / `{right_owners}` | `{left}` | `{right}` | "
            f"{equal} | `{row['classification']}` | `{row['proposed_location']}` | {row['note']} |"
        )
    metrics = report["metrics"]
    lines.extend(
        [
            "",
            "## Metrics",
            "",
            "These figures count complete patched postimages used by the comparison, not stored patch lines.",
            "",
            f"- Shared files: {metrics['shared_files']}",
            f"- Shared LOC: {metrics['shared_loc']}",
            f"- Adapter files: {metrics['adapter_files']}",
            f"- Adapter LOC: {metrics['adapter_loc']}",
            f"- Shared-code ratio: {metrics['shared_code_ratio']:.2%}",
            "",
        ]
    )
    return "\n".join(lines)


def added_lines(path: Path) -> int:
    return sum(
        1
        for line in path.read_text(encoding="utf-8").splitlines()
        if line.startswith("+") and not line.startswith("+++")
    )


def verify_family(family_root: Path) -> dict[str, object]:
    family_root = family_root.resolve(strict=True)
    family_path = family_root / "family.toml"
    try:
        family = tomllib.loads(family_path.read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise PatchFamilyError(f"cannot read family: {error}") from error
    if family.get("schema") != 2 or family.get("family_id") != family_root.name:
        raise PatchFamilyError("family identity mismatch")
    bindings = family.get("bindings")
    if not isinstance(bindings, list) or not bindings:
        raise PatchFamilyError("family must contain at least one binding")

    shared_additions: dict[str, int] = {}
    shared_patches: dict[str, int] = {}
    shared_loc = 0
    adapter_loc = 0
    adapter_files: set[str] = set()
    adapter_digests: dict[str, list[str]] = {}
    for row in bindings:
        if not isinstance(row, dict) or not isinstance(row.get("manifest"), str):
            raise PatchFamilyError("invalid family binding row")
        manifest_path = family_root / PurePosixPath(row["manifest"])
        payload = _load_payload(manifest_path)
        compat_id = str(payload.manifest["compat_id"])
        absent = set(payload.manifest["preimage_absent"])
        addition_paths: set[str] = set()
        adapter_count = 0
        for patch in payload.manifest["patches"]:
            logical = str(patch["path"])
            physical = _payload_file(payload, logical)
            relative = physical.relative_to(family_root).as_posix()
            touched = set(patch_paths(physical))
            if relative.startswith("shared/additions/"):
                if not touched <= absent:
                    raise PatchFamilyError(f"shared addition modifies an upstream path: {logical}")
                addition_paths.update(touched)
                shared_additions[relative] = shared_additions.get(relative, 0) + 1
            elif relative.startswith("shared/patches/"):
                if touched & absent:
                    raise PatchFamilyError(f"shared exact patch owns an absent path: {logical}")
                shared_patches[relative] = shared_patches.get(relative, 0) + 1
            elif relative.startswith(f"bindings/{compat_id}/patches/"):
                if touched & absent:
                    raise PatchFamilyError(f"binding adapter contains a CSA-owned addition: {logical}")
                adapter_files.add(relative)
                adapter_digests.setdefault(_digest(physical.read_bytes()), []).append(relative)
                adapter_loc += added_lines(physical)
                adapter_count += 1
            else:
                raise PatchFamilyError(f"patch has unsupported family ownership: {relative}")
        if addition_paths != absent:
            raise PatchFamilyError(
                f"shared additions do not exactly own absent preimages for {compat_id}"
            )
        if not addition_paths or adapter_count == 0:
            raise PatchFamilyError(f"binding must use shared additions and adapters: {compat_id}")
    binding_count = len(bindings)
    if any(count != binding_count for count in shared_additions.values()):
        raise PatchFamilyError("every shared addition must be reused by every binding")
    if any(count < 2 for count in shared_patches.values()):
        raise PatchFamilyError("shared exact patches require at least two bindings")
    duplicate_adapters = sorted(
        paths for paths in adapter_digests.values() if len(paths) > 1
    )
    if duplicate_adapters:
        raise PatchFamilyError(
            f"byte-identical binding adapters must be shared: {duplicate_adapters}"
        )
    for relative in shared_additions:
        shared_loc += added_lines(family_root / PurePosixPath(relative))
    total = shared_loc + adapter_loc
    return {
        "schema": 1,
        "status": "pass",
        "family_id": family_root.name,
        "bindings": binding_count,
        "metrics": {
            "shared_files": len(shared_additions) + len(shared_patches),
            "shared_loc": shared_loc,
            "adapter_files": len(adapter_files),
            "adapter_loc": adapter_loc,
            "shared_code_ratio": round(shared_loc / total, 6) if total else 0,
        },
    }


def write_output(path: Path, data: bytes) -> None:
    path = path.resolve()
    if not path.parent.is_dir() or path.is_symlink():
        raise PatchFamilyError(f"output parent must exist and output may not be a symlink: {path}")
    path.write_bytes(data)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    analyze_parser = subparsers.add_parser("analyze")
    analyze_parser.add_argument("--family-id", required=True)
    analyze_parser.add_argument("--left-manifest", type=Path, required=True)
    analyze_parser.add_argument("--left-source", type=Path, required=True)
    analyze_parser.add_argument("--right-manifest", type=Path, required=True)
    analyze_parser.add_argument("--right-source", type=Path, required=True)
    analyze_parser.add_argument("--decisions", type=Path, required=True)
    analyze_parser.add_argument("--json-output", type=Path, required=True)
    analyze_parser.add_argument("--markdown-output", type=Path, required=True)
    verify_parser = subparsers.add_parser("verify")
    verify_parser.add_argument("--family", type=Path, required=True)
    return parser


def main() -> int:
    args = build_parser().parse_args()
    try:
        if args.command == "analyze":
            report = analyze(
                args.family_id,
                args.left_manifest,
                args.left_source,
                args.right_manifest,
                args.right_source,
                args.decisions,
            )
            write_output(args.json_output, json_bytes(report))
            write_output(args.markdown_output, markdown(report).encode("utf-8"))
            result = {"schema": 1, "status": "written", "paths": len(report["paths"])}
        else:
            result = verify_family(args.family)
    except (
        PatchFamilyError,
        VerificationError,
        OSError,
        UnicodeError,
        json.JSONDecodeError,
        tomllib.TOMLDecodeError,
    ) as error:
        print(f"patch-family verification failed: {error}", file=sys.stderr)
        return 2
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
