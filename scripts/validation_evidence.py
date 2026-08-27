#!/usr/bin/env python3
"""Create and verify exact patched-Codex validation evidence."""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import re
import subprocess
import sys
from pathlib import Path
from typing import Any, NoReturn

from compat_catalog import CatalogError, resolve
from run_patch_contract import ContractError, load_contract, load_test_report
from verify_patch_payload import VerificationError, _load_payload, _payload_file


WORKFLOW_PATH = ".github/workflows/validate-patched-codex.yml"
SHA1 = re.compile(r"[0-9a-f]{40}\Z")
RESOLUTION_BINDINGS = (
    "catalog_path",
    "catalog_sha256",
    "compat_id",
    "lifecycle",
    "build_enabled",
    "release_enabled",
    "release_tag",
    "manifest_path",
    "manifest_sha256",
    "manifest_source_schema",
    "manifest_family_id",
    "codex_version",
    "upstream_tag",
    "upstream_commit",
    "rust_toolchain",
    "rustc_commit",
    "build_target",
    "artifact_filename",
    "build_profile_path",
    "build_profile_sha256",
    "runtime_lock_path",
    "runtime_lock_sha256",
    "cargo_xwin_version",
    "sccache_version",
    "xwin_version",
    "llvm_version",
)


class EvidenceError(RuntimeError):
    pass


def fail(message: str) -> NoReturn:
    raise EvidenceError(message)


def read_json(path: Path, label: str) -> Any:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise EvidenceError(f"cannot read {label}: {error}") from error


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def git_head(repository: Path) -> str:
    result = subprocess.run(
        ["git", "rev-parse", "HEAD"],
        cwd=repository,
        check=True,
        capture_output=True,
        text=True,
    )
    return result.stdout.strip()


def load_resolution(repository: Path, path: Path) -> dict[str, Any]:
    value = read_json(path.resolve(strict=True), "compatibility resolution")
    if not isinstance(value, dict) or value.get("schema") != 1:
        fail("compatibility resolution has an unsupported schema")
    compat_id = value.get("compat_id")
    target = value.get("build_target")
    if not isinstance(compat_id, str) or not isinstance(target, str):
        fail("compatibility resolution is missing its exact identity")
    current = resolve(repository, compat_id, target)
    for key in RESOLUTION_BINDINGS:
        if value.get(key) != current.get(key):
            fail(f"compatibility resolution drifted at {key}")
    return current


def payload_identity(repository: Path, resolution: dict[str, Any]) -> dict[str, Any]:
    manifest_path = (repository / resolution["manifest_path"]).resolve(strict=True)
    payload = _load_payload(manifest_path)
    manifest = payload.manifest
    logical_paths = [
        str(manifest["source_hashes"]),
        "test-contract.json",
        *(str(patch["path"]) for patch in manifest["patches"]),
    ]
    if len(logical_paths) != len(set(logical_paths)):
        fail("payload evidence paths must be unique")

    files: list[dict[str, str]] = []
    tree = hashlib.sha256()
    for logical_path in logical_paths:
        physical_path = _payload_file(payload, logical_path)
        digest = sha256_file(physical_path)
        files.append({"path": logical_path, "sha256": digest})
        tree.update(logical_path.encode("utf-8"))
        tree.update(b"\0")
        tree.update(digest.encode("ascii"))
        tree.update(b"\n")

    if files[0]["sha256"] != manifest["source_hashes_sha256"]:
        fail("source-hashes digest differs from the manifest")
    patch_digests = {
        str(patch["path"]): str(patch["sha256"]) for patch in manifest["patches"]
    }
    for entry in files[2:]:
        if patch_digests.get(entry["path"]) != entry["sha256"]:
            fail(f"patch digest differs from the manifest: {entry['path']}")
    load_contract(_payload_file(payload, "test-contract.json"), resolution["compat_id"])
    return {
        "source_hashes_sha256": files[0]["sha256"],
        "test_contract_sha256": files[1]["sha256"],
        "tree_sha256": tree.hexdigest(),
        "files": files,
    }


