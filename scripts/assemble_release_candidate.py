#!/usr/bin/env python3
"""Validate inputs and assemble a self-contained CSA manager release candidate."""

from __future__ import annotations

import argparse
import gzip
import hashlib
import json
import os
import re
import shutil
import subprocess
import tarfile
import tempfile
import tomllib
from pathlib import Path, PurePosixPath
from typing import Any

REVISION = re.compile(r"(?:[0-9a-f]{40}|[0-9a-f]{64})\Z")
ROOT_KEYS = {
    "schema",
    "release_version",
    "source",
    "meta_tarball",
    "platforms",
    "signing",
    "manual_gates",
}
SOURCE_PATHS = [
    ".github",
    "Cargo.lock",
    "Cargo.toml",
    "LICENSE",
    "THIRD_PARTY_NOTICES.md",
    "build.rs",
    "docs",
    "npm",
    "release",
    "rust-toolchain.toml",
    "schemas",
    "scripts",
    "src",
    "tests",
]


class ReleaseError(RuntimeError):
    pass


def digest(path: Path) -> str:
    hasher = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            hasher.update(chunk)
    return hasher.hexdigest()


def json_bytes(value: object) -> bytes:
    return (json.dumps(value, indent=2, sort_keys=True) + "\n").encode()


def write_json(path: Path, value: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(json_bytes(value))


def load_json(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_bytes())
    except (OSError, json.JSONDecodeError) as error:
        raise ReleaseError(f"cannot read JSON {path}: {error}") from error
    if not isinstance(value, dict):
        raise ReleaseError(f"JSON root must be an object: {path}")
    return value


def input_file(base: Path, value: object, label: str) -> Path:
    if not isinstance(value, str) or not value:
        raise ReleaseError(f"{label} must be a non-empty path")
    candidate = Path(value)
    candidate = candidate if candidate.is_absolute() else base / candidate
    if candidate.is_symlink():
        raise ReleaseError(f"{label} must not be a symlink: {candidate}")
    try:
        path = candidate.resolve(strict=True)
    except OSError as error:
        raise ReleaseError(f"{label} does not exist: {candidate}") from error
    if path.is_symlink() or not path.is_file():
        raise ReleaseError(f"{label} must be a non-symlink regular file: {path}")
    return path


def copy_file(source: Path, destination: Path, root: Path) -> dict[str, object]:
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source, destination)
    return {
        "path": destination.relative_to(root).as_posix(),
        "size": destination.stat().st_size,
        "sha256": digest(destination),
    }


def read_tar_json(archive: Path, member_name: str) -> dict[str, Any]:
    try:
        with tarfile.open(archive, "r:gz") as package:
            member = package.getmember(member_name)
            if not member.isfile():
                raise ReleaseError(f"tar member is not a file: {member_name}")
            handle = package.extractfile(member)
            if handle is None:
                raise ReleaseError(f"cannot read tar member: {member_name}")
            value = json.loads(handle.read())
    except (KeyError, OSError, tarfile.TarError, json.JSONDecodeError) as error:
        raise ReleaseError(f"cannot read {member_name} from {archive}: {error}") from error
    if not isinstance(value, dict):
        raise ReleaseError(f"tar JSON root must be an object: {archive}:{member_name}")
    return value


def read_tar_bytes(archive: Path, member_name: str) -> bytes:
    try:
        with tarfile.open(archive, "r:gz") as package:
            member = package.getmember(member_name)
            if not member.isfile():
                raise ReleaseError(f"tar member is not a file: {member_name}")
            handle = package.extractfile(member)
            if handle is None:
                raise ReleaseError(f"cannot read tar member: {member_name}")
            return handle.read()
    except (KeyError, OSError, tarfile.TarError) as error:
        raise ReleaseError(f"cannot read {member_name} from {archive}: {error}") from error


def validate_tar_files(archive: Path, expected: set[str]) -> None:
    try:
        with tarfile.open(archive, "r:gz") as package:
            files = []
            for member in package.getmembers():
                if member.isdir():
                    continue
                if not member.isfile():
                    raise ReleaseError(f"npm tarball contains a non-file member: {member.name}")
                files.append(PurePosixPath(member.name).as_posix())
    except (OSError, tarfile.TarError) as error:
        raise ReleaseError(f"cannot inspect npm tarball {archive}: {error}") from error
    if len(files) != len(set(files)) or set(files) != expected:
        raise ReleaseError(f"npm tarball file set mismatch: {archive.name}")


