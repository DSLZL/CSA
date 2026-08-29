#!/usr/bin/env python3
"""Generate deterministic CSA Manager or patched Codex release notes."""

from __future__ import annotations

import argparse
from dataclasses import dataclass
from functools import cmp_to_key
import json
import os
from pathlib import Path, PurePosixPath
import re
import subprocess
import sys
import tempfile


SEMVER = re.compile(
    r"(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)"
    r"(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?"
    r"(?:\+([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?\Z"
)
COMPAT_ID = re.compile(
    r"rust-v(?P<version>(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*))-"
    r"(?P<family>[A-Za-z0-9][A-Za-z0-9._-]*)-p(?P<revision>[1-9]\d*)\Z"
)
CONVENTIONAL = re.compile(
    r"(?P<type>feat|fix|perf|refactor|docs|ci|build|test|chore)"
    r"(?:\((?P<scope>[A-Za-z0-9._/-]+)\))?(?P<breaking>!)?:\s+(?P<title>.+)\Z"
)
SAFE_REF = re.compile(r"[0-9A-Za-z][0-9A-Za-z._/@:+~^-]{0,199}\Z")
SAFE_VALUE = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]*\Z")
SHA1 = re.compile(r"[0-9a-f]{40}\Z")
SHA256 = re.compile(r"[0-9a-f]{64}\Z")
SKIP_TRAILER = re.compile(r"Changelog:\s*skip\Z", re.IGNORECASE)
REPOSITORY_URL = "https://github.com/DSLZL/CSA"

MANAGER_PREFIXES = (
    "src/",
    "npm/",
    "docs/",
    ".github/actions/setup-codex-rust-cache/",
)
MANAGER_FILES = {
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    "build.rs",
    "README.md",
    "README_ZH.md",
    "release/support-matrix.json",
    "release/release-inputs.schema.json",
    ".github/release.yml",
    ".github/workflows/ci.yml",
    ".github/workflows/publish-npm.yml",
    ".github/workflows/release-csa.yml",
    "scripts/assemble_release_candidate.py",
    "scripts/ci_release.py",
    "scripts/generate_release_notes.py",
    "scripts/stage_npm_packages.mjs",
    "scripts/test_installed_launcher.mjs",
    "scripts/test_npm_launcher.mjs",
}
COMPAT_FILES = {
    "release/compatibility-index.json",
    ".github/release.yml",
    ".github/workflows/release-patched-codex.yml",
    ".github/workflows/validate-patched-codex.yml",
    ".github/workflows/watch-codex-release.yml",
    "scripts/build_patched_codex_bundle.sh",
    "scripts/check_sccache_stats.py",
    "scripts/compat_catalog.py",
    "scripts/compat_release.py",
    "scripts/compatibility_audit.py",
    "scripts/generate_release_notes.py",
    "scripts/run_patch_contract.py",
    "scripts/validation_evidence.py",
    "scripts/verify_patch_payload.py",
    "scripts/verify_release_asset_set.py",
}
COMPAT_PREFIXES = ("release/build-profiles/", "tests/ui/")


class ReleaseNotesError(RuntimeError):
    pass


@dataclass(frozen=True)
class Version:
    core: tuple[int, int, int]
    prerelease: tuple[str, ...] | None


@dataclass(frozen=True)
class Commit:
    sha: str
    short: str
    subject: str
    body: str
    paths: tuple[str, ...]


@dataclass(frozen=True)
class ParsedCommit:
    kind: str
    scope: str | None
    title: str
    breaking: bool


def parse_version(value: str) -> Version:
    match = SEMVER.fullmatch(value)
    if match is None:
        raise ReleaseNotesError(f"invalid semantic version: {value!r}")
    prerelease = match.group(4)
    parts = tuple(prerelease.split(".")) if prerelease else None
    if parts and any(part.isdigit() and len(part) > 1 and part.startswith("0") for part in parts):
        raise ReleaseNotesError(f"numeric prerelease identifiers must not have leading zeroes: {value!r}")
    return Version(tuple(int(part) for part in match.groups()[:3]), parts)