def validation_identity(
    repository: Path,
    resolution: dict[str, Any],
    test_report_path: Path,
) -> dict[str, Any]:
    test_report_path = test_report_path.resolve(strict=True)
    payload = _load_payload((repository / resolution["manifest_path"]).resolve(strict=True))
    contract = load_contract(_payload_file(payload, "test-contract.json"), resolution["compat_id"])
    expected_steps = [
        (kind, step["name"])
        for kind, contract_steps in (
            ("generation", contract["generation"]),
            ("test", contract["tests"]),
        )
        for step in contract_steps
    ]
    report = load_test_report(
        test_report_path,
        resolution["compat_id"],
        contract["known_upstream_errata"],
        expected_steps,
    )
    source = report["source_verification"]
    if (
        source.get("compat_id") != resolution["compat_id"]
        or source.get("commit") != resolution["upstream_commit"]
        or source.get("applied") is not True
    ):
        fail("test report source verification differs from the resolved payload")
    clippy_names = {
        step["name"]
        for step in contract["tests"]
        if step.get("argv", [])[:2] == ["cargo", "clippy"]
    }
    if not clippy_names or not clippy_names <= {step["name"] for step in report["steps"]}:
        fail("test report does not prove the selected contract's Clippy step")
    return {
        "test_report_sha256": sha256_file(test_report_path),
        "contract": "passed",
        "tests": "passed",
        "clippy": "passed",
        "steps": [{"kind": kind, "name": name} for kind, name in expected_steps],
    }


def require_commit(repository: Path, value: str) -> str:
    if not SHA1.fullmatch(value):
        fail("CSA commit must be a lowercase 40-hex Git commit")
    if git_head(repository) != value:
        fail("CSA commit differs from repository HEAD")
    return value


def require_positive(value: int, label: str) -> int:
    if value < 1:
        fail(f"{label} must be a positive integer")
    return value


def evidence_document(
    repository: Path,
    resolution_path: Path,
    test_report_path: Path,
    csa_commit: str,
    workflow_run_id: int,
    workflow_run_attempt: int,
    created_at: str,
) -> dict[str, Any]:
    resolution = load_resolution(repository, resolution_path)
    workflow = (repository / WORKFLOW_PATH).resolve(strict=True)
    return {
        "schema": 1,
        "result": "pass",
        "created_at": created_at,
        "csa_commit": require_commit(repository, csa_commit),
        "compatibility": {
            "compat_id": resolution["compat_id"],
            "lifecycle": resolution["lifecycle"],
            "release_enabled": resolution["release_enabled"],
            "release_tag": resolution["release_tag"],
            "codex_version": resolution["codex_version"],
            "upstream_tag": resolution["upstream_tag"],
            "upstream_commit": resolution["upstream_commit"],
            "build_target": resolution["build_target"],
            "artifact_filename": resolution["artifact_filename"],
        },
        "authorities": {
            "catalog": {
                "path": resolution["catalog_path"],
                "sha256": resolution["catalog_sha256"],
            },
            "manifest": {
                "path": resolution["manifest_path"],
                "sha256": resolution["manifest_sha256"],
                "source_schema": resolution["manifest_source_schema"],
                "family_id": resolution["manifest_family_id"],
            },
            "build_profile": {
                "path": resolution["build_profile_path"],
                "sha256": resolution["build_profile_sha256"],
            },
            "runtime_lock": {
                "path": resolution["runtime_lock_path"],
                "sha256": resolution["runtime_lock_sha256"],
            },
        },
        "payload": payload_identity(repository, resolution),
        "toolchain": {
            "rust_toolchain": resolution["rust_toolchain"],
            "rustc_commit": resolution["rustc_commit"],
            "cargo_xwin": resolution["cargo_xwin_version"],
            "sccache": resolution["sccache_version"],
            "xwin": resolution["xwin_version"],
            "llvm": resolution["llvm_version"],
        },
        "validation": validation_identity(repository, resolution, test_report_path),
        "workflow": {
            "path": WORKFLOW_PATH,
            "sha256": sha256_file(workflow),
            "run_id": require_positive(workflow_run_id, "workflow run ID"),
            "run_attempt": require_positive(workflow_run_attempt, "workflow run attempt"),
        },
    }


def create_evidence(
    repository: Path,
    resolution_path: Path,
    test_report_path: Path,
    output: Path,
    csa_commit: str,
    workflow_run_id: int,
    workflow_run_attempt: int,
) -> dict[str, Any]:
    repository = repository.resolve(strict=True)
    output = output.resolve()
    if output.exists() or not output.parent.is_dir():
        fail("evidence output must be a new file under an existing directory")
    created_at = dt.datetime.now(dt.timezone.utc).isoformat().replace("+00:00", "Z")
    evidence = evidence_document(
        repository,
        resolution_path,
        test_report_path,
        csa_commit,
        workflow_run_id,
        workflow_run_attempt,
        created_at,
    )
    with output.open("xb") as stream:
        stream.write((json.dumps(evidence, indent=2, sort_keys=True) + "\n").encode())
    return evidence


