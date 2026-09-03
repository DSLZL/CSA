# Developing CSA Manager

This repository contains the CSA Manager only. Patched Codex payloads, exact-source validation, native builds, acceptance records, and compatibility releases live in [`DSLZL/CSA-codex`](https://github.com/DSLZL/CSA-codex) and its six platform build repositories.

Read [CSA architecture](architecture.md) for the runtime model and [Release process](release.md) before changing publication behavior.

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
| `tests/` | Cross-module Manager tests |
| `release/support-matrix.json` | Manager build and release platforms |
| `release/release-inputs.schema.json` | Manager release-candidate input contract |
| `release/install-catalog-bootstrap-v1.json` | Legacy compatibility discovery bootstrap consumed by the Manager |
| `scripts/` | Manager release, npm staging, distribution, and test helpers |
| `npm/` | Meta package, launcher, platform metadata, and launcher tests |
| `.github/workflows/` | Manager CI, GitHub Release, and npm publication workflows |

Compatibility facts belong in `DSLZL/CSA-codex`, not in Manager workflow YAML or Rust version conditionals.

## Toolchains

Use the Rust toolchain pinned by `rust-toolchain.toml`. Local work also uses Python 3 for release helpers, Node.js 18 or newer for npm launcher tests, Git for provenance checks, and PowerShell for the Windows distribution harness.

## Build and test the Manager

Run focused tests while editing, then the full gate:

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --all-targets
cargo build --release --locked
node --check npm\meta\bin\csa.js
node --check scripts\stage_npm_packages.mjs
node scripts\test_npm_launcher.mjs
py -3 scripts\test_release_tools.py
```

These checks validate the Manager repository. They do not prove a patched-Codex build, target-specific Codex packaging, or runtime acceptance; those gates belong to the producer repositories.

## Test local preparation safely

Manager compatibility parsing and local preparation still accept an exact external manifest with either a prepared artifact or a clean source checkout. Keep all test inputs outside the repository and use disposable directories.

Use `csa exec --isolated` for routine patched-runtime checks:

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

Every isolated path must be absolute, normalized, distinct from the others, and outside Manager and official Codex trees. Pre-create the required directories. The evidence record must not already exist.

Daily tests must not:

- call a bare `codex` outside a dedicated command-resolution test;
- persistently change `PATH` or a shell profile;
- install npm packages into the real global prefix;
- use the default `CODEX_HOME`;
- place credentials, sessions, or full environment dumps in evidence.

## Test the packaged Windows distribution

The Windows distribution harness installs local npm tarballs into a temporary prefix. It exercises Manager discovery, local installation, status, isolated execution, child-only shim resolution, uninstall, official fallback, and cleanup.

```powershell
$TempRoot = Join-Path $env:TEMP ("csa-e2e-" + [Guid]::NewGuid().ToString('N'))

powershell -NoProfile -File .\scripts\test_npm_distribution_windows.ps1 `
  -MetaTarball C:\absolute\dslzl-csa-0.1.8.tgz `
  -PlatformTarball C:\absolute\dslzl-csa-win32-x64-0.1.8.tgz `
  -TempRoot $TempRoot `
  -OutputPath C:\absolute\trellis-e2e.json `
  -Official C:\absolute\official\codex.exe `
  -OfficialNative C:\absolute\official-native\codex.exe `
  -Manifest C:\absolute\manifest.toml `
  -Artifact C:\absolute\patched\codex.exe `
  -TrellisSource C:\absolute\path\to\.trellis
```

The parent `PATH`, default `CODEX_HOME`, npm state, profiles, and official installation must have the same fingerprints before and after the test.

## Stop conditions

Stop the test if it would:

- replace, modify, or repair official Codex;
- use the default `CODEX_HOME` without explicit operator authorization;
- put authentication data inside the repository, fixture, logs, or evidence;
- persist `PATH`, profile, or global npm changes;
- invoke bare `codex` where an absolute test path is required;
- run two write-capable agents in one worktree;
- label an unavailable lane as passing;
- continue after executable, source, runtime, or checksum drift.
