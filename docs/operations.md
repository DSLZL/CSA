# Operations Guide

## Identity first

Before `install`, `prepare`, or activation, record all four identities:

1. Official launcher absolute path, version, size, and SHA-256.
2. Official native executable absolute path, version, size, and SHA-256.
3. Manager absolute path, version, size, and SHA-256.
4. Compatibility Release tag/source commit, manifest, target, asset sizes, and SHA-256 values. In local diagnostic mode, record the explicit manifest/artifact paths instead.

The official launcher, official native executable, manager shim, and patched artifact must be distinct files. The manager rejects unsafe overlap.

## Command reference

| Command | Effect |
| --- | --- |
| `doctor` | Verifies official identity and compatibility inputs without preparing state. |
| `install` | With no release input, resolves the current official stable version and downloads the exact formal `dslzl/CSA` compatibility Release; with `--manifest` plus one artifact/source, stays local-only. It then prepares and publishes the managed shim without editing PATH. |
| `uninstall` | Withdraws the shim, then removes manager-owned preparation data. Repeated calls are safe. |
| `prepare` | Validates or builds the exact patched artifact and atomically records prepared state. |
| `status` | Reports prepared/activation health and drift. |
| `exec --isolated` | Runs the prepared artifact with explicit isolated directories and records evidence. |
| `plug` | Publishes a checksum-bound shim inside the manager-owned `bin` directory. It does not edit PATH. |
| `unplug` | Withdraws the shim. Repeated calls are safe. |
| `purge` | Unplugs and removes owned prepared/source/build/state data. It does not remove official Codex or npm packages. |

Always pass an absolute normalized `--manager-root` in automation. This prevents a test from using the default per-user data location by accident.

Online install is deliberately strict. The installed official version, OpenAI's current latest formal release, the compatibility manifest, and the CSA Release provenance must all identify the same version/tag/commit. Missing support returns `latest_not_yet_supported`; malformed releases, unexpected redirects, extra/missing assets, or checksum drift fail closed. There is no fallback to an older CSA payload.

## PATH setup

The manager never edits PATH. Put `<manager-root>/bin` before the official npm bin only after `install` (or lower-level `prepare` plus `plug`), and never remove the official entry.

Current PowerShell process:

```powershell
$env:PATH = (Join-Path $ManagerRoot 'bin') + [IO.Path]::PathSeparator + $env:PATH
```

Current `cmd.exe` process:

```bat
set "PATH=C:\absolute\manager-root\bin;%PATH%"
```

Current Bash/Zsh process:

```bash
export PATH="/absolute/manager-root/bin:$PATH"
```

Current Fish process:

```fish
fish_add_path --prepend /absolute/manager-root/bin
```

For a persistent user setup, use the operating system's user PATH editor or the shell's normal profile mechanism. Add exactly one manager `bin` entry. Do not use `setx`, which can rewrite or truncate PATH, and do not automate profile edits. Linux and macOS commands document the intended manager behavior; those native package lanes are not yet verified.

## Health checks

When unplugged, command resolution must reach the official launcher. When plugged, it must reach the manager shim. In both cases the supported binary reports `codex-cli 0.148.0`.

```powershell
Get-Command codex -All
codex --version
& $Manager status --manager-root $ManagerRoot
```

Rehash official files after every smoke test. An official hash change is a hard failure, not a condition to repair in place.

## Failure and recovery

Use this order for every incident:

1. Run `unplug` by the manager's absolute path.
2. Open a new shell and confirm `codex` resolves to the official launcher.
3. If it still resolves to the managed directory, remove only that PATH entry and open another shell.
4. Run `status` and preserve its JSON for diagnosis.
5. Run `uninstall` only after official fallback works.
6. Rehash the official launcher and native executable.
7. Uninstall the npm packages last.

Interrupted plug/unplug state is recovered on the next manager command. If artifact, state, or shim validation fails at invocation time, the shim selects the official executable instead of an unverified patched artifact.

Online download failures remove the per-attempt staging directory. A completed prepare stores its small compatibility payload under the manager root, so later `status`, shim validation, and `uninstall` do not depend on a temporary download path.

## Uninstall

```powershell
& $Manager uninstall --manager-root $ManagerRoot
npm uninstall --prefix $Prefix @dslzl/csa @dslzl/csa-win32-x64
```

For a future global installation, use `npm uninstall -g @dslzl/csa @dslzl/csa-win32-x64` only after official fallback is confirmed. Remove a persistent manager PATH entry separately.

## Authentication and data

`exec --isolated` does not copy credentials. A real authenticated run needs a separately created `CODEX_HOME` whose login is performed explicitly by the operator. Never seed it from the default `CODEX_HOME` in automation.

Evidence may contain paths, versions, hashes, timestamps, and exit results. It must not contain tokens, cookies, authorization headers, auth files, or full environment dumps. Test directories should be disposable and outside the repository when they may contain auth or session data.

## Known limitations

- Codex `0.148.0` on Windows x64 has a focused-verified current candidate; its Release workflow must pass the complete contract before publication.
- Non-Windows manager/npm jobs exist but have not produced verified release artifacts.
- POSIX process-group signal behavior is covered by the launcher test only when run on POSIX; it is not verified by the local Windows run.
- Extended authenticated real-provider lanes and production persistent activation remain manual gates; only the basic spawn/Join smoke has run.
- `purge` removes only manager-owned data; it intentionally leaves external packages and official files alone.
