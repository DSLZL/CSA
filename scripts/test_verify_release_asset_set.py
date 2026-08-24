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
        with self.assertRaisesRegex(ValueError, "exactly"):
            MODULE.local_inventory(root, "codex.exe")

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


if __name__ == "__main__":
    unittest.main()
