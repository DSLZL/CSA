# Release Readiness

Checked: 2026-08-23

Overall status: **LOCAL WINDOWS P3 HYBRID CANDIDATE PASS; FORMAL PUBLICATION NOT READY OR EXECUTED**

The exact `rust-v0.149.0-native-join-p3` payload now contains 13 ordered patches. The complete TUI/Join base and hybrid install-context contracts remain passed; the added resident-panel polish and Windows mouse-initialization guard then passed exact clean-source replay, focused live-panel and mouse tests, Clippy, release build, strict artifact verification, manager regression tests, release-tool checks, and a user-run interactive PowerShell ConPTY startup. The local CSA `0.1.1` manager/npm candidate also passed the disposable Windows lifecycle. The current patched executable remains installed only in the disposable manager root. No npm package, GitHub Release, tag, signature, production activation, profile, persistent PATH, global npm prefix, official file, or user Codex configuration was created or changed.

## Scope and artifacts

| Artifact | Result |
| --- | --- |
| manager | `csa 0.1.1`, 3,771,392 bytes, SHA-256 `988544a184547fa09276656d3cd6820e7cafa294a450035e90991a7616c2a835` |
| reviewed payload | `payload/codex/rust-v0.149.0-native-join-p3`; patch-set version `5`; 13 ordered patches, 72 present preimages, 8 absence assertions |
| payload manifest | 11,340 bytes, SHA-256 `07f465feceffa5811a8af80a6d18cc594c153e063d068e90328867c74315924c` |
| source hashes | 9,195 bytes, SHA-256 `5d4c89c237a5a726f9db3b8c1046dd1521d586bed2e3d0150d2b690c6f71ba33` |
| test contract | 16 tests, 5,974 bytes, SHA-256 `f40304989ae83356946ab3c1d0419777f96d0ce2662f2ccb38ab782fb8c405bb` |
| patched Codex | `codex-cli 0.149.0`, 299,645,952 bytes, SHA-256 `64badb66f88d0cee23276dd81e26fee3f2a490803a48c9c63bc55bca40b9174d` |
| CSA-owned runtime | manager overlay `runtime/bin/codex.exe` only; no official companion copies or links |
| compatibility identity | upstream `rust-v0.149.0` / `758ef40f50c1a458425c7cfbf1eb12cbc07af0b0`; Rust `1.95.0`; `x86_64-pc-windows-msvc` |

The repository manifest binds this local executable exactly. A formal hosted run must reproduce it from a reviewed clean default-branch commit and independently assemble a checksum-complete compatibility Release before publishing.

## Verification matrix

