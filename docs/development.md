# Development and Trellis Isolation

## Two-instance rule

```text
Official Codex control plane
  -> reads Trellis task/spec context
  -> edits this repository and launches checks
  -> invokes the SUT by an explicit path

Patched Codex SUT
  -> runs in a disposable fixture
  -> uses a distinct CODEX_HOME, cwd, logs, state, npm prefix, and PATH
  -> never replaces or controls the official process
```

The two instances must not write the same working tree. A writable patched E2E uses a copied fixture or disposable worktree.

## Automated lane

The Windows distribution harness is the final default E2E. It:

- creates a throwaway Git/Trellis fixture and session-scoped task pointer;
- validates implement/check manifests and recovers active task/session context;
- installs local npm tarballs offline into a temporary prefix;
- runs doctor, cold install, status, and isolated patched `--version`;
- resolves the plugged shim only in a child process whose temporary PATH starts with manager `bin`;
- runs cold uninstall and proves a new child resolves to the official launcher;
- uninstalls npm packages, removes the fixture, and compares parent/profile/npm/official invariants.

```powershell
$TempRoot = Join-Path $env:TEMP ("csa-e2e-" + [Guid]::NewGuid().ToString('N'))
powershell -NoProfile -File .\scripts\test_npm_distribution_windows.ps1 `
  -MetaTarball C:\absolute\dslzl-csa-0.1.2.tgz `
  -PlatformTarball C:\absolute\dslzl-csa-win32-x64-0.1.2.tgz `
  -TempRoot $TempRoot `
  -OutputPath C:\existing-directory\trellis-e2e.json `
  -Official C:\absolute\official\codex.exe `
  -OfficialNative C:\absolute\official-native\codex.exe `
  -Manifest C:\absolute\manifest.toml `
  -Artifact C:\absolute\patched\codex.exe `
  -TrellisSource C:\absolute\path\to\.trellis
```

The deterministic Rust integration suite uses `FakeRunner` to cover local install/uninstall, prepare, exec, drift, lock, activation, fallback, interruption recovery, persistent compatibility payloads, and purge without credentials or network access. Pure online-contract tests cover exact formal tag parsing and strict checksums. `scripts/test_release_tools.py` separately exercises upstream state classification, compatibility porting, flat Release packing, and blocker Issue rendering without creating a real Issue, PR, tag, or Release.

## Trellis workflow

For each compatibility or release change:

1. The official control plane selects and starts the Trellis task.
2. Read `.trellis/spec/csa/` before implementation.
3. Use a research/implement/check context manifest that references specs and task evidence, not mutable production code.
4. Run patched Codex only through an absolute path or `exec --isolated` in a disposable fixture.
5. Record machine-readable evidence and rehash official files.
6. Run the Trellis validator and quality gates.
7. Archive with `task.py archive <task> --no-commit` only after all required lanes are honestly classified.

Live Trellis research/implement/check delegation is allowed only when project policy explicitly permits subagents and independent worktrees are available. The automated lane validates the task-context path; live child delegation remains an explicit manual lane.

## Native Join runtime evidence

The isolated fake-provider lane already passed:

- a 65-second native Join with one Join call, zero polling, one result, and one continuation;
- a five-minute pending Join with no parent sampling during the wait;
- approval approve and reject paths;
- observer cancellation, steer, exact rejoin, and cached replay;
- TUI Ctrl+C behavior;
- bounded parent/session shutdown with no orphan child.

These are real patched CLI runs with an isolated fake loopback provider, not authenticated production-provider runs.

## Authenticated manual lane

Run this only when an operator supplies credentials and authorizes the test:

1. Create a fresh `CODEX_HOME` outside the repository. Do not copy the default home or auth files.
2. Log in explicitly inside that isolated home.
3. Prepare a disposable Trellis fixture/worktree and separate logs/state/npm directories.
4. Start patched Codex by the manager's absolute `exec --isolated` path.
5. Run a child task lasting at least 65 seconds and record one Join, zero wait/status polling, one terminal result, and one continuation.
6. Exercise one approval allow and one approval reject path.
7. Start another child, press Ctrl+C, and confirm the shell remains usable.
8. Shut down the parent and confirm no patched or child process remains.
9. Sanitize evidence, delete the isolated home when no longer needed, and compare official hashes.

Status for this repository: **not executed**. No real authentication was copied or used.

## Development kill gates

Stop immediately if any test would:

- replace or modify official Codex;
- reuse the default `CODEX_HOME` or shared writable working tree;
- persist PATH/profile changes without explicit authorization;
- invoke a bare `codex` where an absolute SUT path is required;
- hide an unverified lane as passing;
- place authentication or full environment data in evidence.
