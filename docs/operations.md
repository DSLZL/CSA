# Operating CSA

This guide is for people installing and running a published CSA Manager with a formal patched Codex compatibility. For command schemas and status values, use the [CLI reference](reference.md). Payload authors should use [Development](development.md), and release maintainers should use [Release process](release.md).

## Before you install

You need:

- Node.js 18 or newer when installing from npm;
- a working official Codex CLI;
- a formal CSA compatibility that exactly matches the official Codex version and current target.

CSA has two independent products:

| Product | Release namespace | Current release |
| --- | --- | --- |
| CSA Manager | `vX.Y.Z` and `@dslzl/csa` | `0.1.8` |
| Patched Codex | `compat-<compat_id>` | Codex `0.151.0` p10 |

The Manager never installs, downgrades, or repairs official Codex. It only accepts a patched Release that already matches the installed runtime.

## Install the Manager

Install the npm meta package globally:

```powershell
npm install --global @dslzl/csa@0.1.8
csa --version
```

For one-off use:

```powershell
npx @dslzl/csa@0.1.8 --version
bunx @dslzl/csa@0.1.8 --version
```

`npx --yes` suppresses npm's confirmation before installing a missing package. It does not bypass CSA validation or answer the later version picker. `csa install --yes` is the separate CSA option for automatic compatibility selection.

Installing the npm package has no lifecycle script. It does not download patched Codex, build source, edit `PATH`, create a shim, or change the official package.

### Why npm lists several CSA packages

Users install only `@dslzl/csa`. It contains the JavaScript launcher and exact optional dependencies for five native platform packages:

- `@dslzl/csa-win32-x64`
- `@dslzl/csa-linux-x64`
- `@dslzl/csa-linux-arm64`
- `@dslzl/csa-darwin-x64`
- `@dslzl/csa-darwin-arm64`

npm selects the package for the current operating system and architecture. The launcher verifies the platform package, target, binary path, and SHA-256 before starting the Rust Manager.

### When an npm mirror returns 404

Check the configured registry and compare it with the official npm registry:

```powershell
npm config get registry
npm view @dslzl/csa version --registry=https://registry.npmjs.org
```

If the official registry has the version but a mirror returns 404, use the official registry for this installation or wait for synchronization:

```powershell
npm install --global @dslzl/csa@0.1.8 --registry=https://registry.npmjs.org
```

The same issue can affect `bunx` when Bun is configured to use an npm mirror.

## Choose a Manager root

Without `--manager-root`, CSA uses the platform local-data directory. `doctor --json` and `status --json` report the resolved absolute path.

Use an explicit root for manual acceptance or automation:

```powershell
$ManagerRoot = Join-Path $env:LOCALAPPDATA 'CSA\managed-test'
csa doctor --manager-root $ManagerRoot
```

The root must be absolute and normalized. Do not put it inside the repository, official Codex package, default `CODEX_HOME`, or another isolated test directory.

## Inspect official Codex

Run `doctor` before installation:

```powershell
csa doctor
```

Human output lists ordered PASS, WARN, and FAIL checks for:

- the official launcher, native executable, and platform package;
- prepared Manager state;
- shim activation;
- current `codex` command precedence;
- an optional local compatibility manifest.

Warnings and failures include their impact and a recovery action. Use `csa doctor --json` for exact paths, hashes, runtime files, target, and compatibility data.

If discovery is ambiguous, provide absolute paths:

```powershell
csa doctor `
  --official C:\absolute\official\codex.cmd `
  --official-native C:\absolute\official\node_modules\@openai\codex-win32-x64\vendor\x86_64-pc-windows-msvc\codex\codex.exe
```

Do not point either option at a CSA-owned executable.

## Install a formal patched Release

Start the interactive installer:

```powershell
csa install
```

CSA reads public `DSLZL/CSA` Git refs and probes at most the 16 newest compatibility tags for `install-catalog-v1.json`. An embedded catalog covers older p3, p8, and p9 Releases that predate that asset.

The catalog only supplies picker rows. It does not authorize installation.

### Use the version picker

The picker opens only when stdin, stdout, and stderr are terminals, Human output is active, and neither `--yes` nor `--compat` was supplied.

It displays five rows after filtering to the resolved artifact target and official Codex version. Linux GNU Manager targets resolve to the corresponding musl artifact target. The unique greatest numeric `-pN` revision starts as `Recommended`. An exact valid prepared state is marked `Installed`.

| Key | Action |
| --- | --- |
| Up/Down | Move one row and wrap at the first or last item |
| PgUp/PgDn | Move five rows and stop at the boundary |
| Home/End | Move to the first or last result |
| `/` or printable text | Start or continue a search |
| Backspace | Remove one search character |
| Enter | Install the selected row |
| Escape | Clear search, then cancel |
| Ctrl+C | Cancel |

Search matches the patch revision, compatibility ID, Codex version, and acceptance date.