def load_matrix(repository: Path) -> tuple[dict[str, Any], dict[str, dict[str, Any]]]:
    matrix = load_json(repository / "release" / "support-matrix.json")
    if set(matrix) != {"schema", "manager_version", "rust_toolchain", "node_test_versions", "platforms"}:
        raise ReleaseError("support matrix keys do not match schema 1")
    if matrix["schema"] != 1 or not isinstance(matrix["platforms"], list):
        raise ReleaseError("unsupported support matrix")
    platforms: dict[str, dict[str, Any]] = {}
    for platform in matrix["platforms"]:
        if not isinstance(platform, dict) or not isinstance(platform.get("id"), str):
            raise ReleaseError("invalid support matrix platform")
        if platform["id"] in platforms:
            raise ReleaseError(f"duplicate support matrix platform: {platform['id']}")
        platforms[platform["id"]] = platform
    return matrix, platforms


def validate_meta_tarball(
    archive: Path, version: str, platforms: dict[str, dict[str, Any]]
) -> None:
    validate_tar_files(
        archive,
        {
            "package/LICENSE",
            "package/README.md",
            "package/THIRD_PARTY_NOTICES.md",
            "package/bin/csa.js",
            "package/package.json",
            "package/platforms.json",
        },
    )
    manifest = read_tar_json(archive, "package/package.json")
    expected_optional = {platform["npm_package"]: version for platform in platforms.values()}
    if (
        manifest.get("name") != "@dslzl/csa"
        or manifest.get("version") != version
        or manifest.get("optionalDependencies") != expected_optional
        or manifest.get("bin") != {"csa": "bin/csa.js"}
        or "scripts" in manifest
    ):
        raise ReleaseError("meta npm tarball metadata does not match the release matrix")


def validate_platform_tarball(
    archive: Path,
    manager: Path,
    version: str,
    platform: dict[str, Any],
) -> str:
    validate_tar_files(
        archive,
        {
            "package/LICENSE",
            "package/README.md",
            "package/THIRD_PARTY_NOTICES.md",
            f"package/bin/{platform['binary']}",
            "package/package.json",
        },
    )
    manifest = read_tar_json(archive, "package/package.json")
    binding = manifest.get("csa")
    expected_binding = {
        "schema": 1,
        "target": platform["target"],
        "binary": f"bin/{platform['binary']}",
        "sha256": digest(manager),
    }
    if (
        manifest.get("name") != platform["npm_package"]
        or manifest.get("version") != version
        or manifest.get("os") != [platform["os"]]
        or manifest.get("cpu") != [platform["arch"]]
        or binding != expected_binding
        or "scripts" in manifest
    ):
        raise ReleaseError(f"platform npm metadata mismatch: {platform['id']}")
    binary = read_tar_bytes(archive, f"package/bin/{platform['binary']}")
    packaged_hash = hashlib.sha256(binary).hexdigest()
    if packaged_hash != expected_binding["sha256"]:
        raise ReleaseError(f"platform npm binary mismatch: {platform['id']}")
    return packaged_hash


def source_files(repository: Path) -> list[Path]:
    paths: list[Path] = []
    selected = SOURCE_PATHS + [path.name for path in repository.glob("README*.md")]
    for name in sorted(set(selected)):
        root = repository / name
        if not root.exists():
            continue
        candidates = [root] if root.is_file() else sorted(root.rglob("*"))
        for path in candidates:
            if path.is_dir() or "__pycache__" in path.parts or path.suffix == ".pyc":
                continue
            if path.is_symlink() or not path.is_file():
                raise ReleaseError(f"source bundle contains unsupported file: {path}")
            paths.append(path)
    return sorted(set(paths), key=lambda path: path.relative_to(repository).as_posix())


