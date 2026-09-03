#!/usr/bin/env python3
"""Generate deterministic CSA Manager release notes."""

from __future__ import annotations

import argparse
from dataclasses import dataclass
from functools import cmp_to_key
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile


SEMVER = re.compile(
    r"(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)"
    r"(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?"
    r"(?:\+([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?\Z"
)
CONVENTIONAL = re.compile(
    r"(?P<type>feat|fix|perf|refactor|docs|ci|build|test|chore)"
    r"(?:\((?P<scope>[A-Za-z0-9._/-]+)\))?(?P<breaking>!)?:\s+(?P<title>.+)\Z"
)
SAFE_REF = re.compile(r"[0-9A-Za-z][0-9A-Za-z._/@:+~^-]{0,199}\Z")
SHA1 = re.compile(r"[0-9a-f]{40}\Z")
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
    "release/install-catalog-bootstrap-v1.json",
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


def relevant_commits(commits: list[Commit]) -> list[Commit]:
    return [
        commit
        for commit in commits
        if any(path in MANAGER_FILES or path.startswith(MANAGER_PREFIXES) for path in commit.paths)
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


def section_for(parsed: ParsedCommit) -> str | None:
    if parsed.scope == "cli":
        return "CLI"
    if parsed.kind == "feat":
        return "New Features"
    return {
        "fix": "Bug Fixes",
        "perf": "Improvements",
        "refactor": "Improvements",
        "docs": "Documentation",
        "ci": "Build & Release",
        "build": "Build & Release",
    }.get(parsed.kind)


def render_dynamic(commits: list[Commit]) -> tuple[list[str], int]:
    order = [
        "New Features",
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
        section = section_for(parsed)
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
    commits: list[Commit], previous_tag: str | None, current_tag: str
) -> list[str]:
    lines = ["## Changelog", ""]
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
    version: str,
) -> dict[str, object]:
    repository = repository.resolve(strict=True)
    top = run_git(repository, "rev-parse", "--show-toplevel").stdout.strip()
    if Path(top).resolve() != repository:
        raise ReleaseNotesError("--repository must be the Git worktree root")
    current_commit = resolve_commit(repository, current_ref)

    if stream != "manager":
        raise ReleaseNotesError(f"unsupported release stream: {stream!r}")
    previous_tag = previous_manager_tag(repository, current_commit, version)
    current_tag = f"v{version}"

    if previous_tag:
        validate_ancestor(repository, previous_tag, current_commit)
    commits = relevant_commits(read_commits(repository, previous_tag, current_commit))
    dynamic, visible = render_dynamic(commits)
    changelog = render_changelog(commits, previous_tag, current_tag)
    text = "\n".join([*dynamic, *changelog]).rstrip() + "\n"
    if "No user-facing changes in this release." not in text and visible == 0:
        raise ReleaseNotesError("release notes contain no categorized changes or explicit empty state")
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
    result.add_argument("--stream", choices=("manager",), required=True)
    result.add_argument("--current-ref", required=True)
    result.add_argument("--output", type=Path, required=True)
    result.add_argument("--version", required=True)
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
        )
    except (ReleaseNotesError, OSError, UnicodeError, json.JSONDecodeError) as error:
        print(f"ERROR: {error}", file=sys.stderr)
        return 2
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
