from __future__ import annotations

import os
import tempfile
import unittest
from pathlib import Path

from verify_patch_payload import VerificationError, verify


ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "payload" / "codex" / "rust-v0.147.0-native-join-p1" / "manifest.toml"


class PatchPayloadVerifierTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        source = os.environ.get("CODEX_PATCH_TEST_SOURCE")
        if not source:
            raise unittest.SkipTest("CODEX_PATCH_TEST_SOURCE is required")
        cls.source = Path(source).resolve()

    def test_exact_manifest_passes_without_applying(self) -> None:
        result = verify(MANIFEST.resolve(), self.source, False, None)
        self.assertFalse(result["applied"])

    def test_unknown_schema_and_key_fail_without_artifact(self) -> None:
        self._assert_rejected("schema = 1", "schema = 2", "unsupported manifest")
        self._assert_rejected(
            "\n[[patches]]",
            "\nunknown_field = true\n\n[[patches]]",
            r"unknown=\['unknown_field'\]",
        )

    def test_schema_type_bypasses_are_rejected_without_artifact(self) -> None:
        self._assert_rejected("schema = 1", "schema = true", "unsupported manifest")
        self._assert_rejected("patch_api = 1", "patch_api = true", "unsupported manifest")
        self._assert_rejected(
            "patch_set_version = 1",
            "patch_set_version = true",
            "patch_set_version must be a positive integer",
        )
        self._assert_rejected("size = 299944448", "size = true", "artifact size must be positive")
        self._assert_rejected(
            'build_target = "x86_64-pc-windows-msvc"',
            'build_target = "../x86_64-pc-windows-msvc"',
            "invalid build_target",
        )

    def test_unknown_version_fails_without_artifact(self) -> None:
        self._assert_rejected(
            'codex_version = "0.147.0"',
            'codex_version = "9.9.9"',
            "workspace version mismatch",
        )

    def test_drifted_commit_fails_without_artifact(self) -> None:
        self._assert_rejected(
            'upstream_commit = "be6e8eac029b183056b7e4402879f15d2c85f61b"',
            'upstream_commit = "0000000000000000000000000000000000000000"',
            "exact commit mismatch",
        )

    def _assert_rejected(self, old: str, new: str, message: str) -> None:
        text = MANIFEST.read_text(encoding="utf-8")
        self.assertIn(old, text)
        with tempfile.TemporaryDirectory(prefix="codex-patch-negative-") as temp_dir:
            temp = Path(temp_dir)
            manifest = temp / "manifest.toml"
            manifest.write_text(text.replace(old, new, 1), encoding="utf-8")
            artifact = temp / "codex.exe"
            with self.assertRaisesRegex(VerificationError, message):
                verify(manifest.resolve(), self.source, False, artifact.resolve())
            self.assertFalse(artifact.exists())


if __name__ == "__main__":
    unittest.main()
