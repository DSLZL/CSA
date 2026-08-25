#!/usr/bin/env python3
"""Validate local and GitHub compatibility Release asset inventories."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from pathlib import Path
from typing import Any

SHA256 = re.compile(r"[0-9a-f]{64}\Z")
FORBIDDEN_PRODUCT_TOKENS = ("app-server", "desktop", "codex-app", "exec-server", "mcp")
EXECUTABLE_SUFFIXES = {".exe", ".app", ".msi", ".dmg"}


def digest(path: Path) -> str:
    value = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            value.update(chunk)
    return value.hexdigest()


def local_inventory(root: Path, expected_executable: str) -> dict[str, dict[str, Any]]:
    root = root.resolve(strict=True)
    descriptor_path = root / "compatibility-release.json"
    if not descriptor_path.is_file():
        raise ValueError("compatibility-release.json is missing")
    descriptor = json.loads(descriptor_path.read_text(encoding="utf-8"))
    artifact = descriptor.get("artifact")
    if not isinstance(artifact, dict) or artifact.get("asset") != expected_executable:
        raise ValueError("descriptor artifact does not match the expected CLI executable asset")
    if any(token in expected_executable.lower() for token in FORBIDDEN_PRODUCT_TOKENS):
        raise ValueError(f"forbidden non-CLI product asset: {expected_executable}")

    files = sorted(path for path in root.iterdir() if path.is_file())
    executables = [path.name for path in files if path.suffix.lower() in EXECUTABLE_SUFFIXES]
    if executables != [expected_executable]:
        raise ValueError(
            f"CLI-only Release must contain exactly {expected_executable!r} as executable; got {executables!r}"
        )
    inventory = {
        path.name: {"size": path.stat().st_size, "sha256": digest(path)}
        for path in files
    }
    if "SHA256SUMS" not in inventory or "compatibility-release.json" not in inventory:
        raise ValueError("release asset directory must contain SHA256SUMS and compatibility-release.json")
    return inventory


def remote_inventory(release: dict[str, Any]) -> dict[str, dict[str, Any]]:
    assets = release.get("assets")
    if not isinstance(assets, list):
        raise ValueError("GitHub Release JSON has no assets array")
    inventory: dict[str, dict[str, Any]] = {}
    for item in assets:
        if not isinstance(item, dict):
            raise ValueError("GitHub Release asset is not an object")
        name = item.get("name")
        state = item.get("state")
        size = item.get("size")
        value = item.get("digest")
        if not isinstance(name, str) or not name or "/" in name or "\\" in name:
            raise ValueError(f"unsafe GitHub Release asset name: {name!r}")
        if name in inventory:
            raise ValueError(f"duplicate GitHub Release asset: {name}")
        if state != "uploaded" or not isinstance(size, int) or size <= 0:
            raise ValueError(f"GitHub Release asset is incomplete: {name}")
        if not isinstance(value, str) or not value.startswith("sha256:") or not SHA256.fullmatch(value[7:]):
            raise ValueError(f"GitHub Release asset has no usable SHA-256 digest: {name}")
        inventory[name] = {"size": size, "sha256": value[7:]}
    return inventory


def command_local(args: argparse.Namespace) -> None:
    inventory = local_inventory(Path(args.root), args.expected_executable)
    print(json.dumps({"schema": 1, "status": "pass", "assets": inventory}, indent=2, sort_keys=True))


def command_remote(args: argparse.Namespace) -> None:
    local = local_inventory(Path(args.root), args.expected_executable)
    release = json.loads(Path(args.release_json).read_text(encoding="utf-8"))
    remote = remote_inventory(release)
    if local != remote:
        missing = sorted(set(local) - set(remote))
        unknown = sorted(set(remote) - set(local))
        mismatched = sorted(name for name in set(local) & set(remote) if local[name] != remote[name])
        raise ValueError(
            f"GitHub Release assets differ from local authority; missing={missing}, unknown={unknown}, mismatched={mismatched}"
        )
    print(json.dumps({"schema": 1, "status": "pass", "asset_count": len(local)}, sort_keys=True))


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    sub = result.add_subparsers(dest="command", required=True)
    local = sub.add_parser("local")
    local.add_argument("--root", required=True)
    local.add_argument("--expected-executable", required=True)
    local.set_defaults(handler=command_local)
    remote = sub.add_parser("remote")
    remote.add_argument("--root", required=True)
    remote.add_argument("--expected-executable", required=True)
    remote.add_argument("--release-json", required=True)
    remote.set_defaults(handler=command_remote)
    return result


def main() -> int:
    args = parser().parse_args()
    try:
        args.handler(args)
    except (OSError, UnicodeError, json.JSONDecodeError, ValueError) as error:
        print(f"ERROR: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