def write_source_bundle(
    repository: Path, destination: Path, version: str, output_root: Path
) -> dict[str, object]:
    files = source_files(repository)
    manifest_entries = []
    aggregate = hashlib.sha256()
    for path in files:
        relative = path.relative_to(repository).as_posix()
        file_hash = digest(path)
        size = path.stat().st_size
        manifest_entries.append({"path": relative, "size": size, "sha256": file_hash})
        aggregate.update(f"{relative}\0{size}\0{file_hash}\n".encode())

    destination.parent.mkdir(parents=True, exist_ok=True)
    prefix = f"csa-{version}"
    with destination.open("wb") as raw:
        with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode="w") as archive:
                for path in files:
                    relative = path.relative_to(repository).as_posix()
                    info = archive.gettarinfo(str(path), f"{prefix}/{relative}")
                    info.uid = info.gid = 0
                    info.uname = info.gname = ""
                    info.mtime = 0
                    info.mode = 0o755 if relative.startswith("scripts/") else 0o644
                    with path.open("rb") as source:
                        archive.addfile(info, source)
    return {
        "path": destination.relative_to(output_root).as_posix(),
        "size": destination.stat().st_size,
        "sha256": digest(destination),
        "tree_sha256": aggregate.hexdigest(),
        "files": manifest_entries,
    }