def compare_versions(left: Version, right: Version) -> int:
    if left.core != right.core:
        return (left.core > right.core) - (left.core < right.core)
    if left.prerelease is None or right.prerelease is None:
        return (left.prerelease is None) - (right.prerelease is None)
    for left_part, right_part in zip(left.prerelease, right.prerelease):
        if left_part == right_part:
            continue
        left_numeric = left_part.isdigit()
        right_numeric = right_part.isdigit()
        if left_numeric and right_numeric:
            return (int(left_part) > int(right_part)) - (int(left_part) < int(right_part))
        if left_numeric != right_numeric:
            return -1 if left_numeric else 1
        return (left_part > right_part) - (left_part < right_part)
    return (len(left.prerelease) > len(right.prerelease)) - (
        len(left.prerelease) < len(right.prerelease)
    )


def run_git(repository: Path, *args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    result = subprocess.run(
        ["git", "-C", str(repository), *args],
        check=False,
        capture_output=True,
        text=True,
        encoding="utf-8",
    )
    if check and result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or f"exit {result.returncode}"
        raise ReleaseNotesError(f"git {' '.join(args)} failed: {detail}")
    return result


def resolve_commit(repository: Path, ref: str) -> str:
    if SAFE_REF.fullmatch(ref) is None:
        raise ReleaseNotesError(f"current ref contains unsafe characters: {ref!r}")
    commit = run_git(
        repository,
        "rev-parse",
        "--verify",
        "--end-of-options",
        f"{ref}^{{commit}}",
    ).stdout.strip()
    if SHA1.fullmatch(commit) is None:
        raise ReleaseNotesError("Git did not resolve current ref to a lowercase SHA-1 commit")
    return commit


def tag_commit(repository: Path, tag: str) -> str | None:
    result = run_git(
        repository,
        "rev-parse",
        "--verify",
        "--end-of-options",
        f"refs/tags/{tag}^{{commit}}",
        check=False,
    )
    if result.returncode != 0:
        return None
    commit = result.stdout.strip()
    if SHA1.fullmatch(commit) is None:
        raise ReleaseNotesError(f"Git returned an invalid commit for tag {tag}")
    return commit


def merged_tags(repository: Path, current_commit: str) -> list[str]:
    tags = run_git(repository, "tag", "--merged", current_commit, "--list").stdout.splitlines()
    if len(tags) != len(set(tags)):
        raise ReleaseNotesError("Git returned duplicate tag names")
    return tags


def require_current_tag_identity(
    repository: Path, current_tag: str, current_commit: str
) -> None:
    existing = tag_commit(repository, current_tag)
    if existing is not None and existing != current_commit:
        raise ReleaseNotesError(
            f"current release tag {current_tag} points to {existing}, not {current_commit}"
        )


def previous_manager_tag(
    repository: Path, current_commit: str, version_text: str
) -> str | None:
    current_version = parse_version(version_text)
    current_tag = f"v{version_text}"
    require_current_tag_identity(repository, current_tag, current_commit)
    candidates: list[tuple[str, Version]] = []
    for tag in merged_tags(repository, current_commit):
        if not tag.startswith("v"):
            continue
        try:
            version = parse_version(tag[1:])
        except ReleaseNotesError:
            continue
        if compare_versions(version, current_version) < 0:
            candidates.append((tag, version))
    if not candidates:
        return None
    candidates.sort(
        key=cmp_to_key(lambda left, right: compare_versions(left[1], right[1])),
        reverse=True,
    )
    best = candidates[0]
    if len(candidates) > 1 and compare_versions(best[1], candidates[1][1]) == 0:
        raise ReleaseNotesError("multiple Manager tags have the same previous SemVer precedence")
    return best[0]


def parse_compat_id(compat_id: str) -> tuple[str, str, int]:
    match = COMPAT_ID.fullmatch(compat_id)
    if match is None:
        raise ReleaseNotesError(f"invalid compatibility ID: {compat_id!r}")
    return match.group("version"), match.group("family"), int(match.group("revision"))


def previous_compat_tag(
    repository: Path, current_commit: str, compat_id: str
) -> str | None:
    version, family, current_revision = parse_compat_id(compat_id)
    current_tag = f"compat-{compat_id}"
    require_current_tag_identity(repository, current_tag, current_commit)
    prefix = f"compat-rust-v{version}-{family}-p"
    candidates: list[tuple[int, str]] = []
    for tag in merged_tags(repository, current_commit):
        if not tag.startswith(prefix):
            continue
        suffix = tag[len(prefix) :]
        if suffix.isdigit() and not suffix.startswith("0"):
            revision = int(suffix)
            if 0 < revision < current_revision:
                candidates.append((revision, tag))
    if not candidates:
        return None
    candidates.sort(reverse=True)
    if len(candidates) > 1 and candidates[0][0] == candidates[1][0]:
        raise ReleaseNotesError("multiple compatibility tags claim the same previous revision")
    return candidates[0][1]


def validate_ancestor(repository: Path, previous_tag: str, current_commit: str) -> None:
    previous_commit = tag_commit(repository, previous_tag)
    if previous_commit is None:
        raise ReleaseNotesError(f"previous tag disappeared from the local checkout: {previous_tag}")
    result = run_git(
        repository,
        "merge-base",
        "--is-ancestor",
        previous_commit,
        current_commit,
        check=False,
    )
    if result.returncode != 0:
        raise ReleaseNotesError(f"previous tag {previous_tag} is not an ancestor of current ref")


def commit_paths(repository: Path, sha: str) -> tuple[str, ...]:
    output = run_git(
        repository,
        "diff-tree",
        "--root",
        "--no-commit-id",
        "--name-only",
        "-r",
        "-m",
        "-z",
        sha,
    ).stdout
    return tuple(sorted(set(path for path in output.split("\0") if path)))


def read_commits(
    repository: Path, previous_tag: str | None, current_commit: str
) -> list[Commit]:
    revision = f"{previous_tag}..{current_commit}" if previous_tag else current_commit
    hashes = run_git(repository, "rev-list", "--reverse", "--no-merges", revision).stdout.splitlines()
    commits: list[Commit] = []
    for sha in hashes:
        if SHA1.fullmatch(sha) is None:
            raise ReleaseNotesError("Git returned an invalid commit in the release range")
        fields = run_git(
            repository,
            "show",
            "-s",
            "--format=%H%x00%h%x00%s%x00%b",
            "--no-patch",
            sha,
        ).stdout.split("\0", 3)
        if len(fields) != 4 or fields[0] != sha:
            raise ReleaseNotesError(f"cannot parse Git metadata for commit {sha}")
        subject = " ".join(fields[2].strip().split())
        if not subject:
            raise ReleaseNotesError(f"commit {sha} has an empty subject")
        commits.append(
            Commit(sha, fields[1], subject, fields[3], commit_paths(repository, sha))
        )
    return commits


def load_compatibility_paths(
    repository: Path, current_commit: str, compat_id: str, target: str
) -> tuple[set[str], tuple[str, ...]]:
    try:
        index = json.loads(
            run_git(
                repository,
                "show",
                f"{current_commit}:release/compatibility-index.json",
            ).stdout
        )
        entry = index["compatibilities"][compat_id]
        manifest = PurePosixPath(entry["manifest"])
        route = entry["targets"][target]
        if not isinstance(route, dict):
            raise TypeError("target route must be an object")
    except (OSError, UnicodeError, json.JSONDecodeError, KeyError, TypeError) as error:
        raise ReleaseNotesError(
            f"cannot resolve compatibility paths for {compat_id}/{target}: {error}"
        ) from error
    if (
        manifest.is_absolute()
        or len(manifest.parts) < 4
        or manifest.parts[:2] != ("payload", "codex")
        or any(part in {"", ".", ".."} for part in manifest.parts)
    ):
        raise ReleaseNotesError("compatibility manifest path is unsafe")
    payload_prefix = PurePosixPath(*manifest.parts[:3]).as_posix() + "/"
    exact = set(COMPAT_FILES)
    for key in ("runtime_lock", "acceptance"):
        value = route.get(key)
        if value is not None:
            if not isinstance(value, str) or not value:
                raise ReleaseNotesError(f"compatibility {key} path must be a non-empty string")
            pure = PurePosixPath(value)
            if pure.is_absolute() or any(part in {"", ".", ".."} for part in pure.parts):
                raise ReleaseNotesError(f"compatibility {key} path is unsafe")
            exact.add(pure.as_posix())
    return exact, (*COMPAT_PREFIXES, payload_prefix)


def relevant_commits(
    repository: Path,
    commits: list[Commit],
    stream: str,
    current_commit: str,
    compat_id: str | None,
    target: str | None,
) -> list[Commit]:
    if stream == "manager":
        exact, prefixes = MANAGER_FILES, MANAGER_PREFIXES
    else:
        assert compat_id is not None and target is not None
        exact, prefixes = load_compatibility_paths(
            repository, current_commit, compat_id, target
        )
    return [
        commit
        for commit in commits
        if any(path in exact or path.startswith(prefixes) for path in commit.paths)
    ]


def parse_commit(commit: Commit) -> ParsedCommit | None:
    match = CONVENTIONAL.fullmatch(commit.subject)
    if match is None:
        return None
    return ParsedCommit(
        match.group("type"),
        match.group("scope"),
        match.group("title"),
        match.group("breaking") is not None,
    )


def skipped(commit: Commit, parsed: ParsedCommit | None) -> bool:
    if any(SKIP_TRAILER.fullmatch(line.strip()) for line in commit.body.splitlines()):
        return True
    return parsed is not None and parsed.kind in {"test", "chore"}


def markdown(value: str) -> str:
    escaped = value.replace("\\", "\\\\")
    for character in "`*_[]<>|~":
        escaped = escaped.replace(character, f"\\{character}")
    if escaped.startswith(("#", ">", "-", "+")):
        escaped = "\\" + escaped
    escaped = re.sub(r"^(\d+)([.)])(?=\s)", r"\1\\\2", escaped)
    return escaped


def display_title(parsed: ParsedCommit) -> str:
    title = parsed.title.strip()
    title = title[:1].upper() + title[1:]
    if parsed.breaking:
        title = f"Breaking: {title}"
    if title[-1:] not in ".!?":
        title += "."
    return markdown(title)


def section_for(stream: str, parsed: ParsedCommit) -> str | None:
    if stream == "manager" and parsed.scope == "cli":
        return "CLI"
    if parsed.kind == "feat":
        return "New Features" if stream == "manager" else "Patch Changes"
    return {
        "fix": "Bug Fixes",
        "perf": "Improvements",
        "refactor": "Improvements",
        "docs": "Documentation",
        "ci": "Build & Release",
        "build": "Build & Release",
    }.get(parsed.kind)


def render_dynamic(stream: str, commits: list[Commit]) -> tuple[list[str], int]:
    order = [
        "New Features" if stream == "manager" else "Patch Changes",
        "Bug Fixes",
        "CLI",
        "Improvements",
        "Build & Release",
        "Documentation",
    ]
    sections: dict[str, list[str]] = {name: [] for name in order}
    seen: dict[str, set[str]] = {name: set() for name in order}
    visible = 0
    for commit in commits:
        parsed = parse_commit(commit)
        if skipped(commit, parsed) or parsed is None:
            continue
        section = section_for(stream, parsed)
        if section is None:
            continue
        title = display_title(parsed)
        key = " ".join(title.casefold().split())
        if key not in seen[section]:
            seen[section].add(key)
            sections[section].append(title)
            visible += 1
    lines: list[str] = []
    for section in order:
        if not sections[section]:
            continue
        lines.extend([f"## {section}", "", *[f"- {title}" for title in sections[section]], ""])
    if not lines:
        lines.extend(["## Chores", "", "No user-facing changes in this release.", ""])
    return lines, visible


def render_changelog(
    stream: str,
    commits: list[Commit],
    previous_tag: str | None,
    current_tag: str,
) -> list[str]:
    title = "Changelog" if stream == "manager" else "Full Changelog"
    lines = [f"## {title}", ""]
    if previous_tag:
        comparison = f"{previous_tag}...{current_tag}"
        lines.extend(
            [f"Full Changelog: [{comparison}]({REPOSITORY_URL}/compare/{comparison})", ""]
        )
    else:
        lines.extend([f"Initial release history through `{current_tag}`.", ""])
    for commit in commits:
        parsed = parse_commit(commit)
        if skipped(commit, parsed):
            continue
        lines.append(f"- `{commit.short}` {markdown(commit.subject)}")
    lines.append("")
    return lines


def compat_information(
    compat_id: str,
    codex_version: str,
    upstream_tag: str,
    upstream_commit: str,
    target: str,
    artifact_sha256: str,
) -> list[str]:
    revision = parse_compat_id(compat_id)[2]
    return [
        "## Compatibility",
        "",
        f"- Compatibility ID: `{compat_id}`",
        f"- Upstream Codex: `{upstream_tag}` (`{codex_version}`)",
        f"- CSA patch revision: `p{revision}`",
        f"- Target: `{target}`",
        "",
        "## Verification",
        "",
        f"- Upstream commit: `{upstream_commit}`",
        f"- Production executable SHA-256: `{artifact_sha256}`",
        "- Built independently from the reviewed upstream source by GitHub Actions.",
        "- Exact compatibility payload, provenance descriptor, and checksums are included.",
        "",
    ]


def write_atomic(path: Path, text: str) -> None:
    path = path.resolve()
    parent = path.parent.resolve(strict=True)
    if path.exists() and not path.is_file():
        raise ReleaseNotesError(f"output is not a regular file: {path}")
    fd, temporary_name = tempfile.mkstemp(prefix=f".{path.name}.", dir=parent)
    temporary = Path(temporary_name)
    try:
        with os.fdopen(fd, "w", encoding="utf-8", newline="\n") as stream:
            stream.write(text)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def generate(
    repository: Path,
    stream: str,
    current_ref: str,
    output: Path,
    *,
    version: str | None = None,
    compat_id: str | None = None,
    codex_version: str | None = None,
    upstream_tag: str | None = None,
    upstream_commit: str | None = None,
    target: str | None = None,
    artifact_sha256: str | None = None,
) -> dict[str, object]:
    repository = repository.resolve(strict=True)
    top = run_git(repository, "rev-parse", "--show-toplevel").stdout.strip()
    if Path(top).resolve() != repository:
        raise ReleaseNotesError("--repository must be the Git worktree root")
    current_commit = resolve_commit(repository, current_ref)

    if stream == "manager":
        if version is None or any(
            value is not None
            for value in (compat_id, codex_version, upstream_tag, upstream_commit, target, artifact_sha256)
        ):
            raise ReleaseNotesError("Manager generation requires only --version")
        previous_tag = previous_manager_tag(repository, current_commit, version)
        current_tag = f"v{version}"
        fixed: list[str] = []
    elif stream == "compat":
        if version is not None or any(
            value is None
            for value in (compat_id, codex_version, upstream_tag, upstream_commit, target, artifact_sha256)
        ):
            raise ReleaseNotesError("compatibility generation requires every compatibility argument")
        assert compat_id and codex_version and upstream_tag and upstream_commit and target and artifact_sha256
        compat_version, _, _ = parse_compat_id(compat_id)
        parsed_codex = parse_version(codex_version)
        if parsed_codex.prerelease is not None or compat_version != codex_version:
            raise ReleaseNotesError("compatibility ID and stable Codex version differ")
        if upstream_tag != f"rust-v{codex_version}":
            raise ReleaseNotesError("upstream tag must equal rust-v<codex-version>")
        if SHA1.fullmatch(upstream_commit) is None:
            raise ReleaseNotesError("upstream commit must be lowercase 40-hex")
        if SAFE_VALUE.fullmatch(target) is None:
            raise ReleaseNotesError("target must be a safe non-empty identifier")
        if SHA256.fullmatch(artifact_sha256) is None:
            raise ReleaseNotesError("artifact SHA-256 must be lowercase 64-hex")
        previous_tag = previous_compat_tag(repository, current_commit, compat_id)
        current_tag = f"compat-{compat_id}"
        fixed = compat_information(
            compat_id,
            codex_version,
            upstream_tag,
            upstream_commit,
            target,
            artifact_sha256,
        )
    else:
        raise ReleaseNotesError(f"unsupported release stream: {stream!r}")

    if previous_tag:
        validate_ancestor(repository, previous_tag, current_commit)
    commits = relevant_commits(
        repository,
        read_commits(repository, previous_tag, current_commit),
        stream,
        current_commit,
        compat_id,
        target,
    )
    dynamic, visible = render_dynamic(stream, commits)
    changelog = render_changelog(stream, commits, previous_tag, current_tag)
    text = "\n".join([*dynamic, *changelog, *fixed]).rstrip() + "\n"
    if "No user-facing changes in this release." not in text and visible == 0:
        raise ReleaseNotesError("release notes contain no categorized changes or explicit empty state")
    if stream == "compat" and text.index("## Compatibility") <= text.index("## Full Changelog"):
        raise ReleaseNotesError("compatibility facts must follow dynamic history")
    if previous_tag:
        comparison = f"{previous_tag}...{current_tag}"
        if f"[{comparison}]({REPOSITORY_URL}/compare/{comparison})" not in text:
            raise ReleaseNotesError("release notes lost the selected comparison link")
    if not previous_tag and f"`{current_tag}`" not in text:
        raise ReleaseNotesError("first-release notes lost the current release tag")
    write_atomic(output, text)
    return {
        "schema": 1,
        "status": "written",
        "stream": stream,
        "current_commit": current_commit,
        "current_tag": current_tag,
        "previous_tag": previous_tag,
        "relevant_commits": len(commits),
        "visible_entries": visible,
        "output": str(output.resolve()),
    }


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument("--repository", type=Path, default=Path("."))
    result.add_argument("--stream", choices=("manager", "compat"), required=True)
    result.add_argument("--current-ref", required=True)
    result.add_argument("--output", type=Path, required=True)
    result.add_argument("--version")
    result.add_argument("--compat-id")
    result.add_argument("--codex-version")
    result.add_argument("--upstream-tag")
    result.add_argument("--upstream-commit")
    result.add_argument("--target")
    result.add_argument("--artifact-sha256")
    return result


def main() -> int:
    args = parser().parse_args()
    try:
        result = generate(
            args.repository,
            args.stream,
            args.current_ref,
            args.output,
            version=args.version,
            compat_id=args.compat_id,
            codex_version=args.codex_version,
            upstream_tag=args.upstream_tag,
            upstream_commit=args.upstream_commit,
            target=args.target,
            artifact_sha256=args.artifact_sha256,
        )
    except (ReleaseNotesError, OSError, UnicodeError, json.JSONDecodeError) as error:
        print(f"ERROR: {error}", file=sys.stderr)
        return 2
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
