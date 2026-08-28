# Operating CSA

This guide covers installation, activation, recovery, and removal of the CSA Manager. It is for people running a published Manager and a formal patched Codex compatibility. Payload authors should use [Development](development.md), and maintainers should use [Release process](release.md).

## Know which product you are installing

CSA has two products with separate release streams:

| Product | Release form | Current scope |
| --- | --- | --- |
| Manager | `vX.Y.Z` and `@dslzl/csa` | `0.1.3` on five Manager platforms |
| Patched Codex | `compat-<compat_id>` | Codex `0.150.1` p8 on Windows x64 |

The Manager can be installed on Windows x64, Linux x64, Linux arm64 glibc, macOS x64, and macOS arm64. The current patched Codex compatibility is Windows x64 only. A Manager package for an operating system does not imply that a patched compatibility is available there.

CSA treats these locations as separate identities:

1. the existing official Codex package and launcher;
2. the CSA Manager executable;
3. the Manager-owned patched executable and state;
4. the Manager-owned `codex` shim.

Official Codex paths are read-only. The Manager rejects path overlap with its own root.

## Install the Manager

The npm distribution requires Node.js 18 or newer:

```powershell
npm install --global @dslzl/csa@0.1.3
csa --version
```

For a one-off command, use `npx`:

```powershell
npx @dslzl/csa@0.1.3 --version
```

`npx --yes` suppresses npm's confirmation before it downloads a missing package. It does not bypass CSA validation or answer the later `csa install` release prompt.

### Why npm shows several CSA packages

Users install only `@dslzl/csa`. It is a small meta package containing the JavaScript launcher and exact optional dependencies for these platform packages:

- `@dslzl/csa-win32-x64`
- `@dslzl/csa-linux-x64`
- `@dslzl/csa-linux-arm64`
- `@dslzl/csa-darwin-x64`
- `@dslzl/csa-darwin-arm64`

npm installs the one package that matches the current platform. Keeping native binaries in separate packages is expected; it does not require five user installations.

Package installation has no lifecycle script. It does not download patched Codex, build source, edit `PATH`, change profiles, or replace the official `codex` bin.

### Registry mirror returns 404

First check which registry the client is using:

```powershell
npm config get registry
npm view @dslzl/csa version --registry=https://registry.npmjs.org
```

If the official registry returns the version but a configured mirror returns 404, use the official registry for this install or wait for the mirror to synchronize:

```powershell
npm install --global @dslzl/csa@0.1.3 --registry=https://registry.npmjs.org
```

The same distinction applies to tools such as `bunx` when they are configured to use an npm mirror.

## Choose the Manager root

Without `--manager-root`, CSA uses the platform user-data directory and reports the resolved absolute path in `doctor` and `status` output.

For automation and manual acceptance, pass an absolute normalized root. This keeps test state visible and prevents accidental use of the normal per-user location:

```powershell
$ManagerRoot = Join-Path $env:LOCALAPPDATA 'CSA\managed-test'
csa doctor --manager-root $ManagerRoot
```

Do not place the root inside the repository, official Codex package, default `CODEX_HOME`, or another isolated test directory.

## Inspect the official installation

Run `doctor` before installation:

```powershell
csa doctor
```

The JSON report includes the Manager target, resolved root, official launcher, detected native executable, official version, runtime package information, and optional compatibility checks.

If automatic discovery is ambiguous, use absolute paths:

```powershell
csa doctor `
  --official C:\absolute\official\codex.cmd `
  --official-native C:\absolute\official\node_modules\@openai\codex-win32-x64\vendor\x86_64-pc-windows-msvc\codex\codex.exe
```

Do not point either option at a CSA-owned executable.

## Install from formal GitHub Releases

In an interactive terminal:

```powershell
csa install
```

CSA reads formal `compat-*` Releases from the fixed `DSLZL/CSA` repository. Drafts and prereleases are ignored. The list is sorted by Codex version and patch generation. Entries that do not match the current Manager target or installed official Codex version remain visible but cannot be selected.

The selected Release is checked against its annotated tag, CSA commit, upstream Codex tag and commit, manifest, exact asset inventory, file sizes, and SHA-256 values. A mismatch stops the install.

Bare `csa install` needs interactive stdin and stderr. Scripts and CI must select an exact ID:

```powershell
csa install --compat rust-v0.150.1-native-join-p8
```

Public GitHub API access normally needs no token. If the rate limit is exhausted, set `GITHUB_TOKEN` or `GH_TOKEN` only for that process. CSA attaches the token only to fixed `api.github.com` metadata requests, never forwards it to redirected asset hosts, and never stores it.

## Install an exact local payload

Local mode is for payload development and acceptance. It does not make an incompatible official version acceptable.

Pass a manifest and exactly one local artifact or source directory:

