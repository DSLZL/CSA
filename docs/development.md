# Developing CSA

This guide covers Manager changes, compatibility payload work, the lightweight TUI harness, and isolated runtime acceptance. Read [Release process](release.md) before changing publication authority or compatibility lifecycle data.

## Read the project contracts first

CSA uses Trellis for project-specific development rules. Before editing a package or layer:

1. read `.trellis/workflow.md`;
2. read the relevant index under `.trellis/spec/`;
3. follow the linked CSA contract for the code or payload being changed;
4. keep official Codex source and installed runtime files read-only.

The repository `AGENTS.md` defines the normal build, test, style, and security requirements.

## Repository map

| Path | Purpose |
| --- | --- |
| `src/` | Rust Manager, CLI parsing, discovery, online install, state, activation, and isolation |
| `tests/` | Cross-module Manager tests and the standalone UI harness |
| `payload/codex/<compat-id>/` | Version-specific manifest, patches, hashes, and test contract |
| `release/compatibility-index.json` | Compatibility routing and lifecycle |
| `release/build-profiles/` | Pinned toolchain and build identities |
| `release/runtime-locks/` | Exact official runtime package contracts |
| `release/acceptance/` | Sanitized local acceptance records |
| `scripts/` | Build, validation, release, npm staging, and test helpers |
| `validation/` | Replacement and payload-structure checks |
| `npm/` | Meta package, launcher, platform package metadata, and launcher tests |
| `.github/workflows/` | CI, validation, candidate, Manager release, and compatibility release workflows |

Compatibility facts belong in the catalog, manifest, build profile, runtime lock, and acceptance record. Do not duplicate version authority in workflow YAML or Rust conditionals.

## Toolchains and prerequisites

The Manager crate declares Rust `1.89` as its minimum supported toolchain. Current CI, Manager release builds, and patched Codex `0.150.1` p8 are pinned to Rust `1.95.0`.

Local work also uses:

- Python 3 for validation and release helpers;
- Node.js 18 or newer for npm launcher and package tests;
- Git for exact upstream tag and commit checks;
- PowerShell for the Windows distribution harness.

The hosted patched-Codex workflows prepare their pinned Rust, xwin, LLVM, build-tool, and official-runtime inputs. Cache hits improve runtime but never replace exact preparation or validation.

## Build and test the Manager

Run focused tests while editing, then the full Manager gate:

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --all-targets
cargo build --release --locked
```

Validate the npm launcher and release helpers:

```powershell
node --check npm\meta\bin\csa.js
node --check scripts\stage_npm_packages.mjs
node scripts\test_npm_launcher.mjs
py -3 scripts\test_validation_evidence.py
py -3 scripts\test_compat_catalog.py
py -3 scripts\test_verify_release_asset_set.py
py -3 scripts\test_verify_patch_payload.py
py -3 scripts\test_release_tools.py
```

These checks do not prove a hosted patched-Codex build or runtime acceptance. Report unavailable lanes as unverified.

## Work on a compatibility payload

A compatibility directory is named exactly after its `compat_id`. Its manifest pins:

- upstream Codex version, tag, and commit;
- patch-set version, ordered patches, and exact source preimages;
- target, product, package, binary, and build command;
- Rust and build-profile identity;
- runtime lock and expected official files;
- output asset name, size, and SHA-256 state;
- generation and test contract.

Keep the upstream checkout outside the CSA repository. Check out the exact manifest commit in detached mode, verify the clean source, and apply patches only through the repository tooling. Never use fuzzy or three-way application to make a version-bound patch fit.

The current p8 test contract covers workspace formatting, schema generation and reverse checks, parent fork and transport inheritance, completion and terminal-outcome mapping, single and batch Join schemas, Native Join integration, TUI live state, Orbit rendering, complete TUI library tests, TUI Clippy, and official runtime overlay behavior.

Committed candidate manifests are immutable build inputs. A formal workflow finalizes a temporary manifest copy from the independently built production executable. Do not write a production hash back into the committed candidate manifest by hand.

## Keep the control plane and SUT separate

Use two identities:

```text
Official Codex control plane
  -> reads Trellis context
  -> edits CSA and runs checks
  -> launches the SUT by an absolute path

Patched Codex SUT
  -> uses a disposable fixture or worktree
  -> uses a separate CODEX_HOME, cwd, logs, state, npm prefix, and PATH
  -> never replaces or controls the official process
```

The two processes must not write the same worktree. A write-capable patched test uses a copied fixture or disposable worktree.

Daily patched runs should use `csa exec --isolated`:

```powershell
csa exec --isolated `
  --manager-root C:\absolute\manager-root `
  --codex-home C:\absolute\isolated\codex-home `
  --cwd C:\absolute\fixture `
  --logs-dir C:\absolute\logs `
  --state-dir C:\absolute\state `
  --record C:\absolute\evidence.json `
  --npm-prefix C:\absolute\npm-prefix `
  -- --version