def dependency_inventory(repository: Path) -> dict[str, object]:
    try:
        lock = tomllib.loads((repository / "Cargo.lock").read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise ReleaseError(f"cannot read Cargo.lock: {error}") from error
    packages = []
    for package in lock.get("package", []):
        packages.append(
            {
                "name": package["name"],
                "version": package["version"],
                "source": package.get("source"),
                "checksum": package.get("checksum"),
                "dependencies": package.get("dependencies", []),
            }
        )
    packages.sort(key=lambda package: (package["name"], package["version"], package["source"] or ""))
    return {
        "schema": 1,
        "type": "cargo-lock-dependency-inventory",
        "lock_sha256": digest(repository / "Cargo.lock"),
        "packages": packages,
    }


def validate_inputs(
    value: dict[str, Any], matrix: dict[str, Any], platforms: dict[str, dict[str, Any]]
) -> None:
    if set(value) != ROOT_KEYS or value.get("schema") != 1:
        raise ReleaseError("release input keys do not match schema 1")
    if value["release_version"] != matrix["manager_version"]:
        raise ReleaseError("release version does not match the support matrix")
    source = value["source"]
    if not isinstance(source, dict) or set(source) != {"revision", "ref", "repository"}:
        raise ReleaseError("invalid source provenance")
    if not isinstance(source["revision"], str) or not source["revision"]:
        raise ReleaseError("source revision must be non-empty")
    entries = value["platforms"]
    if not isinstance(entries, dict) or set(entries) != set(platforms):
        raise ReleaseError("release inputs must name every support-matrix platform")
    for platform_id, entry in entries.items():
        if not isinstance(entry, dict) or entry.get("status") not in {"pass", "not_verified"}:
            raise ReleaseError(f"invalid platform result: {platform_id}")
        if not isinstance(entry.get("reproduce"), list) or not entry["reproduce"]:
            raise ReleaseError(f"platform reproduction commands are required: {platform_id}")
        if entry["status"] == "pass":
            if set(entry) != {"status", "manager", "npm_tarball", "evidence", "reproduce"}:
                raise ReleaseError(f"passing platform keys do not match schema: {platform_id}")
        elif set(entry) != {"status", "reason", "reproduce"} or not entry.get("reason"):
            raise ReleaseError(f"not_verified platform requires only a reason: {platform_id}")
    signing = value["signing"]
    if (
        not isinstance(signing, dict)
        or signing.get("status") not in {"verified", "not_available", "not_executed"}
        or not isinstance(signing.get("reason"), str)
        or not signing["reason"]
    ):
        raise ReleaseError("invalid signing result")
    if signing["status"] == "verified" and "signature" not in signing:
        raise ReleaseError("verified signing requires a signature path")
    if signing["status"] != "verified" and set(signing) != {"status", "reason"}:
        raise ReleaseError("unsigned signing result must not include a signature")
    if not isinstance(value["manual_gates"], dict):
        raise ReleaseError("manual_gates must be an object")
    allowed = {"pass", "not_verified", "not_executed"}
    if any(status not in allowed for status in value["manual_gates"].values()):
        raise ReleaseError("invalid manual gate status")


def verify_source_revision(repository: Path, revision: str) -> dict[str, str]:
    if not REVISION.fullmatch(revision):
        return {"status": "not_verified", "reason": "source revision is not a commit hash"}
    try:
        head = subprocess.run(
            ["git", "rev-parse", "HEAD"],
            cwd=repository,
            check=False,
            capture_output=True,
            text=True,
        )
        status = subprocess.run(
            ["git", "status", "--porcelain=v1", "--untracked-files=all"],
            cwd=repository,
            check=False,
            capture_output=True,
            text=True,
        )
    except OSError as error:
        return {"status": "not_verified", "reason": f"git verification failed: {error}"}
    actual = head.stdout.strip()
    if head.returncode or actual != revision:
        return {"status": "not_verified", "reason": "source revision does not match repository HEAD"}
    if status.returncode or status.stdout:
        return {"status": "not_verified", "reason": "source repository is not clean"}
    return {"status": "verified_commit", "commit": actual}


def render_readiness(report: dict[str, Any]) -> str:
    lines = [
        "# Release Readiness",
        "",
        f"Overall status: **{report['overall_status']}**",
        "",
        f"Release candidate: `{report['release_version']}`",
        f"Source revision: `{report['source']['revision']}` ({report['source']['status']})",
        f"Source tree SHA-256: `{report['source']['tree_sha256']}`",
        f"Signing: `{report['signing']['status']}` - {report['signing']['reason']}",
        "",
        "## Platform Matrix",
        "",
        "| Platform | Runner | Manager/npm |",
        "| --- | --- | --- |",
    ]
    for platform in report["platforms"]:
        lines.append(
            f"| `{platform['id']}` | `{platform['runner']}` | `{platform['status']}` |"
        )
    unverified = [platform for platform in report["platforms"] if platform.get("reason")]
    if unverified:
        lines.extend(["", "## Unverified Platforms", ""])
        for platform in unverified:
            lines.append(f"- `{platform['id']}`: {platform['reason']}")
    lines.extend(["", "## Manual Gates", ""])
    for name, status in report["manual_gates"].items():
        lines.append(f"- `{name}`: `{status}`")
    lines.extend(
        [
            "",
            "## Reproduction",
            "",
            "Run the commands recorded under each platform in `release-readiness.json` on the named native runner. Reassemble from a clean commit/tag and compare `SHA256SUMS` before publication.",
            "",
        ]
    )
    return "\n".join(lines)


def assemble(repository: Path, input_path: Path, output: Path) -> dict[str, Any]:
    if not repository.is_absolute() or not input_path.is_absolute() or not output.is_absolute():
        raise ReleaseError("repository, input, and output paths must be absolute")
    repository = repository.resolve(strict=True)
    input_path = input_path.resolve(strict=True)
    if output.exists() or not output.parent.is_dir():
        raise ReleaseError("output must be a new path under an existing directory")
    matrix, platforms = load_matrix(repository)
    inputs = load_json(input_path)
    validate_inputs(inputs, matrix, platforms)
    base = input_path.parent
    version = inputs["release_version"]
    meta_tarball = input_file(base, inputs["meta_tarball"], "meta tarball")
    validate_meta_tarball(meta_tarball, version, platforms)

    temporary = Path(tempfile.mkdtemp(prefix=f".{output.name}.", dir=output.parent))
    try:
        assets: list[dict[str, object]] = []
        platform_reports = []
        copied_meta = temporary / "npm" / meta_tarball.name
        assets.append(copy_file(meta_tarball, copied_meta, temporary))

        for platform_id, platform in platforms.items():
            entry = inputs["platforms"][platform_id]
            report = {
                "id": platform_id,
                "runner": platform["runner"],
                "target": platform["target"],
                "status": entry["status"],
                "reproduce": entry["reproduce"],
            }
            if entry["status"] == "pass":
                manager = input_file(base, entry["manager"], f"{platform_id} manager")
                npm_tarball = input_file(base, entry["npm_tarball"], f"{platform_id} npm tarball")
                evidence = input_file(base, entry["evidence"], f"{platform_id} evidence")
                if manager.name != platform["binary"]:
                    raise ReleaseError(f"manager filename mismatch: {platform_id}")
                evidence_value = load_json(evidence)
                if evidence_value.get("result") != "pass":
                    raise ReleaseError(f"platform evidence is not passing: {platform_id}")
                if not isinstance(evidence_value.get("isolation"), dict):
                    raise ReleaseError(f"platform evidence lacks isolation data: {platform_id}")
                manager_hash = validate_platform_tarball(npm_tarball, manager, version, platform)
                manager_copy = temporary / "manager" / platform_id / platform["binary"]
                manager_asset = copy_file(manager, manager_copy, temporary)
                if platform["os"] != "win32":
                    manager_copy.chmod(0o755)
                npm_asset = copy_file(
                    npm_tarball, temporary / "npm" / npm_tarball.name, temporary
                )
                evidence_asset = copy_file(
                    evidence,
                    temporary / "evidence" / f"platform-{platform_id}.json",
                    temporary,
                )
                assets.extend([manager_asset, npm_asset, evidence_asset])
                report.update(
                    {
                        "manager_sha256": manager_hash,
                        "npm_tarball_sha256": npm_asset["sha256"],
                        "evidence": evidence_asset["path"],
                        "isolation": evidence_value["isolation"],
                    }
                )
            else:
                report["reason"] = entry["reason"]
                report["isolation"] = {
                    "status": "not_verified",
                    "reason": entry["reason"],
                }
            platform_reports.append(report)

        for name in ["LICENSE", "THIRD_PARTY_NOTICES.md"]:
            assets.append(copy_file(repository / name, temporary / name, temporary))

        inventory = dependency_inventory(repository)
        write_json(temporary / "dependency-inventory.json", inventory)
        source_bundle = write_source_bundle(
            repository,
            temporary / "source" / f"csa-{version}.tar.gz",
            version,
            temporary,
        )
        write_json(temporary / "source" / "source-manifest.json", source_bundle)

        source_verification = verify_source_revision(repository, inputs["source"]["revision"])
        signing = dict(inputs["signing"])
        if signing["status"] == "verified":
            signature = input_file(base, signing["signature"], "release signature")
            signature_asset = copy_file(
                signature, temporary / "signatures" / signature.name, temporary
            )
            assets.append(signature_asset)
            signing["signature"] = signature_asset

        all_platforms = all(report["status"] == "pass" for report in platform_reports)
        overall = (
            "ready"
            if all_platforms
            and source_verification["status"] == "verified_commit"
            else "not_ready"
        )
        report = {
            "schema": 1,
            "release_version": version,
            "overall_status": overall,
            "source": {
                **inputs["source"],
                **source_verification,
                "tree_sha256": source_bundle["tree_sha256"],
                "bundle_sha256": source_bundle["sha256"],
            },
            "platforms": platform_reports,
            "signing": signing,
            "manual_gates": inputs["manual_gates"],
            "dependency_inventory": {
                "type": inventory["type"],
                "packages": len(inventory["packages"]),
                "lock_sha256": inventory["lock_sha256"],
            },
        }
        write_json(temporary / "release-readiness.json", report)
        (temporary / "release-readiness.md").write_text(render_readiness(report), encoding="utf-8")
        provenance = {
            "schema": 1,
            "builder": "scripts/assemble_release_candidate.py",
            "source": report["source"],
            "support_matrix_sha256": digest(repository / "release" / "support-matrix.json"),
            "input_manifest_sha256": digest(input_path),
            "assets": sorted(assets, key=lambda asset: str(asset["path"])),
            "signing": signing,
        }
        write_json(temporary / "provenance.json", provenance)

        checksum_lines = []
        for path in sorted(temporary.rglob("*")):
            if path.is_file() and path.name != "SHA256SUMS":
                checksum_lines.append(f"{digest(path).upper()}  {path.relative_to(temporary).as_posix()}")
        (temporary / "SHA256SUMS").write_text("\n".join(checksum_lines) + "\n", encoding="ascii")
        temporary.replace(output)
        return report
    except Exception:
        shutil.rmtree(temporary, ignore_errors=True)
        raise


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repository", type=Path, required=True)
    parser.add_argument("--input", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    try:
        report = assemble(args.repository, args.input, args.output)
    except (OSError, ReleaseError) as error:
        print(json.dumps({"schema": 1, "error": str(error)}, indent=2), file=os.sys.stderr)
        return 2
    print(json.dumps(report, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
