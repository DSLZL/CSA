<div align="center">

# CSA

A version-pinned, fail-closed manager for running a patched Codex CLI beside the official installation.

[![CI](https://github.com/DSLZL/CSA/actions/workflows/ci.yml/badge.svg)](https://github.com/DSLZL/CSA/actions/workflows/ci.yml)
[![CSA release](https://img.shields.io/github/v/release/DSLZL/CSA?filter=v%2A&label=CSA)](https://github.com/DSLZL/CSA/releases)
[![npm](https://img.shields.io/npm/v/%40dslzl%2Fcsa)](https://www.npmjs.com/package/@dslzl/csa)
[![Patched Codex](https://img.shields.io/badge/patched%20Codex-0.150.1%20accepted-white)](https://github.com/DSLZL/CSA/releases/tag/compat-rust-v0.150.1-native-join-p8)

[Quick start](#quick-start) · [Current support](#current-support) · [Commands](#command-reference) · [Documentation](#development-and-documentation) · [简体中文](README_ZH.md)

</div>

CSA adds native subagent joins and a live subagent view to Codex without replacing the official CLI. The Manager verifies the installed official runtime, downloads one exact patched compatibility from a formal GitHub Release, and keeps every managed file in a separate directory.

> [!IMPORTANT]
> CSA Manager `0.1.3` is published as `@dslzl/csa` and as the `v0.1.3` GitHub Release. The current formal patched compatibility is Codex `0.150.1` p8 for Windows x64.

## Why CSA

CSA keeps the patch useful without turning the official installation into a mutable build target:

- `join_agent` waits for one exact child run to finish in one tool call.
- `join_agents` waits for several exact runs and returns their outcomes in request order.
- The TUI shows live child activity, completed work, and navigation back to a child session.
- The patched executable reuses the verified official Codex runtime and companion tools.
- A checksum-bound shim falls back to official Codex if the prepared binding is no longer valid.

The Manager does not overwrite official Codex files, copy the default `CODEX_HOME`, or edit shell profiles. On Windows, an explicit `csa install` changes only the current user's `PATH`; installing the npm package does not.

## Current support

| Product | Current release | Platforms |
| --- | --- | --- |
| CSA Manager | `0.1.3` | Windows x64, Linux x64, Linux arm64 glibc, macOS x64, macOS arm64 |
| Patched Codex CLI | [`rust-v0.150.1-native-join-p8`](https://github.com/DSLZL/CSA/releases/tag/compat-rust-v0.150.1-native-join-p8) | Windows x64 |

Manager availability on a platform does not imply that a patched Codex compatibility exists for that platform. The [compatibility index](release/compatibility-index.json) is authoritative for repository payloads, while normal installation discovers published `compat-*` Releases.

Online installation is exact-match only. A release is selectable only when its target and Codex version match the Manager and the installed official runtime. Tags, commits, manifests, file sizes, and SHA-256 values are checked before activation.

## Quick start

### Install the Manager

You need Node.js 18 or newer and a working official Codex CLI installation.

```powershell
npm install --global @dslzl/csa@0.1.3
csa --version
```

You can also run the CLI without a global install:

```powershell
npx @dslzl/csa@0.1.3 --version
```

Adding `--yes` to `npx` only suppresses npm's package-install confirmation. It does not choose a patched Codex release for you:

```powershell
npx --yes @dslzl/csa@0.1.3 --version
```

Prebuilt Manager archives and `SHA256SUMS` are available from the [`v0.1.3` Release](https://github.com/DSLZL/CSA/releases/tag/v0.1.3).

> [!NOTE]
> Installing the npm package exposes only `csa`. It does not replace `codex`, download a patched build, or activate a shim during package installation.

### Check and install patched Codex

Run these commands in an interactive terminal:

```powershell
csa doctor
csa install
csa status
```

`csa install` lists public `compat-*` Releases with complete fixed metadata in version order. It marks incompatible entries as unavailable and asks you to select an installable version. No GitHub login is required: five-second Cloudflare and Alibaba/Taobao country probes run in parallel, and either one reporting mainland China (`CN`) starts with `gh-proxy.com`; otherwise CSA starts with GitHub directly and keeps the direct-to-mirror fallback.

Automation must provide the exact compatibility ID:

```powershell
csa install --compat rust-v0.150.1-native-join-p8
```

> [!WARNING]
> CSA does not download, downgrade, or overwrite the official Codex installation to make a compatibility fit. If no entry matches, install the required official Codex version yourself or wait for a matching CSA compatibility.

### Use the managed shim

On Windows, `install` creates a verified shim, moves its managed directory to the front of the current user's persistent `PATH`, and silently verifies it with the system `where.exe`. A running VS Code window keeps its old environment, so fully quit and reopen it. To use the shim immediately in the current PowerShell process:

```powershell
$Status = csa status | ConvertFrom-Json
$ManagedBin = [string]$Status.activation.managed_bin
$OtherEntries = @($env:PATH -split ';' | Where-Object { $_ -and $_ -ine $ManagedBin })
$env:PATH = (@($ManagedBin) + $OtherEntries) -join ';'

Get-Command codex -All
codex --version
codex
```

Keep the official Codex launcher on `PATH` after the managed directory. If the binding becomes invalid, the shim uses that official launcher instead of an unverified patched executable.

### Remove CSA-managed state

```powershell
csa uninstall

Get-Command codex -All
codex --version
npm uninstall --global @dslzl/csa
```

`uninstall` withdraws the shim, removes Manager-owned preparation data, and removes CSA's exact managed user-`PATH` entry. It leaves official Codex, user configuration, authentication, npm state, and all other `PATH` entries alone.

## Native Join and TUI

The current p8 patch includes:

- exact single-run and batch Native Join tools;
- replayable terminal outcomes and ordered batch results;
- child transport fallback inheritance;
- a live subagent panel for starting, running, waiting, approval, completed, failed, and cancelled work;
- text, Sixel, and Kitty Orbit rendering with reduced-motion behavior.

The formal Windows x64 acceptance record covers the exact executable hash, official runtime binding, official-file immutability, and an authenticated single-child Native Join. Multi-child Native Join, Ultra runtime behavior, and interactive TUI acceptance remain explicitly unverified.

For quick visual work without compiling Codex, use the standalone [Ratatui UI harness](tests/ui/README.md).

## How CSA works

```text
Official Codex installation, read-only
                 |
                 | detect and fingerprint
                 v
            CSA Manager
                 |
                 | prepare and plug
                 v
      <manager-root>/bin/codex
          | valid binding   -> patched codex.exe + official runtime
          | invalid binding -> official Codex launcher
```

CSA separates four identities:

| Component | Owner | Role |
| --- | --- | --- |
| Official Codex | Existing package-manager installation | Configuration, authentication, runtime files, and fallback |
| CSA Manager | CSA | Discovery, verification, installation, activation, status, and removal |
| Patched Codex | Manager-owned directory | Version-pinned Native Join and TUI changes |
| `codex` shim | Manager-owned `bin` directory | Revalidates the binding and selects patched or official Codex |

Normal shim launches inherit the current `CODEX_HOME`, working directory, terminal, and environment just like an official launch. Tests that need isolation must use explicit disposable directories instead.

## Command reference

| Command | Purpose |
| --- | --- |
| `csa doctor` | Check the official installation and optional compatibility inputs without changing state |
| `csa install` | List formal Releases and install one exact match, or install an exact local payload |
| `csa uninstall` | Withdraw the shim and remove Manager-owned prepared state |
| `csa prepare` | Validate or build an exact local payload without activating it |
| `csa plug` | Publish the checksum-bound shim inside `<manager-root>/bin` |
| `csa unplug` | Withdraw the shim while keeping prepared data |
| `csa status` | Report prepared state, activation health, paths, and drift |
| `csa purge` | Remove all Manager-owned prepared, source, build, shim, and state data |
| `csa exec --isolated` | Run the prepared binary with explicit isolated directories and record evidence |

Run `csa --help` for the complete option list. Manager commands write machine-readable JSON to stdout. Invalid input and verification failures write a structured error to stderr and exit with code 2.

## Development and documentation

The Manager requires Rust `1.89` or newer. The current release and patched-Codex build profiles are pinned to Rust `1.95.0`.

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --all-targets
py -3 scripts\test_validation_evidence.py
py -3 scripts\test_release_tools.py
```

- [Operations, recovery, and troubleshooting](docs/operations.md)
- [Development and test isolation](docs/development.md)
- [Compatibility and release process](docs/release.md)
- [Compatibility catalog](release/compatibility-index.json)
- [Manager platform support matrix](release/support-matrix.json)

CSA uses two independent release streams: `vX.Y.Z` for the Manager and `compat-<compat_id>` for one reviewed patched Codex compatibility.

## Friends

- [LINUX DO](https://linux.do/)
