<div align="center">

# CSA

Install and switch between version-pinned patched Codex CLI builds without replacing the official installation.

[![CI](https://github.com/DSLZL/CSA/actions/workflows/ci.yml/badge.svg)](https://github.com/DSLZL/CSA/actions/workflows/ci.yml)
[![CSA release](https://img.shields.io/github/v/release/DSLZL/CSA?filter=v%2A&label=CSA)](https://github.com/DSLZL/CSA/releases)
[![npm](https://img.shields.io/npm/v/%40dslzl%2Fcsa)](https://www.npmjs.com/package/@dslzl/csa)
[![Patched Codex](https://img.shields.io/badge/patched%20Codex-0.153.2%20p15-white)](https://github.com/DSLZL/CSA-codex/releases/tag/compat-rust-v0.153.2-native-join-p15)

[Quick start](#quick-start) · [How it works](#how-it-works) · [Commands](#commands) · [Documentation](#documentation) · [简体中文](README_ZH.md)

</div>

CSA is a Rust manager for patched Codex CLI builds. It detects the installed official Codex runtime, lists compatible formal Releases, downloads the selected executable, verifies its identity and checksum, then activates it through a managed `codex` shim.

The official Codex package, configuration, authentication, sessions, and local databases stay in place.

> [!IMPORTANT]
> The current Manager is `0.1.9`. The current formal patched Release is Codex `0.153.2` p15. Six native patched artifacts are published. Linux runtime discovery validates the managed package, platform package, native binary, and required helpers before activation.

If official Codex reports a migration-checksum error after using p10, follow the
[database recovery procedure](https://github.com/DSLZL/CSA-codex/blob/main/docs/sqlite-migration-recovery.md).
p15 preserves native migration checksums; upgrading alone does not repair previously affected databases.

## What the patch adds

- `join_agent` waits for one exact child run in a single tool call.
- `join_agents` waits for a fixed set of exact runs and returns results in request order.
- The TUI keeps live and completed subagent activity visible without merging new work into an old panel.
- Text, Sixel, and Kitty Orbit renderers support animated and reduced-motion modes.
- Native database migration checksums are preserved when returning to the matching official Codex version.

The patch changes Codex behavior. CSA itself handles installation, verification, activation, fallback, and removal.

## Requirements and support

The npm distribution requires Node.js 18 or newer and a working official Codex CLI installation.

| Product | Current release | Published platforms |
| --- | --- | --- |
| CSA Manager | `0.1.9` | Windows x64, Linux x64, Linux arm64, macOS x64, macOS arm64 |
| Patched Codex CLI | [`rust-v0.153.2-native-join-p15`](https://github.com/DSLZL/CSA-codex/releases/tag/compat-rust-v0.153.2-native-join-p15) | Windows x64/arm64, Linux x64/arm64 musl, macOS x64/arm64 |

Manager support does not guarantee that a patched Codex artifact exists for the same platform. Online installation requires an exact official Codex version and resolves Linux Manager targets to the published musl artifacts.

## Quick start

### 1. Install the Manager

```powershell
npm install --global @dslzl/csa@0.1.9
csa --version
```

You can run CSA without a global install:

```powershell
npx @dslzl/csa@0.1.9 --version
bunx @dslzl/csa@0.1.9 --version
```

`npx --yes` only suppresses npm's package-install confirmation. It does not answer CSA's version picker. Use `csa install --yes` when CSA should select the recommended exact match without prompting.

> [!NOTE]
> Installing `@dslzl/csa` exposes the `csa` command only. Package installation does not download patched Codex, edit `PATH`, create a `codex` shim, or modify the official package.

### Linux and other POSIX shells

Use the same npm installation from Bash, zsh, sh, or fish:

```sh
npm install --global @dslzl/csa@0.1.9
csa doctor
csa install --yes
```

POSIX activation writes a CSA-owned marker block to `.profile`, `.bashrc`, `.zshrc`, and fish `conf.d` when those files are safe to update. It never edits the official Codex package. The install report is `prepared_but_inactive` when those files cannot be updated; activate the current shell explicitly:

```sh
eval "$(csa shell env bash)"
command -v codex
codex --version
```

`csa shell env <shell>` only prints a command; it does not modify the current shell. Evaluate it with the syntax for the active shell:

```sh
# sh
eval "$(csa shell env sh)"
# bash
eval "$(csa shell env bash)"
# zsh
eval "$(csa shell env zsh)"
```

```fish
# fish
eval (csa shell env fish)
```

`csa shell init <shell>` prints the profile fragment that sources the manager-owned activation file. Check aliases and functions with `type -a codex`; they are separate from `PATH` and must be resolved by the shell.

### 2. Diagnose and install

```powershell
csa doctor
csa doctor --remote
csa doctor --remote --refresh
csa install
csa status
```

`doctor --remote` checks the public compatibility catalog for the installed Codex version and artifact target. A compatible Release is reported as PASS; no match, an unavailable check, or an ambiguous maximum revision is WARN. The catalog cache is stored in the OS per-user cache directory for one hour, outside `--manager-root`; expired or invalid cache data is ignored. `--refresh` skips the cache and requires `--remote`. The check does not download a patched executable or modify Manager installation state.

In an interactive terminal, `install` opens a five-row picker after filtering Releases to the current target and official Codex version. Use the arrow keys, paging keys, Home/End, or search, then press Enter. Escape or Ctrl+C cancels before the large executable download and exits with code 130.

`csa install --yes`, `--json`, and non-interactive streams select the unique matching entry with the greatest numeric `-pN` revision.

No GitHub login is required. CSA uses public Git refs, chooses direct GitHub or a fixed China mirror pool, samples the exact Release artifact from available mirrors, and downloads in measured order. Size and SHA-256 checks remain mandatory for every source.

### 3. Confirm which Codex will run

On Windows, `install` first puts the managed `bin` directory at the front of the user `PATH`. If a higher-priority machine entry still wins, CSA requests UAC, installs its own dispatcher under Program Files, and puts that protected directory first in the machine `PATH`. It never edits npm, Bun, pnpm, or Vite+ launchers.

Close every terminal window and fully quit terminal hosts such as VS Code after installation, then reopen one. Opening another integrated terminal inside the same VS Code window is not enough.

```powershell
csa status
where.exe codex
Get-Command codex -All
codex --version
```

`where.exe codex` should list a CSA-owned `codex.exe` first. An active patched installation prints `codex-cli X.Y.Z (CSA <compat-id>)` from `codex --version`. CSA requests administrator permission only when the machine `PATH` would otherwise take priority; denying the prompt fails activation and rolls it back.

On Linux and other POSIX shells, start a new shell or evaluate the shell-specific command above, then run `command -v codex`, `type -a codex`, and `codex --version`. The managed shim validates the official runtime package on every launch and falls back safely when a helper, marker, version, or executable fingerprint drifts.

### 4. Pin a compatible older revision

```powershell
csa install --compat rust-v0.150.1-native-join-p8
```

The requested Release must still match the installed official Codex version and current target. CSA never downgrades or overwrites official Codex to make a compatibility fit.

### 5. Remove CSA-managed state

```powershell
csa uninstall
where.exe codex
Get-Command codex -All
codex --version
npm uninstall --global @dslzl/csa
```

`uninstall` removes the managed shim, prepared installation, state, and CSA's exact user and elevated dispatcher `PATH` entries. Windows may request UAC to remove the Program Files dispatcher. Official Codex and user data remain untouched.

## How it works

```text
Official Codex installation, read-only
                 |
                 | detect and fingerprint
                 v
            CSA Manager
                 |
                 | verify, prepare, plug
                 v
      <manager-root>/bin/codex
          | valid binding   -> patched Codex + official runtime
          | invalid binding -> official Codex launcher
```

CSA separates four identities:

| Component | Role |
| --- | --- |
| Official Codex | Existing configuration, authentication, user state, runtime tools, and fallback |
| CSA Manager | Discovery, verification, installation, status, activation, and removal |
| Patched Codex | Version-pinned Native Join and TUI changes in a Manager-owned directory |
| Managed shim | Revalidates the binding before choosing patched or official Codex |

The optional Windows Program Files dispatcher is a protected copy of the managed shim, not another Codex installation.

Online installation downloads only `SHA256SUMS`, `compatibility-release.json`, and the current target's executable. The full patch, source contract, and formal compatibility Release are maintained by [`DSLZL/CSA-codex`](https://github.com/DSLZL/CSA-codex).

Normal shim launches reuse the current `CODEX_HOME`. Tests and acceptance runs should use `csa exec --isolated` with disposable directories.

Read [CSA architecture](docs/architecture.md) for the complete trust, download, runtime overlay, database, and release model.

## Commands

| Command | Purpose |
| --- | --- |
| `csa doctor` | Diagnose official Codex, prepared state, activation, and command precedence |
| `csa install` | Select, verify, prepare, and activate a formal Release or exact local payload |
| `csa uninstall` | Withdraw the shim and remove the prepared installation |
| `csa prepare` | Validate an exact local artifact or source payload without activation |
| `csa plug` | Activate the verified prepared state |
| `csa unplug` | Remove the shim while keeping prepared state |
| `csa status` | Report installation, activation, command resolution, and drift |
| `csa purge` | Remove all Manager-owned data |
| `csa shell init <shell>` | Print the profile fragment for `sh`, `bash`, `zsh`, or `fish` |
| `csa shell env <shell>` | Print a shell-specific command that prepends the CSA bin directory |
| `csa exec --isolated` | Run the prepared artifact with explicit isolated paths and evidence |

Human output follows the detected system language. A `zh` locale uses Simplified Chinese; other locales use English. Use `--json` before or after a command for the stable machine-readable report.

See the [CLI reference](docs/reference.md) for complete syntax, status values, exit codes, paths, platforms, and common errors.

## Documentation

- [Operations and troubleshooting](docs/operations.md)
- [CLI and platform reference](docs/reference.md)
- [Architecture and security model](docs/architecture.md)
- [Development and isolated testing](docs/development.md)
- [Manager release process](docs/release.md)
- [Patched Codex producer](https://github.com/DSLZL/CSA-codex)
- [Manager support matrix](release/support-matrix.json)

The lightweight [Ratatui UI harness](https://github.com/DSLZL/CSA-codex/tree/main/tests/ui) is maintained with the patched Codex producer.

## Friends

- [LINUX DO](https://linux.do/)
