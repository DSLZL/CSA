<div align="center">

# CSA

A fail-closed manager for running a version-pinned, patched Codex CLI beside the official installation.

[![CI](https://github.com/DSLZL/CSA/actions/workflows/ci.yml/badge.svg)](https://github.com/DSLZL/CSA/actions/workflows/ci.yml)
[![CSA release](https://img.shields.io/github/v/release/DSLZL/CSA?filter=v%2A&label=CSA)](https://github.com/DSLZL/CSA/releases)
[![Patched Codex](https://img.shields.io/badge/patched%20Codex-0.149.0%20accepted-white)](https://github.com/DSLZL/CSA/releases/tag/compat-rust-v0.149.0-native-join-p3)

[Getting started](#getting-started) · [How it works](#how-it-works) · [Compatibility](#compatibility) · [Commands](#commands) · [Development](#development) · [简体中文](README_ZH.md)

</div>

CSA adds native subagent joins and a live subagent view to Codex without replacing the official CLI. The manager discovers the installed Codex package, verifies its runtime files, and places the patched executable in a separate managed directory.

> [!IMPORTANT]
> CSA Manager `0.1.2` is distributed through `@dslzl/csa` and the `v0.1.2` GitHub Release. Patched Codex `0.149.0` p3 is the current accepted Windows x64 release; the `0.149.1` p6 and p7 payloads are build-only candidates.

## What the patch changes

- `join_agent` waits for one exact child run to finish in a single tool call.
- `join_agents` waits for several exact runs and returns results in request order.
- The parent no longer needs to poll child status while a native Join is pending.
- The TUI can show live child activity, completed work, and navigation back to a child session.
- The patched executable uses the official Codex runtime package and companion tools instead of carrying a second copy.

The `0.149.1` p7 candidate also contains the latest subagent panel and lossless terminal Orbit work. It is not a formal compatibility release yet.

## How it works

```text
Official Codex installation (read-only)
                  │
                  │ discover and fingerprint
                  ▼
             CSA Manager
                  │ prepare + plug
                  ▼
       <manager-root>/bin/codex
             ├─ valid binding ──> patched codex.exe + official runtime
             └─ invalid binding ─> official Codex launcher
```

CSA keeps four things separate:

| Component | Owned by | Purpose |
| --- | --- | --- |
| Official Codex | OpenAI package manager installation | Configuration, authentication, runtime helpers, and safe fallback |
| CSA Manager | CSA | Verification, preparation, activation, status, and removal |
| Patched Codex | CSA manager directory | Version-pinned Native Join and TUI changes |
| `codex` shim | CSA manager directory | Chooses the verified patched binary or falls back to official Codex |

The manager does not overwrite official files, copy the user's Codex home, or edit `PATH`. Normal shim launches inherit the same `CODEX_HOME`, configuration, authentication, working directory, and terminal as official Codex.

## Compatibility

| Compatibility | Codex | Target | State |
| --- | --- | --- | --- |
| [`rust-v0.149.0-native-join-p3`](https://github.com/DSLZL/CSA/releases/tag/compat-rust-v0.149.0-native-join-p3) | `0.149.0` | Windows x64 | Accepted and published |
| `rust-v0.149.1-native-join-p6` | `0.149.1` | Windows x64 | Candidate, release disabled |
| `rust-v0.149.1-native-join-p7` | `0.149.1` | Windows x64 | Candidate, release disabled |
| `rust-v0.149.1-native-join-p8` | `0.149.1` | Windows x64 | Candidate, release disabled |

The [compatibility index](release/compatibility-index.json) is authoritative for repository payloads. Normal online installation discovers every formal `compat-*` GitHub Release; build-only candidates are not listed.

Online installation is deliberately strict. CSA lists every formal patched release, but enables only entries whose target and Codex version exactly match this Manager and the installed read-only official runtime. Every selected Release is then revalidated against its tag, upstream commit, manifest, size, and SHA-256.

## Prerequisites

- An official Codex CLI installation that CSA can discover.
- Windows x64 for the currently accepted patched Codex target.
- Rust `1.95.0` when building the current Manager or a patched payload from source.
- Node.js `18` or newer when installing the Manager from npm.

Public GitHub API requests work without authentication. If that rate limit is exhausted, set `GITHUB_TOKEN` or `GH_TOKEN` for the current `csa install` process; CSA sends it only to `api.github.com` and never stores it.

## Getting started

### Get the Manager

Install the standard CLI from npm:

```powershell
npm install --global @dslzl/csa
csa --version
```

Prebuilt Manager archives and `SHA256SUMS` are also available on the [Releases page](https://github.com/DSLZL/CSA/releases). Download the archive for your platform, verify it, and extract the `csa` executable.

To build the current source instead:

```powershell
git clone https://github.com/DSLZL/CSA.git
Set-Location CSA
cargo build --release --locked

$Manager = (Resolve-Path '.\target\release\csa.exe').Path
& $Manager --version
```

> [!NOTE]
> The npm package exposes only `csa`. It does not replace `codex` or activate a patched build during package installation; run `csa install` explicitly.

### Verify and install

Use an explicit manager root while testing so its files are easy to inspect and remove:

```powershell
$ManagerRoot = Join-Path $env:LOCALAPPDATA 'CSA\managed'

csa doctor --manager-root $ManagerRoot
csa install --manager-root $ManagerRoot
csa status --manager-root $ManagerRoot
```

In an interactive terminal, bare `csa install` fetches the formal compatibility catalog, lists patched Codex versions and their installability, and asks for a number. Automation must select the exact ID explicitly:

```powershell
csa install --compat rust-v0.149.0-native-join-p3
```

> [!WARNING]
> A Release is selectable only when its target and Codex version match the current Manager and installed official runtime. CSA never downloads or overwrites a different official Codex version. Local payload mode is for development and acceptance work, not a downgrade bypass.

For local payload development, pass a manifest and exactly one local artifact or source directory:

```powershell
$CompatId = 'rust-v0.149.0-native-join-p3'
$Manifest = Join-Path 'C:\absolute\payload' "$CompatId\manifest.toml"
$Artifact = 'C:\absolute\patched\codex.exe'

& $Manager install --manager-root $ManagerRoot `
  --manifest $Manifest `
  --artifact $Artifact
```

The compatibility directory must keep the same name as its `compat_id`. Candidate manifests must be finalized in a disposable payload copy before local installation; the committed candidate files remain immutable inputs.

### Use the patched CLI

`install` creates the managed shim but does not edit `PATH`. Add it only to the current PowerShell process first:

```powershell
$ManagedBin = Join-Path $ManagerRoot 'bin'
$env:PATH = $ManagedBin + [IO.Path]::PathSeparator + $env:PATH

Get-Command codex -All
codex --version
codex
```

For automated or disposable testing, use `exec --isolated` instead of activating the shim:

```powershell
& $Manager exec --isolated `
  --manager-root $ManagerRoot `
  --codex-home C:\absolute\isolated\codex-home `
  --cwd C:\absolute\fixture `
  --logs-dir C:\absolute\logs `
  --state-dir C:\absolute\state `
  --record C:\absolute\evidence.json `
  --npm-prefix C:\absolute\npm-prefix `
  -- --version
```

Every isolated directory must be absolute, normalized, distinct, and outside the manager and official Codex trees.

### Uninstall

```powershell
& $Manager uninstall --manager-root $ManagerRoot

Get-Command codex -All
codex --version
```

`uninstall` removes the managed shim and Manager-owned preparation data. It is safe to run more than once. It does not remove the official Codex installation, npm packages, user configuration, or manually added `PATH` entries.

If you added the managed `bin` directory to a persistent user `PATH`, remove only that entry after confirming that `codex` resolves to the official launcher.

## Commands

| Command | What it does |
| --- | --- |
| `csa doctor` | Checks the official installation and optional compatibility inputs without changing state |
| `csa install` | Lists formal patched releases, installs the selected exact match, or accepts an exact local payload |
| `csa uninstall` | Withdraws the shim and removes Manager-owned preparation data |
| `csa prepare` | Validates or builds an exact local payload without activating it |
| `csa plug` | Publishes the checksum-bound shim inside `<manager-root>/bin` |
| `csa unplug` | Withdraws the shim without removing prepared data |
| `csa status` | Reports prepared state, activation health, and drift |
| `csa purge` | Removes the shim and all Manager-owned prepared, source, build, and state data |
| `csa exec --isolated` | Runs the prepared Codex binary with explicit isolated directories and records evidence |

Run `csa --help` for the exact option list. Manager commands return machine-readable JSON; invalid input and verification failures return a structured error on stderr.

## Safety boundaries

- Official Codex paths are external and read-only.
- Manifests, source preimages, runtime files, artifacts, state, and shims are checksum-bound.
- Missing files, version drift, unsafe path overlap, and unverified assets fail closed.
- The shim revalidates its binding before launch and falls back to official Codex when the patched path is no longer trusted.
- Tests use disposable homes, working directories, state, logs, npm prefixes, and child-only `PATH` values.
- Authentication files, tokens, cookies, and full environment dumps do not belong in test evidence or release assets.

## Development

The Manager is a small Rust binary. Patched Codex payloads are data-driven and pinned to exact upstream tags, commits, source hashes, toolchains, targets, and test contracts.

Run the Manager quality gate:

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --all-targets
```

Validate compatibility and release tooling:

```powershell
py -3 validation\validate_replacements.py --repository .
py -3 scripts\test_compat_catalog.py
py -3 scripts\test_verify_release_asset_set.py
py -3 scripts\test_verify_patch_payload.py
py -3 scripts\test_release_tools.py
```

CSA has two independent release streams:

- `vX.Y.Z` releases contain the Manager and its platform archives.
- `compat-<compat_id>` releases contain one reviewed patched Codex compatibility.

CircleCI builds acceptance candidates. GitHub Actions performs an independent production build and owns formal publication. Neither pipeline treats the other's binary as release authority.

## Documentation

- [Operations and recovery](docs/operations.md)
- [Development and test isolation](docs/development.md)
- [Compatibility and release process](docs/release.md)
- [Current release readiness](release-readiness.md)
- [Compatibility catalog](release/compatibility-index.json)
- [Platform support matrix](release/support-matrix.json)

## Friends

- [LINUX DO](https://linux.do/)