```powershell
$CompatId = 'rust-v0.150.1-native-join-p8'
$Manifest = "C:\absolute\payload\$CompatId\manifest.toml"

csa install `
  --manager-root $ManagerRoot `
  --manifest $Manifest `
  --artifact C:\absolute\patched\codex.exe
```

To build from an exact source checkout instead:

```powershell
csa prepare `
  --manager-root $ManagerRoot `
  --manifest $Manifest `
  --source C:\absolute\clean-codex-source

csa plug --manager-root $ManagerRoot
```

The compatibility directory name must equal the manifest `compat_id`. Source preimages, official runtime files, toolchain, target, output size, and hashes remain fail-closed checks.

## Activate the shim

`install` prepares the payload and publishes `<manager-root>/bin/codex`, but does not edit `PATH`. Test the shim in the current process before making any persistent change.

PowerShell with the default Manager root:

```powershell
$Status = csa status | ConvertFrom-Json
$env:PATH = $Status.activation.managed_bin + [IO.Path]::PathSeparator + $env:PATH
```

PowerShell with an explicit root:

```powershell
$env:PATH = (Join-Path $ManagerRoot 'bin') + [IO.Path]::PathSeparator + $env:PATH
```

Command Prompt:

```bat
set "PATH=C:\absolute\manager-root\bin;%PATH%"
```

Bash or Zsh:

```bash
export PATH="/absolute/manager-root/bin:$PATH"
```

Keep the official Codex launcher later on `PATH`. Confirm resolution in a new shell before adding the managed directory to a persistent user `PATH`. CSA does not automate profile changes.

## Read health and activation state

`status` is the authoritative runtime check:

```powershell
csa status
Get-Command codex -All
codex --version
```

The top-level status is:

| Status | Meaning |
| --- | --- |
| `unprepared` | No valid prepared state exists |
| `prepared` | The recorded patched artifact and official binding validate |
| `invalidated` | Prepared state exists, but a current integrity check failed |

The nested activation status is:

| Activation | Meaning |
| --- | --- |
| `unplugged` | No managed shim is active |
| `plugged` | Shim and binding validate |
| `fallback` | The shim state is present but cannot safely launch the patched artifact |

Both official and patched Codex `0.150.1` report `codex-cli 0.150.1`. Version output alone does not prove which executable ran. Use `status`, command resolution, and the reported absolute paths together.

An official Codex upgrade intentionally invalidates an older prepared binding. Run `status`, then install a CSA compatibility for the new exact version when one is available.

## Unplug, uninstall, or purge

Use the narrowest command that matches the goal:

| Command | Removed | Preserved |
| --- | --- | --- |
| `csa unplug` | Active shim | Prepared payload and state |
| `csa uninstall` | Shim and prepared installation | Official Codex, user data, npm package |
| `csa purge` | All Manager-owned shim, prepared, source, build, and state data | Official Codex, external packages, user data |

Repeated `unplug` and `uninstall` calls are safe.

```powershell
csa uninstall
npm uninstall --global @dslzl/csa
```

Remove a persistent Manager `bin` entry separately, after a new shell resolves `codex` to the official launcher.

## Recover from a failed or interrupted install

Use this order:

1. Run `csa unplug` by the Manager's absolute path.
2. Open a new shell and confirm that `codex` resolves to the official launcher.
3. If it still resolves to the managed directory, remove only that `PATH` entry and open another shell.
4. Save the JSON from `csa status` for diagnosis.
5. Run `csa uninstall` after official fallback works.
6. Recheck the official launcher and native executable.
7. Remove the npm package last.

The next Manager command recovers interrupted plug or unplug transactions. An online download failure removes its per-attempt staging directory. A completed prepare keeps its small compatibility payload under the Manager root, so later status and removal do not depend on a temporary download.

Never repair the official package in place as part of CSA recovery. If its files changed unexpectedly, stop and repair or reinstall official Codex through its own package manager.

## Authentication and evidence

A normal shim launch inherits the current `CODEX_HOME`, configuration, authentication, working directory, and terminal. CSA does not copy those files.

`exec --isolated` requires separate absolute directories for `CODEX_HOME`, cwd, logs, state, and the evidence record. For an operator-authorized authenticated test, create that isolated home outside the repository and populate only the required configuration and authentication files manually. Do not automate copying secrets from the default home, and never commit or upload them.

Evidence may contain paths, versions, hashes, timestamps, and exit results. It must not contain tokens, cookies, authorization headers, `auth.json` contents, session content, or full environment dumps.

## Current limitations

- The current formal patched compatibility is Codex `0.150.1` p8 on Windows x64.
- Its formal acceptance covers an authenticated single-child Native Join. Multi-child Native Join, Ultra runtime behavior, and interactive TUI acceptance are still unverified.
- Manager archives and npm packages are published for five platforms, but no non-Windows patched compatibility is currently published.
- `purge` intentionally leaves official files, user configuration, authentication, and external package-manager state untouched.
