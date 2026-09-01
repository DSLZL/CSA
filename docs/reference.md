# CSA reference

This reference describes the published CSA Manager CLI. It covers commands, output modes, status values, paths, platforms, and the files that define compatibility and release behavior.

## Command syntax

Global options may appear before or after a command. Options after the `--` separator belong to Codex and are not parsed by CSA.

```text
csa [--json] <command> [options]
```

| Command | Syntax |
| --- | --- |
| Help | `csa --help` |
| Version | `csa --version` |
| Diagnose | `csa doctor [--manager-root PATH] [--official PATH] [--official-native PATH] [--manifest PATH]` |
| Install online | `csa install [--yes] [--manager-root PATH] [--official PATH] [--official-native PATH] [--compat ID]` |
| Install locally | `csa install [--manager-root PATH] [--official PATH] [--official-native PATH] --manifest PATH (--artifact PATH \| --source PATH)` |
| Prepare locally | `csa prepare [--manager-root PATH] [--official PATH] [--official-native PATH] --manifest PATH (--artifact PATH \| --source PATH)` |
| Activate | `csa plug [--manager-root PATH]` |
| Deactivate | `csa unplug [--manager-root PATH]` |
| Inspect | `csa status [--manager-root PATH]` |
| Uninstall | `csa uninstall [--manager-root PATH]` |
| Remove all managed data | `csa purge [--manager-root PATH]` |
| Run in isolation | `csa exec --isolated [--manager-root PATH] --codex-home PATH --cwd PATH --logs-dir PATH --state-dir PATH --record PATH [--npm-prefix PATH] -- [CODEX_ARGS...]` |

### Install modes

Online mode is selected when `install` has no `--manifest`. It discovers formal compatibility Releases from `DSLZL/CSA` and accepts only an exact match for the installed official Codex version and the Manager build target.

Local mode requires `--manifest` and exactly one of `--artifact` or `--source`. It rejects `--yes` and `--compat`. Local mode is intended for development and acceptance work.

In a fully interactive terminal, bare `csa install` opens the version picker. `--yes`, `--json`, or any non-interactive standard stream selects the unique matching entry with the greatest numeric terminal `-pN` revision.

## Output modes

CSA has Human and JSON output modes.

| Condition | Output |
| --- | --- |
| Interactive terminal without `--json` | Human output |
| Explicit `--json` | JSON |
| Redirected stdout | JSON |

The Human interface uses Simplified Chinese when the detected primary locale is `zh`, including Simplified, Traditional, and regional variants. Every other locale uses English. Localization does not change command names, flags, paths, compatibility IDs, error codes, JSON fields, or JSON values.

Use either form:

```powershell
csa --json status
csa status --json
```

## Exit codes

| Command or condition | Exit code |
| --- | --- |
| `status` successfully renders any state | `0` |
| `doctor` reports only PASS and WARN | `0` |
| `doctor` completes and reports a FAIL | `1` |
| Invalid input or an incomplete diagnostic assessment | `2` |
| Version picker cancelled with Escape or Ctrl+C | `130` |
| `exec --isolated` child exits normally | Child exit code |

Other commands return `0` on success and a nonzero code with a structured error on failure.

## Status values

The top-level `status` value describes the prepared installation.

| Status | Meaning |
| --- | --- |
| `unprepared` | No prepared state is available |
| `prepared` | The patched artifact and its official runtime binding validate |
| `invalidated` | Prepared state exists, but a current integrity check failed |

Activation is reported separately.

| Activation | Meaning |
| --- | --- |
| `unplugged` | No managed shim is active |
| `plugged` | The shim and binding validate |
| `fallback` | Shim state exists but cannot safely start the patched artifact |

`activation.effective` is true only when the shim validates and the current process resolves `codex` to that shim. An invalidated or fallback state is diagnostic output, so `status` still exits `0`.

Official and patched builds of the same Codex release report the same `codex-cli X.Y.Z` version. Use `csa status`, command resolution, and absolute paths to identify the executable that will run.

## Common error codes