| Lane | Result | Evidence boundary |
| --- | --- | --- |
| exact upstream/source/preimage/absence/13-patch verification | PASS | clean detached v0.149 source at the exact tag/commit |
| complete TUI/Join base contract | PASS | pre-overlay 11-patch authority: 2 generation + 15 test + 1 release-build steps, all exit 0 |
| complete TUI library | PASS | 3,743 passed, 0 failed, 6 ignored; one test thread; snapshots not updated |
| TUI Clippy | PASS | library/tests with `-D warnings` |
| Native Join integration | PASS | 7 passed, 1 ignored in the deterministic integration target |
| hybrid install-context and TUI polish | PASS | all 13 patches replayed exactly; focused TUI 22 passed; release build exit 0 |
| Windows mouse-mode initialization | PASS | 5 automated mouse tests plus user-run startup in a real PowerShell ConPTY; no initial-console-mode error was reported |
| release artifact absolute execution and strict payload verify | PASS | `codex-cli 0.149.0`; manifest size/SHA-256 independently matched |
| manager format/tests/release build | PASS | 13 unit and 16 manager integration tests; release executable rebuilt |
| Windows npm `0.1.1` candidate | PASS | offline temporary-prefix install/version/doctor/cold install/isolated exec/cold uninstall/npm uninstall; official hashes and parent environment unchanged |
| bare official-runtime discovery | PASS | current Bun launcher auto-bound native, meta package, platform package, marker, host, runner, sandbox helper, and `rg` without explicit official paths |
| install/reinstall/default-config launch/doctor | PASS | disposable manager root; second install unchanged; `features list` and doctor exit 0 with expected runtime/install/helper data; current patched TUI started interactively with the default config |
| overlay ownership | PASS | recursive manager scan found zero copied official companions; patched executable is under manager-owned `runtime/bin` |
| uninstall/fallback/idempotency | PASS | first uninstall removed shim/state/overlay; second changed nothing; official CLI still reports `0.149.0` |
| official runtime immutability | PASS | all eight bound official input hashes matched after install/reinstall/launch/uninstall |
| unsupported p6 | PASS (rejected) | verifier accepts only reviewed patch-set versions `1` through `5` |
| v0.148 source drift | PASS (rejected) | verifier rejected exact commit mismatch |
| Native Join request counts | PASS | `main_model_requests[J,T)=0`, Join result `1`, resume `1`, status polling `0` |
| read-only panel/click/Left/`/subagents` paths | PASS | each emitted `0` Main model operations |
| 8-agent event storm | PASS | 800 routed events retained 24 rows (`8 × Recent 3`), with bounded reducer/app timing |
| frame/input fairness | PASS | 800 frame requests coalesced to 2 draws; queued key and mouse each serviced within 2 polls |
| p1/p2 byte immutability | PASS | aggregate SHA-256 values remain `2c1b077b…`, `04ad7a9b…`, and `75a4a0bf…` |
| root `Cargo.lock` | PASS, unchanged | worktree and HEAD Git blob `85add89356d8b73f56a58dc271359e572150204d` |
| reviewed clean CSA source commit/tag containing p3 | **NOT VERIFIED** | local source is ready for commit/push; no release tag created |
| hosted compatibility Release workflow | **NOT VERIFIED** | configured for exact p3; not dispatched from this worktree |
| hosted manager/Node matrix and current npm tarballs | **NOT VERIFIED** | no hosted jobs or npm candidate assembly in this p3 run |
| Linux x64/arm64 and macOS x64/arm64 manager lanes | **NOT VERIFIED** | CI configuration is not execution evidence |
| POSIX process-group signal behavior | **NOT VERIFIED** | local host is Windows |
| authenticated external-provider p3 lane | **NOT VERIFIED** | request-count E2E used a loopback fake Responses provider |
| 65-second/five-minute authenticated child, approval, and Ctrl+C lanes | **NOT VERIFIED** | not executed for p3 |
| persistent production PATH and interactive TUI rollback | **NOT VERIFIED** | only disposable manager activation and noninteractive default-config commands ran |
| signing | **NOT VERIFIED** | no signing identity supplied |
| npm/GitHub publication | **NOT EXECUTED** | source push and build authorization does not publish packages, Releases, or tags |

## Development isolation

The clean upstream checkout, independent Cargo target, patched binary, manager root, isolated `CODEX_HOME`, cwd, logs, state, npm prefix, and child-only PATH all live in distinct disposable paths under `.dev`. The normal-shim check reused the default Codex configuration by inheritance; no configuration or authentication file was copied or modified.

Official fingerprints remained unchanged:

- launcher: `C:/Users/Long/.bun/bin/codex.exe`, 15,872 bytes, SHA-256 `59b379b53354da72d2c5262119fe70c44b4e473826ebbaa94d47a2d58a359b1a`;
- native executable: `C:/Users/Long/.bun/install/global/node_modules/@openai/codex-win32-x64/vendor/x86_64-pc-windows-msvc/bin/codex.exe`, 297,362,224 bytes, SHA-256 `14b7e6b2356e82d1d9275579eaa588757b4e0a501b65dcc19fccdf77bd83dc00`.

Frozen payload aggregates also remained unchanged:

- `payload/codex/rust-v0.148.0-native-join-p1`: `2c1b077b3880e66904bec82c3e09c7e1669636da5759c779d0839dd3f546324d`;
- `payload/codex/rust-v0.148.0-native-join-p2`: `04ad7a9b78b3dd8e1721d53f187001f1acac9ee3e6b1a3510d96b0f4186c2e44`;
- `payload/codex/native-join-p2`: `75a4a0bf13c2a5afc8a4a97d8f49b4e9fcf6e7ec729ac4f7fa19f595b93c9c3a`.

## Remaining release conditions

1. Commit and push the exact p3 payload, manager/catalog selection, workflows, release hash guard, tests, and documentation as one clean authority change, then require hosted build gates to pass.
2. Rebuild from that clean default-branch commit and require the Windows executable to reproduce SHA-256 `64badb66f88d0cee23276dd81e26fee3f2a490803a48c9c63bc55bca40b9174d`.
3. Run the hosted compatibility workflow with the exact official Windows npm integrity and locally accepted executable hash; inspect the staged manifest, descriptor, and all Release assets before any publication job.
4. Run the hosted manager/Node/platform matrix and assemble a separate manager candidate before advertising or publishing manager/npm artifacts.
5. Keep non-Windows, POSIX signal, authenticated external-provider, long-duration, signing, and production activation lanes `NOT VERIFIED` until each is actually run.

The compatibility update, publication, production plug, and rollback commands are in [docs/release.md](docs/release.md). Operations and uninstall recovery are in [docs/operations.md](docs/operations.md).
