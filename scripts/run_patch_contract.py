#!/usr/bin/env python3
"""Apply and run one immutable patch payload contract in a disposable checkout."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
from pathlib import Path
from typing import Any

from verify_patch_payload import VerificationError, _load_payload, _payload_file, verify


ENV_NAME = re.compile(r"[A-Z][A-Z0-9_]*\Z")
FLAKY_TUI_BACKGROUND_EXIT_STEP = "TUI background exit isolation"


class ContractError(RuntimeError):
    pass


def cross_windows_build_argv(argv: list[str]) -> list[str]:
    if len(argv) < 2 or argv[:2] != ["cargo", "build"]:
        raise ContractError("cross-Windows build step must start with cargo build")
    return ["cargo", "xwin", *argv[1:]]


def cross_windows_build_env(env: dict[str, Any]) -> dict[str, Any]:
    if not isinstance(env, dict) or "RUSTFLAGS" in env:
        raise ContractError("cross-Windows build flags must be runner-owned")
    return {
        **env,
        "RUSTFLAGS": "-C link-arg=/debug:none -C link-arg=/build-id:no",
    }


def load_contract(path: Path, compat_id: str) -> dict[str, Any]:
    try:
        contract = json.loads(path.read_bytes())
    except (OSError, json.JSONDecodeError) as error:
        raise ContractError(f"cannot read test contract: {error}") from error
    expected = {
        "schema",
        "compat_id",
        "parameters",
        "cwd",
        "common_env",
        "generation",
        "tests",
        "build",
        "known_upstream_errata",
    }
    if (
        not isinstance(contract, dict)
        or set(contract) != expected
        or contract.get("schema") != 1
        or contract.get("compat_id") != compat_id
        or not isinstance(contract.get("generation"), list)
        or not isinstance(contract.get("tests"), list)
        or not isinstance(contract.get("build"), dict)
    ):
        raise ContractError("unsupported test contract")
    return contract


def expand(value: str, source: Path, cargo_target: Path) -> str:
    return value.replace("{source}", str(source)).replace("{cargo_target}", str(cargo_target))


def environment(
    common: dict[str, Any], extra: dict[str, Any], source: Path, cargo_target: Path
) -> dict[str, str]:
    if not isinstance(common, dict) or not isinstance(extra, dict):
        raise ContractError("contract environment must be an object")
    result = os.environ.copy()
    for name, value in {**common, **extra}.items():
        if not isinstance(name, str) or not ENV_NAME.fullmatch(name) or not isinstance(value, str):
            raise ContractError("contract environment contains an invalid entry")
        result[name] = expand(value, source, cargo_target)
    return result


def run_step(
    step: dict[str, Any],
    kind: str,
    cwd: Path,
    common_env: dict[str, Any],
    source: Path,
    cargo_target: Path,
) -> dict[str, Any]:
    allowed = {"name", "argv", "env"}
    if not isinstance(step, dict) or set(step) - allowed or not {"name", "argv"} <= set(step):
        raise ContractError(f"invalid {kind} step")
    argv = step["argv"]
    if (
        not isinstance(step["name"], str)
        or not isinstance(argv, list)
        or not argv
        or argv[0] != "cargo"
        or any(not isinstance(value, str) or not value for value in argv)
    ):
        raise ContractError(f"{kind} step must be a named cargo argv array")
    expanded = [expand(value, source, cargo_target) for value in argv]
    attempts = 2 if step["name"] == FLAKY_TUI_BACKGROUND_EXIT_STEP else 1
    for attempt in range(1, attempts + 1):
        result = subprocess.run(
            expanded,
            cwd=cwd,
            env=environment(common_env, step.get("env", {}), source, cargo_target),
            shell=False,
        )
        if not result.returncode or attempt == attempts:
            break
        print(
            f"{kind} step hit the known upstream event race; retrying once: {step['name']}",
            file=sys.stderr,
        )
    if result.returncode:
        raise ContractError(f"{kind} step failed ({result.returncode}): {step['name']}")
    return {"kind": kind, "name": step["name"], "argv": expanded, "exit_code": 0}


def digest(path: Path) -> str:
    hasher = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            hasher.update(chunk)
    return hasher.hexdigest()


def execute_version(executable: Path, expected: str) -> dict[str, Any]:
    result = subprocess.run(
        [str(executable), "--version"],
        cwd=executable.parent,
        check=False,
        capture_output=True,
        text=True,
        errors="replace",
    )
    if result.returncode or result.stdout.strip() != expected:
        raise ContractError(
            f"absolute-path artifact execution failed ({result.returncode}): "
            f"expected {expected!r}, got {result.stdout.strip()!r}"
        )
    return {
        "argv": [str(executable), "--version"],
        "exit_code": 0,
        "stdout": expected,
    }


def run_contract(
    manifest_path: Path,
    source: Path,
    cargo_target: Path,
    output: Path,
    cross_windows_msvc: bool = False,
    portable_evidence: bool = False,
) -> dict[str, Any]:
    for path, label in (
        (manifest_path, "manifest"),
        (source, "source"),
        (cargo_target, "cargo target"),
        (output, "output"),
    ):
        if not path.is_absolute():
            raise ContractError(f"{label} path must be absolute")
    manifest_path = manifest_path.resolve(strict=True)
    source = source.resolve(strict=True)
    if output.exists() or not output.parent.is_dir():
        raise ContractError("output must be a new file under an existing directory")
    if cargo_target.exists():
        raise ContractError("cargo target must not already exist")
    cargo_target.parent.mkdir(parents=True, exist_ok=True)
    payload = _load_payload(manifest_path)
    manifest = payload.manifest
    if cross_windows_msvc:
        if os.name == "nt":
            raise ContractError("cross-Windows mode requires a non-Windows host")
        if manifest["build_target"] != "x86_64-pc-windows-msvc":
            raise ContractError("cross-Windows mode requires x86_64-pc-windows-msvc")
    contract = load_contract(_payload_file(payload, "test-contract.json"), manifest["compat_id"])
    expected_cwd = expand(contract["cwd"], source, cargo_target)
    cwd = (source / "codex-rs").resolve(strict=True)
    if Path(expected_cwd).resolve(strict=True) != cwd:
        raise ContractError("contract cwd must be the candidate codex-rs directory")

    verification = verify(manifest_path, source, True, None)
    if portable_evidence:
        verification.update(
            {
                "manifest": "manifest.toml",
                "source": "{source}",
                "patches": [patch["path"] for patch in manifest["patches"]],
            }
        )
    steps = []
    for step in contract["generation"]:
        steps.append(run_step(step, "generation", cwd, contract["common_env"], source, cargo_target))
    for step in contract["tests"]:
        steps.append(run_step(step, "test", cwd, contract["common_env"], source, cargo_target))
    build = contract["build"]
    if not isinstance(build, dict) or set(build) != {"env", "argv", "artifact"}:
        raise ContractError("invalid build contract")
    build_argv = (
        cross_windows_build_argv(build["argv"]) if cross_windows_msvc else build["argv"]
    )
    build_env = cross_windows_build_env(build["env"]) if cross_windows_msvc else build["env"]
    build_step = {"name": "release build", "argv": build_argv, "env": build_env}
    steps.append(run_step(build_step, "build", cwd, contract["common_env"], source, cargo_target))
    artifact_path = Path(expand(build["artifact"], source, cargo_target)).resolve(strict=True)
    artifact = manifest["artifacts"][manifest["build_target"]]
    expected_version = f"codex-cli {manifest['codex_version']}"
    execution = (
        {
            "deferred": True,
            "reason": "cross-compiled Windows artifact requires local Windows acceptance",
        }
        if cross_windows_msvc
        else execute_version(artifact_path, expected_version)
    )
    actual_hash = digest(artifact_path)
    actual_size = artifact_path.stat().st_size
    canonical = actual_hash == artifact["sha256"] and actual_size == artifact["size"]
    report = {
        "schema": 1,
        "result": "pass",
        "compat_id": manifest["compat_id"],
        "source_verification": verification,
        "steps": steps,
        "artifact": {
            "path": build["artifact"] if portable_evidence else str(artifact_path),
            "size": actual_size,
            "sha256": actual_hash,
            "manifest_size": artifact["size"],
            "manifest_sha256": artifact["sha256"],
            "canonical_manifest_match": canonical,
            "release_eligible": canonical and not cross_windows_msvc,
            "absolute_path_execution": execution,
        },
        "known_upstream_errata": contract["known_upstream_errata"],
    }
    with output.open("xb") as destination:
        destination.write((json.dumps(report, indent=2, sort_keys=True) + "\n").encode())
    return report


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--cargo-target", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--cross-windows-msvc", action="store_true")
    parser.add_argument("--portable-evidence", action="store_true")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    try:
        report = run_contract(
            args.manifest,
            args.source,
            args.cargo_target,
            args.output,
            args.cross_windows_msvc,
            args.portable_evidence,
        )
    except (ContractError, OSError, VerificationError) as error:
        print(json.dumps({"schema": 1, "error": str(error)}, indent=2), file=sys.stderr)
        return 2
    print(json.dumps(report, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
