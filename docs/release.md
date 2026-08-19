# Compatibility and Release Runbook

## Current compatibility

The current local compatibility candidate is:

| Field | Value |
| --- | --- |
| compatibility ID | `rust-v0.148.0-native-join-p1` |
| Codex version/tag | `0.148.0` / `rust-v0.148.0` |
| upstream commit | `3ba0f711642a888aec92a611a3f3b2211157ff89` |
| Rust toolchain | `1.95.0` |
| target | `x86_64-pc-windows-msvc` |
| patched artifact SHA-256 | `795930548B858AAE020B26C7C90464C5DD27C9B83CA0315BC78A072897747D6F` |

Do not retarget this payload in place. A new Codex tag or changed preimage requires a new compatibility ID, updated expected hashes, a new ordered patch set, and the full gate matrix.

The added `join_agent` remains in the default function namespace. OpenAI-compatible providers reject unrecognized additions to the reserved `collaboration` namespace; the upstream tools remain there unchanged.

## Compatibility update

1. Select an exact upstream release tag and resolve its immutable commit.
2. Create a clean checkout at that commit. Do not work from a moving branch.
3. Compare every manifest preimage and absent-path assertion using Git blob bytes.
4. Port the smallest patch set; keep each layer independently reviewable.
5. Update generated schemas and exact contract fixtures where the protocol changes.
6. Run the drift audit, strict patch contract, negative tests, patched runtime gates, and stock comparison.
7. Build each declared target and bind the resulting filename, size, and SHA-256 in a new manifest.
8. Add support-matrix entries only for lanes that have build, package, installation, execution, and signal evidence.

Core commands:

```text
python scripts/compatibility_audit.py drift --manifest <manifest> --source <clean-source> --tag <exact-tag> --output <drift.json>
python scripts/run_patch_contract.py --manifest <manifest> --source <clean-source> --cargo-target <new-target-dir> --output <evidence-dir>
python scripts/verify_patch_payload.py <payload-dir>
```

## Hourly upstream watcher

`.github/workflows/watch-codex-release.yml` runs at the top of every hour (`0 * * * *`) and supports manual dispatch. Its read-only detect job resolves `openai/codex`'s current non-draft, non-prerelease `rust-vX.Y.Z` Release and peels its tag to the exact commit.

The state machine is:

```text
formal compat Release exists -> no-op
open upstream-patch-blocked Issue -> update that same Issue to the newest target; run no patch
reviewed compatibility entry is on the default branch -> rebuild and publish formal compat Release
open automation candidate PR exists -> no-op
otherwise -> port/test/build on windows-2025 and open one review candidate PR
```

The workflow reuses `scripts/compat_release.py`, `run_patch_contract.py`, and `verify_patch_payload.py`. Both build paths clone only the detected tag into `RUNNER_TEMP` and reject any source path inside `GITHUB_WORKSPACE`; upstream Codex source is never copied into the CSA repository. It never performs a fuzzy or three-way repair. A failed patch hunk, generation step, test, build, artifact binding, or candidate step creates or updates the single open Issue labeled `upstream-patch-blocked`, including the latest upstream tag/commit, failed stage, run URL, captured failure, and reproduction direction. While that Issue remains open, later upstream versions do not start another automatic patch. The human repair PR must target the then-current latest stable release and contain `Fixes #<blocker>`.

Passing automation creates a PR and review artifact only. No formal compatibility Release is published from the candidate branch. After review and merge, a later hourly run rebuilds the exact default-branch entry and may publish it.

The helper commands used by the workflow are also runnable manually:

```text
python scripts/compat_release.py detect --repository <repo> --output <new-json>
python scripts/compat_release.py port --base-manifest <manifest> --source <clean-source> --tag <tag> --commit <commit> --output <new-entry>
python scripts/compat_release.py finalize --manifest <new-manifest> --artifact <patched-binary>
python scripts/compat_release.py pack --manifest <manifest> --artifact <patched-binary> --source-commit <csa-commit> --output <new-flat-assets>
```

Each formal compatibility Release uses tag `compat-<compat_id>` and contains only flat assets: the manifest, every referenced small payload file, the patched target artifact, `compatibility-release.json` provenance, and `SHA256SUMS`. Runtime install requires the Release tag commit, provenance, target, filenames, sizes, and SHA-256 values to agree.

## Two GitHub Release streams

