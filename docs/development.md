# Developing CSA

This guide covers Manager changes, compatibility payloads, the lightweight TUI harness, packaged distribution tests, and isolated runtime acceptance. Read [CSA architecture](architecture.md) for the runtime model and [Release process](release.md) before changing publication authority.

## Read the contracts first

CSA uses Trellis for project-specific rules. Before editing:

1. read `.trellis/workflow.md`;
2. read `.trellis/spec/csa/index.md`;
3. follow the linked contract for the layer being changed;
4. keep upstream Codex source and installed official runtime files read-only.

The repository `AGENTS.md` defines the normal style, test, security, and tool requirements.

## Repository map

| Path | Purpose |
| --- | --- |
| `src/` | Rust Manager, CLI, discovery, online install, state, activation, output, and isolation |
| `tests/` | Cross-module Manager tests and the standalone UI harness |
| `payload/codex/<compat-id>/` | Legacy version-specific compatibility payloads |
| `payload/codex/<family>/bindings/<compat-id>/` | Schema-2 exact bindings backed by shared family files |
| `release/compatibility-index.json` | Compatibility routing and lifecycle |
| `release/patch-family/` | Reviewed family classifications and analysis |
| `release/build-profiles/` | Toolchain and build identities |
| `release/runtime-locks/` | Exact official runtime contracts |
| `release/acceptance/` | Sanitized local acceptance records |
| `scripts/` | Build, validation, release, npm staging, and test helpers |
| `npm/` | Meta package, launcher, platform metadata, and launcher tests |
| `.github/workflows/` | CI, watcher, development build, validation, and release workflows |

Compatibility facts belong in data files, not workflow YAML or Rust version conditionals.

## Toolchains

The Manager crate declares Rust `1.89` as its minimum supported toolchain. Current CI, Manager release builds, and the Codex `0.151.0` p10 patched build use Rust `1.95.0`.

Local work also uses:

- Python 3 for compatibility and release helpers;
- Node.js 18 or newer for npm launcher tests;
- Git for exact upstream identity and patch preflight;
- PowerShell for the Windows distribution harness.

Hosted patched-Codex workflows prepare pinned Rust, xwin, LLVM, build tools, and official runtime inputs. A cache hit reduces compile time but never replaces exact preparation or validation.

## Build and test the Manager

Run focused tests while editing, then the full Rust gate:

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --all-targets
cargo build --release --locked
```

Check the npm launcher and release helpers:

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

These checks do not prove a hosted patched-Codex build, target-specific packaging, or runtime acceptance. Mark an unavailable lane as unverified.

## Change a compatibility payload

A compatibility manifest pins:

- Codex version, upstream tag, and upstream commit;
- patch revision, ordered patches, and exact source preimages;
- target set, product, package, binary, and build command;
- Rust toolchain and build profile;
- official runtime lock;
- artifact filename, size, and SHA-256 state;
- generation and test contract.

Schema-2 payloads live under `payload/codex/<family>/bindings/<compat-id>/`. Their `[files]` table maps logical payload paths to shared additions or binding-local adapters.

Shared additions can create only paths that are absent in every represented upstream source and have byte-identical patched content. A patch against upstream-owned files remains binding-local until at least two exact bindings prove that its canonical diff is identical.

Validate family ownership before the catalog:

```powershell
py -3 scripts\patch_family.py verify --family payload\codex\native-join-p10
py -3 scripts\compat_catalog.py validate --repository .
```

For a new extraction, keep both upstream checkouts outside the CSA repository and inspect:

```powershell
py -3 scripts\patch_family.py analyze --help
```

Commit the reviewed classification JSON, deterministic analysis JSON, and Markdown report together. The analyzer does not port code or resolve conflicts.

For a new Codex version:

1. check out the exact upstream tag and commit outside the repository;
2. verify that the checkout is clean;
3. generate a new exact binding;
4. recalculate source preimages;
5. port only the binding adapters that drifted;
6. run cumulative patch preflight;
7. run the complete generation and test contract;
8. leave the candidate release-disabled until runtime acceptance is recorded.

Do not edit an accepted or released legacy payload, old family row, old binding, or shared file. Do not use fuzzy application or `git apply --3way`.

Committed candidate manifests may contain placeholder artifact size and hash fields. Formal release builds finalize a temporary manifest copy from independently built production artifacts. Do not write a production hash back into the committed candidate by hand.

## Separate the control plane and test system

Use two Codex identities:

```text
Official Codex control plane
  -> reads Trellis context
  -> edits CSA and runs checks
  -> launches the test system by an absolute path

Patched Codex system under test
  -> uses a disposable fixture or worktree
  -> uses separate CODEX_HOME, cwd, logs, state, npm prefix, and PATH
  -> never replaces or controls the official process
