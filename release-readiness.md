# Release Readiness

Checked: 2026-08-18

Overall status: **NOT READY**

The CSA identity, Windows package path, local `csa install/uninstall` lifecycle, exact Codex `0.147.0` payload, reversible activation, online Release contract fixtures, hourly watcher tooling, and isolated Trellis/npm E2E pass locally. Manager and patched-Codex publication are separate `vX.Y.Z` and `compat-<compat_id>` streams. Release is blocked by unexecuted hosted CI/platform lanes, unavailable signing, unexecuted live formal-Release install, unexecuted authenticated/live-agent manual lanes, unexecuted production plug, and no publication execution authorization.

## Scope and artifacts

| Artifact | Result |
| --- | --- |
| manager version | `csa 0.1.0` |
| Windows manager | `target/release/csa.exe`, SHA-256 `1D7C4BAD23762A99D1F8EFD255ACBF3CB15F2A9440417759F7D7743D75C55675` |
| meta npm tarball | `dslzl-csa-0.1.0.tgz`, SHA-256 `658378BBCDE9D8375C235DA1415111DABCAB9370BB87E3DF54C58367D8D84AD2` |
| Windows npm tarball | `dslzl-csa-win32-x64-0.1.0.tgz`, SHA-256 `749AC05F18CC31497FB894BBE32FE7A3FFB3D0EEA4FC6A6CAC75E528548A45F1` |
| patched Codex | `codex-cli 0.147.0`, 299,944,448 bytes, SHA-256 `C2DD9740354DE90E18600D1EFC242DB19E1D9832D0DCA72906D8E6F5E44F4C0A` |
| compatibility | `rust-v0.147.0-native-join-p1`, upstream commit `be6e8eac029b183056b7e4402879f15d2c85f61b` |
| local validation staging | kept outside Git; combined local evidence only, not a publishable Release |

A future `vX.Y.Z` manager candidate's `provenance.json`, `release-readiness.json`, `source-manifest.json`, dependency inventory, and `SHA256SUMS` will be the authority for manager file counts and hashes. A separate `compat-<compat_id>` descriptor and checksum set will be authoritative for patched Codex.

## Verification matrix

| Lane | Result | Evidence boundary |
| --- | --- | --- |
| Rust format, Clippy `-D warnings`, tests, release build | PASS | manager workspace |
| fake runner manager/activation flow | PASS | deterministic `tests/manager_core.rs` |
| strict payload verifier and negative cases | PASS | exact manifest/preimage/patch/artifact contract |
| online stable tag/checksum/digest/descriptor validation | PASS | deterministic Rust/Python contract fixtures; no live asset download |
| compatibility port/flat pack/blocker state tooling | PASS | `scripts/test_release_tools.py` fixtures |
| hourly watcher workflow | configured; not remotely executed | top of hour, temp-only upstream source, `windows-2025`, reviewed PR gate, global blocker Issue |
| exact upstream drift check | PASS, unchanged | tag `rust-v0.147.0`, all five patches preflight |
| native Join 65-second and five-minute fake-provider runs | PASS | one Join, zero polling, one result, one continuation |
| approval, cancellation/rejoin/replay, Ctrl+C, shutdown | PASS | isolated patched CLI/test lanes |
| throwaway Trellis context create/start/validate/recover | PASS | disposable Git/Trellis fixture |
| offline temp-prefix npm install/doctor/`csa install`/status | PASS | Windows x64 |
| child-only PATH after cold install | PASS | child resolved managed shim |
| isolated packaged exec and `csa uninstall` | PASS | Windows x64 |
| child-only PATH after cold uninstall | PASS | child resolved official launcher |
| npm uninstall and cleanup | PASS | Windows x64 |
| official launcher/native hashes and parent configuration | PASS, unchanged | before/after automated E2E |
| Node 22/24/26 compatibility jobs | configured; hosted execution not run from this worktree | GitHub Actions |
| Linux x64 native manager/npm | NOT VERIFIED | CI configured only; no patched payload |
| Linux arm64 native manager/npm | NOT VERIFIED | CI configured only; no patched payload |
| macOS x64 native manager/npm | NOT VERIFIED | CI configured only; no patched payload |
| macOS arm64 native manager/npm | NOT VERIFIED | CI configured only; no patched payload |
| POSIX process-group signal | NOT VERIFIED locally | requires POSIX CI |
| real authenticated 65-second/approval/Ctrl+C lane | NOT EXECUTED | no real auth copied or supplied |
| live Trellis research/implement/check child delegation | NOT EXECUTED | context path passed; live delegation remains a manual lane |
| persistent production plug and interactive smoke | NOT EXECUTED | explicit user authorization required |
| signing | NOT AVAILABLE | no signing identity supplied |
| npm publication / GitHub Release | NOT EXECUTED | not authorized |
| live bare `csa install` from a formal compatibility Release | NOT EXECUTED | no formal CSA compatibility Release was created during local validation |

## Development isolation

Automated lanes used a disposable HOME, `CODEX_HOME`, npm prefix/cache/config, manager root, fixture cwd, logs/state, Trellis session identity, and child-only PATH. Cleanup completed. No profile, persistent PATH, npm global prefix, official installation, or default authentication state was changed.

Official checkpoint hashes remained:

- launcher: `59B379B53354DA72D2C5262119FE70C44B4E473826EBBAA94D47A2D58A359B1A`
- native executable: `935A1911ED2556E4FFCEC995F4886AC2AC425863BA26FED264DF62E30272AD9D`

## Release blockers

1. Use a reviewed clean source commit and one annotated tag for any release.
2. Run the hosted Node matrix and all five native platform jobs; provide real manager/npm artifacts for every advertised platform.
3. Add and verify patched payloads for any non-Windows platform advertised as Native Join capable.
4. Run POSIX signal checks on POSIX hosts.
5. Decide whether real authenticated and live Trellis child-agent lanes are mandatory; if so, authorize and execute them with isolated credentials/worktrees.
6. Provide and verify the required signing identity and signatures.
7. Explicitly authorize and pass the production plug/rollback smoke.
8. Explicitly authorize npm publication and `vX.Y.Z` manager GitHub Release creation; compatibility `compat-*` automation becomes active only after this reviewed workflow is committed and pushed.

## Release and rollback

The complete compatibility update, candidate assembly, publication order, production plug, and rollback commands are in [docs/release.md](docs/release.md). Operations and uninstall recovery are in [docs/operations.md](docs/operations.md).

No package publication, signing, persistent activation, profile edit, global install, auth copy, release tag, or GitHub Release is represented as complete here.