| Stream | Tag | Managed assets |
| --- | --- | --- |
| CSA manager | `vX.Y.Z` | `csa` native binaries, npm tarballs, manager evidence/provenance, source bundle, and checksums |
| Patched Codex | `compat-<compat_id>` | compatibility manifest/payload, patched `codex` binary, compatibility provenance, and checksums |

The streams never share managed assets. `csa install` reads only the `compat-*` stream; publishing a new patched Codex does not require a new manager version. GitHub's automatic source archives for either tag still represent the CSA repository and are not compatibility download assets.

## Manager and npm artifacts

Build with the pinned toolchain, then stage packages only into a new directory:

```text
cargo +1.95.0 build --locked --release
node scripts/stage_npm_packages.mjs --out <new-absolute-stage> --binary win32-x64=<absolute-manager.exe>
npm pack <stage/platforms/win32-x64> --pack-destination <tarball-dir>
npm pack <stage/meta> --pack-destination <tarball-dir>
```

Each platform package must contain only its manifest, manager binary, README, license, and notices. The meta package must contain no manager binary or lifecycle script. Repacking identical staged inputs must preserve the expected file set and binary binding.

## CI and candidate assembly

The ordinary `.github/workflows/ci.yml` has read-only repository permissions, SHA-pinned actions, Node 22/24/26 checks, and five native manager/npm lanes. The separate upstream watcher grants write permissions only to the candidate PR, blocker Issue, and merged-default-branch compatibility publication jobs that need them. Hosted results must be downloaded and inspected before a manager/npm release; a configured matrix is not evidence that a lane passed.

After all lane artifacts are present:

```text
python scripts/ci_release.py input --repository . --artifacts <downloaded-artifacts> --output <release-input.json> --revision <clean-commit> --ref <tag> --repository-url <url>
python scripts/assemble_release_candidate.py --repository . --input <release-input.json> --output <new-candidate-dir>
```

Manager candidate assembly is atomic and refuses an existing output path. It has no patched-Codex input and emits no top-level `payload/`, `patched/`, or compatibility descriptor. Verify every `SHA256SUMS` entry, source provenance, dependency inventory, npm tarball, and platform evidence. Assembly must report `ready`, not merely finish successfully.

## Publication gate

Do not publish until all declared platform lanes pass, source is a clean commit/tag, required signatures exist, checksums match, and manual gates have explicit results.

Publish every scoped platform package first and the meta package last:

```text
npm publish <platform-package.tgz> --access public --provenance
npm publish <next-platform-package.tgz> --access public --provenance
npm publish <dslzl-csa-0.1.0.tgz> --access public --provenance
```

Then create an annotated `v<manager-version>` tag and GitHub Release from the same clean commit, attach only the manager/npm candidate artifacts and `SHA256SUMS`, and record registry/release URLs in provenance. Never attach compatibility payloads or patched Codex binaries to this Release. Never reuse a version after a partial publication; finish missing packages only when their bytes match the approved candidate, otherwise increment the version.

No npm publish, GitHub Release, signing, or tag is represented as complete by this repository state.

## Production plug smoke

This lane changes user command resolution and requires explicit authorization:

1. Record official launcher/native and manager identities and hashes.
2. Install the approved npm package, run `doctor`, then run bare `csa install` against the reviewed formal compatibility Release and require healthy `status`. Record the resolved OpenAI tag/commit and CSA Release checksums. The explicit local artifact mode remains available for comparison and diagnosis.
3. Add the manager `bin` once before the official npm bin using the chosen user PATH mechanism. Do not use `setx`; keep the official entry.
4. While unplugged, open a new shell and prove `codex` resolves to official.
5. Open another shell, prove resolution reaches the checksum-matched shim published by `install`, and verify the version.
6. In disposable cwd and `CODEX_HOME`, run an interactive command and test Ctrl+C, shell usability, and orphan-free shutdown.
7. Run `uninstall` twice and prove both are safe. Open a new shell and prove official fallback.
8. Rehash official files and preserve sanitized evidence.

Rollback: run `unplug`; if resolution still reaches the manager directory, remove only that PATH entry and open a new shell. Run `uninstall` only after official fallback works. Do not delete official Codex or user auth/session data.

Status for this repository: **not executed**.

## Required release evidence

- clean source commit and annotated tag;
- exact compatibility drift and patch-contract results;
- one native manager/npm result for every declared platform;
- patched payload results for every advertised compatibility target;
- deterministic candidate manifest and complete checksums;
- signing status and signature artifacts when required;
- authenticated/manual, production plug, and rollback results classified as pass, fail, or not verified;
- proof that official hashes and persistent user configuration remained unchanged during automation.