Cancellation restores the terminal, removes staging, prints `Installation cancelled.`, and exits 130 before the large artifact download, preparation, activation, or `PATH` change.

### Select without interaction

```powershell
csa install --yes
```

`--yes`, `--json`, and non-interactive streams select the same unique greatest numeric revision without reading stdin. A tie fails closed and requires an exact compatibility ID.

Pin an older matching Release:

```powershell
csa install --compat rust-v0.150.1-native-join-p8
```

Exact `--compat` bypasses the display catalog, but it does not bypass version, target, Release, size, or checksum validation.

### What CSA downloads

After selection, CSA validates the selected tag commit, Release descriptor, complete checksum inventory, upstream identity, current target, filename, size, and SHA-256.

Online install downloads only:

```text
SHA256SUMS
compatibility-release.json
<current-target patched Codex executable>
```

Patch files, source hashes, test contracts, and source manifests remain in the Release for build and audit work. They are not downloaded or stored by online installation.

Interactive terminals show metadata verification, connection state, transferred bytes, and final verification. JSON and redirected output contain only the final machine report.

### Direct and mirror routing

Installation does not use the GitHub REST API and does not require `GITHUB_TOKEN` or `GH_TOKEN`.

CSA runs five-second Cloudflare and Alibaba/Taobao region probes in parallel. If either explicitly returns mainland China (`CN`), CSA enables the fixed `gh-proxy.org`, `v4.gh-proxy.org`, `v6.gh-proxy.org`, `cdn.gh-proxy.org`, `axisnow.gh-proxy.org`, legacy `gh-proxy.com`, and `ghfast.top` pool.

Mirror health is checked with the actual CSA Git refs request. Once the exact executable is known, every active mirror concurrently requests its first 256 KiB with a three-second limit. Only a valid `206 Content-Range` response whose total matches the declared artifact size is ranked. CSA orders successful samples by complete request and transfer time, then keeps unmeasured nodes as later fallbacks.

A failed transfer, size check, or SHA-256 check removes that node and retries the next one. Outside mainland China, GitHub direct is first. Qualifying direct network failures switch the remaining installation to the same pool.

Region and speed results are not stored. Credentials are not sent to mirrors. Redirect hosts remain restricted, and every download must pass the formal Release checks.

## Activate and verify the shim

Preparation publishes a content-addressed patched artifact and a minimal runtime manifest. Activation then creates `<manager-root>/bin/codex[.exe]`.

On Windows, `install` and `plug` first put that directory at the front of the current user's persistent `PATH`. CSA reconstructs the next process's machine-plus-user ordering. If a machine entry still wins, Windows displays a UAC prompt; after approval, CSA installs a protected dispatcher at `%ProgramFiles%\DSLZL\CSA\bin\codex.exe` and puts that directory first in the machine `PATH`. Package-manager launchers remain untouched.

Existing applications keep the environment they inherited at startup. Close every terminal and fully quit terminal hosts such as VS Code after installation; opening another integrated terminal inside the same window is not enough.

Windows places the machine `PATH` before the user `PATH`. CSA handles that conflict through the elevated dispatcher. Denying UAC returns `path_elevation_failed` and rolls back activation. `path_precedence_conflict` is reserved for a policy or later machine `PATH` change that still prevents the CSA entry from becoming first.

Verify from a new terminal:

```powershell
csa status
where.exe codex
Get-Command codex -All
codex --version
```

Keep the official Codex launcher later on `PATH`. The shim needs it for safe fallback.

## Switch between patched and official Codex

Use `unplug` to stop using the patched executable without deleting prepared data:

```powershell
csa unplug
csa status
Get-Command codex -All
```

Use `plug` to reactivate the same prepared state after it validates:

```powershell
csa plug
csa status
```

Normal official and patched launches use the same current `CODEX_HOME`, so configuration, authentication, sessions, and database state remain available in both modes.

The p10 patch handles the exact known SQLx migration checksum caused by cross-host line endings. It updates only that known checksum in one transaction and leaves unknown drift to SQLx validation. It does not delete or rebuild the database. The formal Windows x64 acceptance record does not yet cover a complete official-to-patched-to-official database roundtrip, so test sensitive workflows in an isolated home first.

An official Codex upgrade changes the recorded version or runtime fingerprints and intentionally invalidates an older prepared binding. Install a compatibility for the new exact version when one is published.

## Install an exact local payload

Local mode is for payload development and acceptance. It does not relax compatibility checks.

Install a prepared artifact:

```powershell
$CompatId = 'rust-v0.150.1-native-join-p9'
$Manifest = "C:\absolute\payload\$CompatId\manifest.toml"

csa install `
  --manager-root $ManagerRoot `
  --manifest $Manifest `
  --artifact C:\absolute\patched\codex.exe
