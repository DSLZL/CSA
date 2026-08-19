# CSA

CSA packages a fail-closed manager and a version-pinned Codex patch that adds one native `join` operation for child runs. It installs beside the official Codex CLI and never replaces it.

Current status: **publication is gated by the hosted Release workflows**. Windows x64 has a locally built and focused-verified Codex `0.148.0` candidate; the compatibility workflow reruns the complete contract before publishing, and the CSA workflow builds all five manager platforms. See [release-readiness.md](release-readiness.md).

[中文说明](README_ZH.md)

## Why it exists

The patched runtime lets a parent submit one native Join and remain pending until the exact child run becomes terminal. It removes client-side wait/status polling while preserving approval, cancellation, replay, and shutdown behavior.

CSA exposes the added `join_agent` as a normal function because providers reject new tool names in the reserved `collaboration` namespace. The upstream multi-agent tools keep their original namespace.

The package keeps two roles separate:

```text
Official Codex       runs Trellis and controls development
Patched Codex SUT    runs only by absolute path, isolated exec, or an explicit shim
```

Installing `@dslzl/csa` exposes only `csa` and runs no lifecycle scripts. Package installation does not download, build, patch, activate, or edit PATH/profile. A later explicit `csa install` discovers the official CLI, accepts only OpenAI's current formal `rust-vX.Y.Z` release, and downloads the matching formal `compat-<compat_id>` assets from `dslzl/CSA`. The official `codex` launcher and native binary remain external and unchanged.

GitHub Releases use two separate streams: `vX.Y.Z` contains CSA manager/npm artifacts only, while `compat-<compat_id>` contains the patched Codex compatibility assets only. The hourly watcher clones upstream Codex into runner-temporary storage, never into this repository.

## Verified scope

| Platform | Manager/npm lane | Patched Codex payload |
| --- | --- | --- |
| Windows x64 | local PASS; CI configured on `windows-2025` | `rust-v0.148.0-native-join-p1` focused PASS; Release requires the full contract |
| Linux x64 | CI configured, not verified | none |
| Linux arm64 | CI configured, not verified | none |
| macOS x64 | CI configured, not verified | none |
| macOS arm64 | CI configured, not verified | none |

The current compatibility candidate binds upstream tag `rust-v0.148.0`, commit `3ba0f711642a888aec92a611a3f3b2211157ff89`, Rust `1.95.0`, and target `x86_64-pc-windows-msvc`. Any upstream or artifact drift fails closed.

## Prerequisites

- Official Codex CLI `0.148.0`, kept installed and discoverable.
- Node.js `>=18`; CI covers Node 22, 24, and 26.
- A supported platform manager package and a published, checksum-matched CSA compatibility Release. Windows x64 is the only patched target currently verified.
- Rust `1.95.0` only when building the manager or patched payload from source.

No package has been published. During development, install the generated tarballs into a temporary prefix:

```powershell
$Prefix = Join-Path $env:TEMP 'csa-prefix'
$PlatformTarball = 'C:\absolute\path\to\dslzl-csa-win32-x64-0.1.0.tgz'
$MetaTarball = 'C:\absolute\path\to\dslzl-csa-0.1.0.tgz'
npm install --prefix $Prefix --offline --no-audit --no-fund `
  $PlatformTarball `
  $MetaTarball
$Manager = Join-Path $Prefix 'node_modules\.bin\csa.cmd'
& $Manager --version
```

After an authorized publication, the intended install is `npm install -g @dslzl/csa@0.1.0`. Do not use that command until the release is marked ready and the packages exist in the registry.

## Cold install and run

After the manager package is published, the normal command is:

```powershell
csa install
csa status
```

This succeeds only when the discovered official CLI version equals OpenAI's current latest non-draft, non-prerelease `rust-vX.Y.Z` release and CSA has published the exact matching formal compatibility Release. It never falls back to an older payload; a newly released but not-yet-supported Codex returns `latest_not_yet_supported`.

