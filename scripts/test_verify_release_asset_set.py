#!/usr/bin/env python3
from __future__ import annotations

import hashlib
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path

MODULE_PATH = Path(__file__).with_name("verify_release_asset_set.py")
SPEC = importlib.util.spec_from_file_location("verify_release_asset_set", MODULE_PATH)
assert SPEC and SPEC.loader
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class ReleaseAssetSetTests(unittest.TestCase):
    def fixture(self) -> tuple[tempfile.TemporaryDirectory[str], Path]:
        holder: tempfile.TemporaryDirectory[str] = tempfile.TemporaryDirectory()
        root = Path(holder.name)
        (root / "codex.exe").write_bytes(b"codex")
        descriptor = {
            "repository": "DSLZL/CSA",
            "release_tag": "compat-rust-v9.9.9-native-join-p9",
            "source_commit": "a" * 40,
            "artifact": {
                "path": "codex.exe",
                "asset": "codex.exe",
                "size": 5,
                "sha256": hashlib.sha256(b"codex").hexdigest(),
            }
        }
        (root / "compatibility-release.json").write_text(json.dumps(descriptor), encoding="utf-8")
        (root / "manifest.toml").write_text("schema = 1\n", encoding="utf-8")
        sums = "".join(
            f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.name}\n"
            for path in sorted(root.iterdir())
            if path.is_file()
        )
        (root / "SHA256SUMS").write_text(sums, encoding="ascii")
        return holder, root

    def test_local_inventory_accepts_one_cli(self) -> None:
        holder, root = self.fixture()
        self.addCleanup(holder.cleanup)
        inventory = MODULE.local_inventory(root, "codex.exe")
        self.assertIn("codex.exe", inventory)

    def test_local_inventory_rejects_second_executable(self) -> None:
        holder, root = self.fixture()
        self.addCleanup(holder.cleanup)
        (root / "codex-app.exe").write_bytes(b"bad")
        with self.assertRaisesRegex(ValueError, "differs"):
            MODULE.local_inventory(root, "codex.exe")

    def test_local_inventory_accepts_multi_target_cli_set(self) -> None:
        holder, root = self.fixture()
        self.addCleanup(holder.cleanup)
        unix_asset = "rust-v9.9.9-native-join-p9--x86_64-unknown-linux-musl--codex"
        windows_asset = "rust-v9.9.9-native-join-p9--x86_64-pc-windows-msvc--codex.exe"
        (root / "codex.exe").rename(root / windows_asset)
        (root / unix_asset).write_bytes(b"codex-linux")
        descriptor_path = root / "compatibility-release.json"
        descriptor = json.loads(descriptor_path.read_text(encoding="utf-8"))
        descriptor["schema"] = 2
        artifact = descriptor.pop("artifact")
        artifact["asset"] = windows_asset
        descriptor["artifacts"] = {
            "x86_64-pc-windows-msvc": artifact,
            "x86_64-unknown-linux-musl": {
                "path": "codex",
                "asset": unix_asset,
                "size": 11,
                "sha256": hashlib.sha256(b"codex-linux").hexdigest(),
            },
        }
        descriptor_path.write_text(json.dumps(descriptor), encoding="utf-8")
        sums = "".join(
            f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.name}\n"
            for path in sorted(root.iterdir())
            if path.is_file() and path.name != "SHA256SUMS"
        )
        (root / "SHA256SUMS").write_text(sums, encoding="ascii")

        inventory = MODULE.local_inventory(root, None)
        self.assertIn(unix_asset, inventory)
        self.assertIn(windows_asset, inventory)

    def test_remote_inventory_must_match_digest_and_size(self) -> None:
        holder, root = self.fixture()
        self.addCleanup(holder.cleanup)
        local = MODULE.local_inventory(root, "codex.exe")
        remote = {
            "assets": [
                {
                    "name": name,
                    "state": "uploaded",
                    "size": metadata["size"],
                    "digest": "sha256:" + metadata["sha256"],
                }
                for name, metadata in local.items()
            ]
        }
        self.assertEqual(MODULE.remote_inventory(remote), local)

    def test_required_install_catalog_is_validated_outside_payload_checksums(self) -> None:
        holder, root = self.fixture()
        self.addCleanup(holder.cleanup)
        catalog = {
            "schema": 1,
            "repository": "DSLZL/CSA",
            "source_release_tag": "compat-rust-v9.9.9-native-join-p9",
            "source_commit": "a" * 40,
            "entries": [
                {
                    "compat_id": "rust-v9.9.9-native-join-p9",
                    "release_tag": "compat-rust-v9.9.9-native-join-p9",
                    "release_commit": "a" * 40,
                    "codex_version": "9.9.9",
                    "build_target": "x86_64-pc-windows-msvc",
                    "patch_revision": 9,
                    "recorded_on": "2026-08-29",
                }
            ],
        }
        (root / "install-catalog-v1.json").write_text(json.dumps(catalog), encoding="utf-8")
        inventory = MODULE.local_inventory(root, "codex.exe", require_install_catalog=True)
        self.assertIn("install-catalog-v1.json", inventory)
        with (root / "SHA256SUMS").open("a", encoding="ascii") as stream:
            stream.write("0" * 64 + "  install-catalog-v1.json\n")
        with self.assertRaisesRegex(ValueError, "outside"):
            MODULE.local_inventory(root, "codex.exe", require_install_catalog=True)


if __name__ == "__main__":
    unittest.main()