```

Prepare from an exact clean source checkout, then activate:

```powershell
csa prepare `
  --manager-root $ManagerRoot `
  --manifest $Manifest `
  --source C:\absolute\clean-codex-source

csa plug --manager-root $ManagerRoot
```

Local preparation validates upstream identity, source preimages, the complete ordered patch series, generation and test commands, toolchain, target, artifact size, and hash. It never applies fuzzy or three-way patches.

## Read status and diagnostics

`status` is the authoritative runtime check:

```powershell
csa status
csa status --json
```

Human output leads with installed, active, and healthy conclusions. JSON includes full paths, hashes, runtime files, timestamps, and raw state.

`doctor` is a read-only diagnosis. It exits 0 for PASS/WARN-only results, 1 for a fully diagnosed FAIL, and 2 when invalid input, I/O, corrupt state, or another error prevents a complete assessment.

See [CSA reference](reference.md) for every status and activation value.

## Recover from a failed or interrupted install

Use this order:

1. Run `csa status --json` and keep the report.
2. Run `csa unplug` by the Manager's absolute path.
3. Open a new shell and confirm that `codex` resolves to the official launcher.
4. If command precedence is still stale, fully restart the host application.
5. Run `csa uninstall` after official fallback works.
6. Recheck the official launcher and native executable.
7. Remove the npm package last.

The next Manager command recovers interrupted plug or unplug transactions. Failed online downloads remove their attempt-specific staging directory. A completed prepare may remain inactive for diagnosis or retry.

Do not repair official Codex in place as part of CSA recovery. If official files changed, reinstall or repair them through their own package manager.

## Unplug, uninstall, or purge

Choose the narrowest command:

| Command | Removed | Preserved |
| --- | --- | --- |
| `csa unplug` | Active shim | Prepared payload and state |
| `csa uninstall` | Shim, prepared installation, exact user-`PATH` entry, and elevated CSA dispatcher registration | Official Codex, user data, npm package |
| `csa purge` | All Manager-owned shim, prepared, source, build, state data, and exact user/system CSA `PATH` entries | Official Codex, user data, external packages |

These commands are idempotent. Windows may request UAC when `uninstall` or `purge` removes the Program Files dispatcher and its machine `PATH` entry.

```powershell
csa uninstall
npm uninstall --global @dslzl/csa
```

## Troubleshooting

| Symptom | Check | Action |
| --- | --- | --- |
| `csa` is not recognized | `Get-Command csa -All` and npm global bin | Restart the terminal or use `npx`/`bunx` |
| npm or Bun mirror returns 404 | `npm config get registry` | Use `registry.npmjs.org` or wait for mirror sync |
| Picker has no versions | `csa doctor --json` official version and Manager target | Install a matching official Codex version or wait for a formal compatibility for the resolved artifact target |
| Install pauses after selection | Human progress and network route | Wait for bounded metadata and mirror probes; retry if a structured network error appears |
| `codex` still resolves to official | `csa status` and `Get-Command codex -All` | Close all terminals and fully restart the terminal host |
| Install reports `path_elevation_failed` | UAC was denied or unavailable | Retry and approve the administrator prompt |
| Install reports `path_precedence_conflict` | Policy or later machine `PATH` changes still override CSA | Ask an administrator to inspect the machine `PATH`, then run `csa plug` |
| `codex --version` has no `CSA` marker | The official command still wins or patched mode is inactive | Check that `where.exe codex` lists a CSA path first, then run `csa status` |
| State becomes invalidated after official upgrade | Official version and hashes | Install a compatibility for the new exact official release |
| Shim reports fallback | Activation reason in `status --json` | Keep official Codex on `PATH`, then reinstall or unplug |
| Codex reports a migration checksum mismatch | Database path and exact error | Stop; do not delete the database. Verify the selected compatibility and use an isolated copy for diagnosis |

## Authentication and evidence

A normal shim launch inherits the current `CODEX_HOME`, configuration, authentication, working directory, and terminal. CSA does not copy these files.

`exec --isolated` requires separate absolute directories for `CODEX_HOME`, cwd, logs, state, and evidence. When an operator authorizes authenticated testing, populate only the required files in that isolated home. Do not automate reading or copying secrets from the default home.

Evidence may include paths, versions, hashes, timestamps, and exit results. It must not include tokens, cookies, authorization headers, `auth.json` contents, session content, or full environment dumps.

## Current limitations

- The current formal patched Release is Codex `0.151.0` p10 with six native artifacts.
- Formal Windows x64 evidence covers exact executable identity, official runtime binding, official-file immutability, and an authenticated single-child Native Join.
- Multi-child Native Join, the complete database roundtrip, Ultra runtime behavior, and interactive TUI acceptance remain unverified.
- Windows arm64 has a patched Codex artifact but no CSA Manager npm package.
- Current Linux Manager packages use GNU targets, while the p10 patched Linux artifacts use musl targets. Exact target matching means those artifacts are not an installable pair.