For offline diagnosis or local payload development, use absolute paths. Passing an explicit manager root also makes cleanup unambiguous.

```powershell
$ManagerRoot = Join-Path $env:LOCALAPPDATA 'csa\managed'
$Manifest = (Resolve-Path '.\payload\codex\rust-v0.148.0-native-join-p1\manifest.toml').Path
$Artifact = 'C:\absolute\path\to\patched\codex.exe'
$Official = (Get-Command codex -CommandType Application | Select-Object -First 1).Source
$OfficialNative = 'C:\absolute\path\to\official\native\codex.exe'

& $Manager doctor --manager-root $ManagerRoot --official $Official `
  --official-native $OfficialNative --manifest $Manifest
& $Manager install --manager-root $ManagerRoot --official $Official `
  --official-native $OfficialNative --manifest $Manifest --artifact $Artifact
& $Manager status --manager-root $ManagerRoot
```

Both install modes run the same prepare and plug transaction. The no-input mode downloads only the exact reviewed Release manifest, manifest-referenced files, and patched artifact, verifies release/tag/commit/target/size/SHA-256 bindings, and cleans download staging. Supplying `--manifest` plus exactly one `--artifact` or `--source` selects the local-only diagnostic mode. Neither mode edits PATH.

Daily development should use isolated exec. It does not create a shim or change PATH:

```powershell
& $Manager exec --isolated --manager-root $ManagerRoot `
  --codex-home C:\absolute\isolated\codex-home `
  --cwd C:\absolute\fixture `
  --logs-dir C:\absolute\logs `
  --state-dir C:\absolute\state `
  --record C:\absolute\evidence.json `
  --npm-prefix C:\absolute\npm-prefix `
  -- --version
```

## Reversible activation

`plug` copies the manager into its own `bin` directory as the `codex` shim. `install` already invokes it; the lower-level command is useful for retry and diagnosis. Test activation in the current shell or a child shell first:

```powershell
& $Manager plug --manager-root $ManagerRoot
$env:PATH = (Join-Path $ManagerRoot 'bin') + [IO.Path]::PathSeparator + $env:PATH
Get-Command codex -CommandType Application
codex --version

& $Manager uninstall --manager-root $ManagerRoot
Get-Command codex -CommandType Application
codex --version
```

`uninstall` withdraws the shim first, then removes manager-owned activation/preparation data. Normal PATH lookup falls through to the unchanged official launcher. It does not uninstall npm packages or remove the official CLI.

## Recovery

Run these in order:

```powershell
& $Manager uninstall --manager-root $ManagerRoot
Get-Command codex -CommandType Application
codex --version
npm uninstall --prefix $Prefix @dslzl/csa @dslzl/csa-win32-x64
```

If a persistent PATH entry was added manually, remove only the manager `bin` entry after official fallback is confirmed. Never delete or overwrite the official Codex installation as a recovery step.

## Documentation

- [Operations and troubleshooting](docs/operations.md)
- [Development and Trellis isolation](docs/development.md)
- [Compatibility, release, and production plug runbook](docs/release.md)
- [Current release readiness](release-readiness.md)

## Security and non-goals

- Compatibility manifests, source preimages, binaries, state, and activation shims are checksum-bound.
- Invalid, missing, drifted, overlapping, or unverified paths fail closed or fall back to official Codex.
- Automated tests use disposable HOME, `CODEX_HOME`, npm prefix, cwd, logs, state, and child-only PATH.
- Authentication files, tokens, cookies, and sessions are never copied into the repository or release artifacts.
- This project does not hot-patch a running Codex process, silently modify profiles, download from arbitrary origins, or support arbitrary Codex versions. Compatibility publication is eligible only after a reviewed compatibility PR is merged to the default branch.

Licensed under [MIT](LICENSE). Upstream and dependency notices are in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

## Link

- [LINUX DO](https://linux.do/)