| Code | Meaning |
| --- | --- |
| `invalid_cli` | An option is unknown, duplicated, missing a value, or used in the wrong mode |
| `no_installable_compatibility_releases` | No formal Release matches the official Codex version and current target |
| `compatibility_not_found` | The requested compatibility ID is not published |
| `compatibility_not_installable` | The requested Release does not match the local version or target |
| `ambiguous_compatibility_revision` | More than one entry has the greatest numeric patch revision |
| `invalid_install_catalog` | Display catalog metadata is malformed or inconsistent |
| `invalid_compatibility_release` | Release identity, descriptor, inventory, size, or checksum validation failed |
| `artifact_hash_mismatch` | A supplied or cached artifact does not match the manifest |
| `install_rollback_failed` | Activation failed and CSA could not complete its rollback |
| `execution_integrity_failure` | An executable or runtime fingerprint changed during isolated execution |

Error messages include a stable code and technical detail. Human output may add localized impact and recovery guidance. JSON keeps the schema unchanged.

## Managed data

Without `--manager-root`, CSA uses the platform local-data directory. `doctor` and `status --json` report the resolved path.

```text
<manager-root>/
  bin/          active codex shim
  artifacts/    content-addressed patched runtime overlays
  downloads/    per-install staging, removed after success or failure
  sources/      exact source cache for local preparation
  builds/       local build cache
  manifests/    installed runtime or compatibility manifests
  state.json
  locks/
```

Stored official paths are validation references. CSA does not follow them during removal.

## Environment behavior

Normal shim launches inherit the current working directory, terminal, arguments, environment, and `CODEX_HOME`. CSA only adds the verified official package context needed by the patched overlay.

On Windows, explicit `install` and `plug` place the managed `bin` directory first in the current user's persistent `PATH`. If the next process's machine-plus-user ordering still selects another Codex, CSA requests UAC, installs `%ProgramFiles%\DSLZL\CSA\bin\codex.exe`, and places that protected dispatcher first in the machine `PATH`. It never edits package-manager launchers or shell profiles. Verification requires the CSA path to resolve first and one `codex --version` result in the form `codex-cli X.Y.Z (CSA <compat-id>)`. Existing applications keep their inherited environment until every terminal host is restarted.

`exec --isolated` requires separate absolute paths for `CODEX_HOME`, cwd, logs, state, and its evidence record. It does not create a shim or persist a `PATH` change.

## Platform support

CSA Manager `0.1.6` is published for:

| Platform | Rust target | npm package |
| --- | --- | --- |
| Windows x64 | `x86_64-pc-windows-msvc` | `@dslzl/csa-win32-x64` |
| Linux x64 glibc | `x86_64-unknown-linux-gnu` | `@dslzl/csa-linux-x64` |
| Linux arm64 glibc | `aarch64-unknown-linux-gnu` | `@dslzl/csa-linux-arm64` |
| macOS x64 | `x86_64-apple-darwin` | `@dslzl/csa-darwin-x64` |
| macOS arm64 | `aarch64-apple-darwin` | `@dslzl/csa-darwin-arm64` |

The formal `rust-v0.151.0-native-join-p10` patched Release contains Windows x64/arm64, Linux x64/arm64 musl, and macOS x64/arm64 Codex binaries. Manager support does not guarantee a patched artifact for the same target.

## Sources of truth

| File or release | Authority |
| --- | --- |
| `Cargo.toml` | Manager crate version and minimum Rust version |
| `npm/meta/package.json` | npm meta package version and platform dependencies |
| `npm/meta/platforms.json` | npm platform mapping |
| `release/support-matrix.json` | Manager build and release platforms |
| `release/compatibility-index.json` | Compatibility routing and lifecycle |
| Compatibility `manifest.toml` | Upstream identity, payload, toolchain, targets, and artifact contract |
| `release/runtime-locks/` | Exact official runtime package contract |
| `release/acceptance/` | Sanitized development acceptance evidence |
| Published `compat-<compat_id>` Release | Final production manifest, descriptor, assets, sizes, and checksums |

Manager Releases use `vX.Y.Z`. Patched Codex Releases use `compat-<compat_id>`. The two asset streams are independent.