```

The two processes must not write the same worktree. Any write-capable patched test needs a copied fixture or disposable worktree.

Use `csa exec --isolated` for routine patched runs:

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

Every isolated path must be absolute, normalized, distinct from the others, and outside the Manager and official Codex trees. Pre-create the required directories. The evidence record must not already exist.

Daily tests must not:

- call a bare `codex` outside a dedicated command-resolution test;
- persistently change `PATH` or a shell profile;
- install npm packages into the real global prefix;
- use the default `CODEX_HOME`;
- place credentials, sessions, or full environment dumps in evidence.

## Run the lightweight TUI harness

`tests/ui` is a standalone Ratatui program. It displays every subagent lifecycle state and a complete start, running, multi-task, and done animation without compiling or launching Codex.

```powershell
cargo run --manifest-path tests/ui/Cargo.toml
```

| Key | Action |
| --- | --- |
| `m` | Toggle animated and reduced motion |
| `g` | Cycle text, Sixel, and Kitty rendering |
| `q` or Escape | Quit |

Run its focused tests:

```powershell
cargo test --manifest-path tests/ui/Cargo.toml
```

If Windows cannot remove `tests\ui\target\debug\csa-ui-harness.exe`, an earlier harness process still owns the executable. Quit it with `q` or Escape, or stop only that process, then rerun Cargo.

The harness is visual feedback. It is not Codex integration, terminal protocol, ConPTY, patched-binary, or release acceptance evidence. Keep its rendering behavior synchronized with the active TUI patch.

## Build a Windows test candidate

Local Codex builds are expensive. The development workflow builds only Windows x64 from any pushed ref and reuses the same target recipe as formal release:

```powershell
gh workflow run build-patched-codex-windows.yml `
  --ref <branch-or-tag> `
  -f compat_selector=<exact-candidate-id>
```

Download the artifact into an ignored disposable directory:

```powershell
gh run download <run-id> `
  -n patched-codex-windows-test-<compat_id> `
  -D .dev\windows-test-candidate
```

The artifact contains:

```text
bundle/bin/codex.exe
bundle/target-record.json
candidate-record.json
resolution.json
```

This workflow does not run formal Patch Validation, build the other targets, finalize a manifest, or publish a Release. Run the downloaded binary only by absolute path or through an isolated Manager root.

## Test the packaged Windows distribution

The Windows distribution harness installs local npm tarballs into a temporary prefix. It exercises doctor, local install, status, isolated execution, child-only shim resolution, uninstall, official fallback, and cleanup.

```powershell
$TempRoot = Join-Path $env:TEMP ("csa-e2e-" + [Guid]::NewGuid().ToString('N'))

powershell -NoProfile -File .\scripts\test_npm_distribution_windows.ps1 `
  -MetaTarball C:\absolute\dslzl-csa-0.1.6.tgz `
  -PlatformTarball C:\absolute\dslzl-csa-win32-x64-0.1.6.tgz `
  -TempRoot $TempRoot `
  -OutputPath C:\absolute\trellis-e2e.json `
  -Official C:\absolute\official\codex.exe `
  -OfficialNative C:\absolute\official-native\codex.exe `
  -Manifest C:\absolute\manifest.toml `
  -Artifact C:\absolute\patched\codex.exe `
  -TrellisSource C:\absolute\path\to\.trellis
```

The parent `PATH`, default `CODEX_HOME`, npm state, profiles, and official installation must have the same fingerprints before and after the test.

## Run authenticated acceptance

Authenticated tests require explicit operator authorization.

1. Create an isolated `CODEX_HOME` outside the repository.
2. If reuse is authorized, copy only `config.toml` and `auth.json` without inspecting or logging their contents.
3. Use a disposable fixture and separate logs, state, npm prefix, and Manager root.
4. Launch through `exec --isolated` or an exact absolute patched executable.
5. Record executable paths, hashes, request and completion times, Join calls, terminal outcomes, and official before-and-after hashes.
6. Sanitize evidence before it enters `release/acceptance/`.
7. Remove the isolated home when it is no longer needed.

The accepted Windows x64 record for `rust-v0.151.0-native-join-p10` binds:

| Field | Accepted value |
| --- | --- |
| Codex version | `0.151.0` |
| Candidate artifact SHA-256 | `67bdf36f6ac50a79c80b88e39506b2a32200ea5e8bc0d2af76eb34abfe656633` |
| Candidate artifact size | `314201088` bytes |
| Patch Validation run | `33316162397` |
| Candidate build run | `33316173523` |

That record covers exact candidate identity, isolated database initialization, an authenticated single-child Native Join, unchanged official runtime files, and unchanged real configuration and authentication files.

It does not cover multi-child Native Join, a complete database roundtrip, Ultra runtime behavior, or interactive TUI acceptance.

## Stop conditions

Stop the test if it would:

- replace, modify, or repair official Codex;
- use the default `CODEX_HOME` without explicit operator authorization;
- put authentication data inside the repository, fixture, logs, or evidence;
- persist `PATH`, profile, or global npm changes;
- invoke bare `codex` where an absolute test path is required;
- run two write-capable agents in one worktree;
- label an unavailable lane as passing;
- continue after executable, source preimage, runtime, or checksum drift.