def verify_evidence(
    repository: Path,
    resolution_path: Path,
    test_report_path: Path,
    evidence_path: Path,
    csa_commit: str,
    workflow_run_id: int,
    workflow_run_attempt: int,
) -> dict[str, Any]:
    repository = repository.resolve(strict=True)
    actual = read_json(evidence_path.resolve(strict=True), "validation evidence")
    if not isinstance(actual, dict) or set(actual) != {
        "schema",
        "result",
        "created_at",
        "csa_commit",
        "compatibility",
        "authorities",
        "payload",
        "toolchain",
        "validation",
        "workflow",
    }:
        fail("validation evidence has an unsupported shape")
    created_at = actual.get("created_at")
    if not isinstance(created_at, str) or not created_at.endswith("Z"):
        fail("validation evidence timestamp is invalid")
    try:
        timestamp = dt.datetime.fromisoformat(created_at.replace("Z", "+00:00"))
    except ValueError as error:
        raise EvidenceError("validation evidence timestamp is invalid") from error
    if timestamp.tzinfo is None or timestamp.utcoffset() != dt.timedelta(0):
        fail("validation evidence timestamp must use UTC")
    expected = evidence_document(
        repository,
        resolution_path,
        test_report_path,
        csa_commit,
        workflow_run_id,
        workflow_run_attempt,
        created_at,
    )
    if actual != expected:
        fail("validation evidence differs from the current exact inputs")
    return {
        "schema": 1,
        "status": "pass",
        "compat_id": expected["compatibility"]["compat_id"],
        "csa_commit": csa_commit,
        "workflow_run_id": workflow_run_id,
    }


def select_run(
    runs_path: Path,
    csa_commit: str,
    default_branch: str,
    requested_run_id: int | None = None,
) -> dict[str, int]:
    response = read_json(runs_path.resolve(strict=True), "validation workflow runs")
    runs = response.get("workflow_runs") if isinstance(response, dict) else None
    if not isinstance(runs, list):
        fail("validation workflow response has no workflow_runs list")
    candidates: list[dict[str, Any]] = []
    for run in runs:
        if not isinstance(run, dict):
            continue
        if (
            run.get("path") == WORKFLOW_PATH
            and run.get("head_sha") == csa_commit
            and run.get("head_branch") == default_branch
            and run.get("event") in {"push", "workflow_dispatch"}
            and run.get("status") == "completed"
            and run.get("conclusion") == "success"
            and isinstance(run.get("id"), int)
            and isinstance(run.get("run_attempt"), int)
            and run["id"] > 0
            and run["run_attempt"] > 0
        ):
            candidates.append(run)
    if requested_run_id is not None:
        candidates = [run for run in candidates if run["id"] == requested_run_id]
    if not candidates:
        fail("no successful default-branch validation run matches the exact CSA commit")
    selected = candidates[0]
    return {"run_id": selected["id"], "run_attempt": selected["run_attempt"]}


def write_github_output(path: Path, selected: dict[str, int]) -> None:
    with path.open("a", encoding="utf-8", newline="\n") as stream:
        stream.write(f"validation_run_id={selected['run_id']}\n")
        stream.write(f"validation_run_attempt={selected['run_attempt']}\n")


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    for name in ("create", "verify"):
        command = subparsers.add_parser(name)
        command.add_argument("--repository", default=".")
        command.add_argument("--resolution", type=Path, required=True)
        command.add_argument("--test-report", type=Path, required=True)
        command.add_argument("--csa-commit", required=True)
        command.add_argument("--workflow-run-id", type=int, required=True)
        command.add_argument("--workflow-run-attempt", type=int, required=True)
        if name == "create":
            command.add_argument("--output", type=Path, required=True)
        else:
            command.add_argument("--evidence", type=Path, required=True)
    select = subparsers.add_parser("select-run")
    select.add_argument("--runs", type=Path, required=True)
    select.add_argument("--csa-commit", required=True)
    select.add_argument("--default-branch", required=True)
    select.add_argument("--requested-run-id", type=int)
    select.add_argument("--github-output", type=Path)
    return parser


def main() -> int:
    args = build_parser().parse_args()
    try:
        if args.command == "create":
            result = create_evidence(
                Path(args.repository),
                args.resolution,
                args.test_report,
                args.output,
                args.csa_commit,
                args.workflow_run_id,
                args.workflow_run_attempt,
            )
        elif args.command == "verify":
            result = verify_evidence(
                Path(args.repository),
                args.resolution,
                args.test_report,
                args.evidence,
                args.csa_commit,
                args.workflow_run_id,
                args.workflow_run_attempt,
            )
        else:
            result = select_run(
                args.runs,
                args.csa_commit,
                args.default_branch,
                args.requested_run_id,
            )
            if args.github_output:
                write_github_output(args.github_output, result)
    except (
        CatalogError,
        ContractError,
        EvidenceError,
        OSError,
        subprocess.CalledProcessError,
        VerificationError,
    ) as error:
        print(json.dumps({"schema": 1, "error": str(error)}, indent=2), file=sys.stderr)
        return 2
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
