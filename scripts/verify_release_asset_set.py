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

from compat_catalog import CatalogError, INSTALL_CATALOG_NAME, load_json, validate_install_catalog

SHA256 = re.compile(r"[0-9a-f]{64}\Z")
FORBIDDEN_PRODUCT_TOKENS = ("app-server", "desktop", "codex-app", "exec-server", "mcp")
EXECUTABLE_SUFFIXES = {".exe", ".app", ".msi", ".dmg"}


def digest(path: Path) -> str:
    value = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            value.update(chunk)
    return value.hexdigest()


def local_inventory(
    root: Path,
    expected_executable: str,
    require_install_catalog: bool = False,
) -> dict[str, dict[str, Any]]:
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
    catalog_path = root / INSTALL_CATALOG_NAME
    if require_install_catalog and not catalog_path.is_file():
        raise ValueError(f"{INSTALL_CATALOG_NAME} is missing")
    if catalog_path.is_file():
        identity = [descriptor.get(key) for key in ("repository", "release_tag", "source_commit")]
        if not all(isinstance(value, str) and value for value in identity):
            raise ValueError("release descriptor has no catalog source identity")
        try:
            validate_install_catalog(
                load_json(catalog_path),
                expected_repository=identity[0],
                expected_source_release_tag=identity[1],
                expected_source_commit=identity[2],
            )
        except CatalogError as error:
            raise ValueError(str(error)) from error
        checksum_names = {
            line.split(maxsplit=1)[1].strip().lstrip("*")
            for line in (root / "SHA256SUMS").read_text(encoding="ascii").splitlines()
            if len(line.split(maxsplit=1)) == 2
        }
        if INSTALL_CATALOG_NAME in checksum_names:
            raise ValueError(f"{INSTALL_CATALOG_NAME} must stay outside the immutable payload checksum set")
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
    inventory = local_inventory(Path(args.root), args.expected_executable, args.require_install_catalog)
    print(json.dumps({"schema": 1, "status": "pass", "assets": inventory}, indent=2, sort_keys=True))


def command_remote(args: argparse.Namespace) -> None:
    local = local_inventory(Path(args.root), args.expected_executable, args.require_install_catalog)
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
    local.add_argument("--require-install-catalog", action="store_true")
    local.set_defaults(handler=command_local)
    remote = sub.add_parser("remote")
    remote.add_argument("--root", required=True)
    remote.add_argument("--expected-executable", required=True)
    remote.add_argument("--release-json", required=True)
    remote.add_argument("--require-install-catalog", action="store_true")
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