```

Every isolated path must be absolute, normalized, pre-created where required, distinct from the others, and outside the Manager and official Codex trees. The evidence record path must not already exist.

## Run the lightweight TUI harness

`tests/ui` is a standalone Ratatui program. It displays every subagent lifecycle state and a complete starting, running, multi-task, done animation without compiling or launching Codex.

```powershell
cargo run --manifest-path tests/ui/Cargo.toml
```

Controls:

| Key | Action |
| --- | --- |
| `m` | Toggle animated and reduced motion |
| `g` | Cycle text, Sixel, and Kitty rendering |
| `q` or `Esc` | Quit |

Run its focused tests with:

```powershell
cargo test --manifest-path tests/ui/Cargo.toml
```

If Windows reports that it cannot remove `tests\ui\target\debug\csa-ui-harness.exe`, an earlier harness process still owns the executable. Quit that window with `q` or `Esc`, or stop only that harness process, then rerun Cargo.

The harness is visual feedback, not Codex integration, terminal-protocol, ConPTY, patched-binary, or release acceptance evidence. Its rendering implementation must remain synchronized with the p8 TUI patch when either copy changes.

## Test the packaged Windows distribution

The Windows distribution harness runs in disposable directories. It installs local npm tarballs into a temporary prefix, exercises doctor, install, status, isolated execution, child-only shim resolution, uninstall, official fallback, and cleanup.

```powershell
$TempRoot = Join-Path $env:TEMP ("csa-e2e-" + [Guid]::NewGuid().ToString('N'))

powershell -NoProfile -File .\scripts\test_npm_distribution_windows.ps1 `
  -MetaTarball C:\absolute\dslzl-csa-0.1.3.tgz `
  -PlatformTarball C:\absolute\dslzl-csa-win32-x64-0.1.3.tgz `
  -TempRoot $TempRoot `
  -OutputPath C:\absolute\trellis-e2e.json `
  -Official C:\absolute\official\codex.exe `
  -OfficialNative C:\absolute\official-native\codex.exe `
  -Manifest C:\absolute\manifest.toml `
  -Artifact C:\absolute\patched\codex.exe `
  -TrellisSource C:\absolute\path\to\.trellis
```

The parent process `PATH`, default `CODEX_HOME`, npm state, profile, and official installation must have the same fingerprints before and after the test.

## Current runtime evidence

The accepted Windows x64 record for `rust-v0.150.1-native-join-p8` binds:

| Field | Accepted value |
| --- | --- |
| Codex version | `0.150.1` |
| Artifact SHA-256 | `b9ce5046cf52c6e5d2ae7a73f69497143c3c98160608b643a08f76734ca9dc93` |
| Artifact size | `310791168` bytes |
| Patch Validation run | `33155290376` |
| Candidate build run | `33156817477` |

Recorded acceptance passed:

- checksum and absolute `codex-cli 0.150.1` execution;
- binding to the complete official Bun runtime with six verified files;
- an authenticated single-child Native Join with one Join call and the expected terminal sentinel;
- unchanged official runtime fingerprints.

The record does not verify:

- multi-child Native Join;
- Ultra runtime behavior;
- interactive TUI acceptance.

Do not generalize the single-child result to those lanes.

## Run an authenticated manual lane

Authentication tests require explicit operator authorization.

1. Create an isolated `CODEX_HOME` outside the repository.
2. If the operator authorizes reuse, copy only `config.toml` and `auth.json` into that isolated home. Do not inspect, log, commit, or upload their contents.
3. Use a disposable fixture and separate logs, state, npm prefix, and Manager root.
4. Launch through `exec --isolated` or an exact absolute patched executable.
5. Record executable paths, hashes, request and completion timing, Join calls, terminal outcomes, and official before-and-after hashes.
6. Sanitize evidence before it enters `release/acceptance/`.
7. Remove the isolated home when it is no longer needed.

The default `CODEX_HOME` must remain unchanged. A manual authenticated result does not replace fake-provider protocol tests, hosted builds, or the remaining multi-child, Ultra, and interactive TUI gates.

## Stop on an isolation failure

Stop immediately if a test would:

- replace, modify, or repair official Codex files;
- reuse the default `CODEX_HOME` without explicit operator authorization;
- put authentication data inside the repository, fixture, logs, or evidence;
- persist `PATH` or profile changes without explicit authorization;
- invoke a bare `codex` where an absolute SUT path is required;
- run two write-capable agents in one worktree;
- hide a missing or unverified lane as passing;
- continue after executable identity, source preimage, runtime, or checksum drift.
