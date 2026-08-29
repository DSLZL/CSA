# Operating CSA

This guide covers installation, activation, recovery, and removal of the CSA Manager. It is for people running a published Manager and a formal patched Codex compatibility. Payload authors should use [Development](development.md), and maintainers should use [Release process](release.md).

## Know which product you are installing

CSA has two products with separate release streams:

| Product | Release form | Current scope |
| --- | --- | --- |
| Manager | `vX.Y.Z` and `@dslzl/csa` | `0.1.4` on five Manager platforms |
| Patched Codex | `compat-<compat_id>` | Codex `0.150.1` p9 on Windows x64 |

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
npm install --global @dslzl/csa@0.1.4
csa --version
```

For a one-off command, use `npx`:

```powershell
npx @dslzl/csa@0.1.4 --version
```

`npx --yes` suppresses npm's confirmation before it downloads a missing package. It does not bypass CSA validation or answer the later release picker. `csa install --yes` is the separate CSA option that auto-selects the recommended compatible Release.

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
npm install --global @dslzl/csa@0.1.4 --registry=https://registry.npmjs.org
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

Interactive output lists ordered `PASS`, `WARN`, and `FAIL` checks for the official installation, prepared state, activation, command precedence, and optional compatibility inputs. Every warning or failure includes its impact and a safe next action. Use `csa doctor --json` to obtain the unchanged Manager target, resolved root, official runtime, command-resolution, and compatibility report.

`doctor` exits `0` when checks contain only PASS/WARN, `1` when it fully diagnoses a FAIL, and `2` when invalid input, I/O, corrupt state, or another operational error prevents a complete assessment.

If automatic discovery is ambiguous, use absolute paths:

```powershell
csa doctor `
  --official C:\absolute\official\codex.cmd `
  --official-native C:\absolute\official\node_modules\@openai\codex-win32-x64\vendor\x86_64-pc-windows-msvc\codex\codex.exe
```

Do not point either option at a CSA-owned executable.

## Install from formal GitHub Releases

In a terminal:

```powershell
csa install
```

CSA discovers public `compat-*` tags without using the GitHub REST API or a login token. It probes at most the 16 newest compatibility Releases for `install-catalog-v1.json`, then uses the committed p3/p8/p9 bootstrap catalog when older Releases do not have that asset. The catalog is display-only: it is not part of a Release's immutable `SHA256SUMS` payload authority.

When stdin, stdout, and stderr are terminals, bare `csa install` opens a fixed five-row picker after filtering to the current Manager target and installed official Codex version. The unique greatest numeric terminal `-pN` revision starts as `Recommended`; an exact valid prepared state is marked `Installed`. Use Up/Down, PgUp/PgDn, Home/End, Enter, Escape, Backspace, `/`, or direct typing. Search covers `pN`, the full compatibility ID, Codex version, and acceptance date. Escape first clears an active search; Escape again or Ctrl+C cancels with exit code 130 before the large artifact, prepare, activation, or PATH changes.

`csa install --yes`, `--json`, and any non-interactive stream skip the picker and auto-select the same unique greatest revision. An unresolved greatest-revision tie fails closed. Exact `--compat` also skips the picker and does not consult the display catalog; local `--manifest` mode rejects `--yes` and remains local.

After a choice, CSA re-enters the existing exact verification path. The selected Release is checked against its peeled tag, CSA commit, upstream Codex tag and commit, manifest, descriptor/checksum coverage, file sizes, and SHA-256 values. A mismatch stops the install. Interactive terminals then show artifact download progress; `--json` and redirected output emit only the final JSON document.

To pin an older exact matching Release, select its full ID explicitly:

```powershell
csa install --compat rust-v0.150.1-native-join-p8
```

No GitHub login or token is required. Before the first GitHub request, CSA runs five-second bounded country probes against Cloudflare's fixed trace endpoint and Alibaba's Taobao IP service in parallel. If either one reports mainland China (`CN`), CSA starts with the same GitHub URL prefixed by `https://gh-proxy.com/`; without a `CN` result, it starts with GitHub directly. If both checks are unavailable, CSA still switches the rest of that installation to the mirror after a connection, timeout, throttling, or transient server failure. Country results are not logged or stored. Redirect hosts remain restricted and every downloaded file still has to pass the Release size and SHA-256 checks.

## Install an exact local payload

Local mode is for payload development and acceptance. It does not make an incompatible official version acceptable.

Pass a manifest and exactly one local artifact or source directory:

```powershell
$CompatId = 'rust-v0.150.1-native-join-p9'
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

On Windows, `install` prepares the payload, publishes `<manager-root>/bin/codex.exe`, moves that managed directory to the front of the current user's persistent `PATH`, and silently verifies the result with the system `where.exe`. It does not overwrite the official Codex launcher or modify the system `PATH`.

An already-running VS Code window keeps its inherited environment. To use the freshly installed shim immediately in the current PowerShell process:

```powershell
$Status = csa status | ConvertFrom-Json
$ManagedBin = [string]$Status.activation.managed_bin
$OtherEntries = @($env:PATH -split ';' | Where-Object { $_ -and $_ -ine $ManagedBin })
$env:PATH = (@($ManagedBin) + $OtherEntries) -join ';'
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

Keep the official Codex launcher later on `PATH` so an unplugged shim falls through safely. Fully quit and reopen VS Code after installation; opening only a new integrated terminal still inherits the existing VS Code window environment. `uninstall` and `purge` remove only CSA's managed user-PATH entry.

## Read health and activation state

`status` is the authoritative runtime check:

```powershell
csa status
Get-Command codex -All
codex --version
```

Interactive output leads with the installed, active, and healthy conclusions, then the recorded official Codex version, compatibility ID, resolved `codex` path, activation detail, and any invalidation reason. It exits `0` for every successfully rendered state. Use `csa status --json` for full paths, hashes, runtime files, timestamps, and raw state.

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

`activation.effective` is true only when the shim validates and the current process resolves `codex` to that shim. `activation.command_resolution` and `doctor.command_resolution` show the first resolved executable and whether it is the managed shim.

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

- The current formal patched compatibility is Codex `0.150.1` p9 on Windows x64.
- Its formal acceptance covers an authenticated single-child Native Join. Multi-child Native Join, Ultra runtime behavior, and interactive TUI acceptance are still unverified.
- Manager archives and npm packages are published for five platforms, but no non-Windows patched compatibility is currently published.
- `purge` intentionally leaves official files, user configuration, authentication, and external package-manager state untouched.
