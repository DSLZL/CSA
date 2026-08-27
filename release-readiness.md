# CSA release readiness

Baseline reviewed for this replacement:

```text
CSA commit: b6826649c3dc46ae99699fa92f8e4dcac6357700
Current compatibility: rust-v0.149.0-native-join-p3
Codex version: 0.149.0
Upstream tag: rust-v0.149.0
Upstream commit: 758ef40f50c1a458425c7cfbf1eb12cbc07af0b0
Patch-set version: 6
Patch count: 14
Patched target: x86_64-pc-windows-msvc
Patched product: codex-cli / codex only
```

## Current artifact authority

```text
Manifest SHA-256:
6627d066d6098613d8b67ec478c66453a175808a5d06e441ac8a51ac3ae0ba2c

Accepted codex.exe SHA-256:
e3302a04e8bc6062c5d092692e7d38239986453c599dcdf128fd1d9598f596fd

Accepted codex.exe size:
298215424 bytes

Build-profile SHA-256:
be3194ff9dab2c69914ef0751f700d8d4114a0bd53528bd5b566c61fdf05df13

Runtime-lock SHA-256:
38ee51e00fce99ef5cf91e2be80345262a52b8adf98608e2c4bcd32ffe7a5566
```

The acceptance record at:

```text
release/acceptance/rust-v0.149.0-native-join-p3/x86_64-pc-windows-msvc.json
```

is a reviewed-baseline migration of the artifact identity already present in the p3 manifest and the previous readiness record. It does **not** claim that the replacement workflow has already reproduced or published that artifact in hosted CI.

## Replacement validation status

### Passed locally

Machine-readable totals:

```text
package-only validation: 50 / 50 passed
real-manifest repository overlay: 52 / 52 passed
```


- compatibility catalog unit tests;
- release-asset guard unit tests;
- Python syntax compilation;
- JSON parsing and hash/link validation;
- GitHub/CircleCI YAML parsing with the available parser;
- Bash syntax validation for `build_patched_codex_bundle.sh`;
- workflow static guard: no compatibility ID, upstream commit, or npm SRI authority in CircleCI/formal patched release YAML;
- CircleCI uses the dated `ubuntu-2604:2026.05.1` machine image rather than a moving `current` tag;
- workflow-dispatch and CircleCI selector/target values are passed as environment data rather than interpolated into shell source;
- Manager platform CI discovers `npm pack --json` output paths instead of assuming version `0.1.2`;
- resolver validation against the real p1/p2/p3 manifests from the fixed baseline;
- current p3 resolution with acceptance and release authority required.

### Not verified by this replacement-generation session

- hosted GitHub Actions execution;
- `actionlint` in the repository toolchain;
- CircleCI CLI validation;
- hosted CircleCI candidate compilation;
- cold/warm/near-warm build timings and CircleCI credits;
- full independent p3 binary rebuild;
- exact reproduction of the accepted `codex.exe` SHA-256;
- draft reconciliation against a real GitHub Release;
- formal publication;
- authenticated local Windows acceptance replay.

Do not mark the replacement production-ready until the hosted and build validations above have passed.

## Data-driven release gates

The following gates are mandatory:

- [x] CircleCI YAML contains no static compatibility version matrix.
- [x] One ordinary pipeline request resolves one exact compatibility and target.
- [x] `build_all_compat=true` fails closed instead of compiling all historical versions.
- [x] Historical payloads remain present and immutable.
- [x] Workflow inputs no longer copy official npm integrity or accepted artifact SHA-256.
- [x] Build infrastructure is committed in a reviewed build profile.
- [x] Official runtime identity is committed in a target-specific runtime lock.
- [x] Acceptance identity is committed in a machine-readable record.
- [x] Formal release rebuilds only `codex-cli` / `codex`.
- [x] Formal release does not call manifest `finalize` in temporary staging.
- [x] Draft publication is recoverable and exact-asset guarded.
- [x] Published compatibility Releases are immutable.
- [ ] Hosted CircleCI current build succeeds.
- [ ] Hosted GitHub formal rebuild reproduces the accepted artifact exactly.
- [ ] Draft upload/recovery canary succeeds.
- [ ] Final published asset set passes remote digest verification.

## Cache decision

```text
Cargo download cache: KEEP
Rustup cache: KEEP
cargo-xwin SDK cache: KEEP
Pinned build-tool cache: KEEP
Official runtime archive cache: KEEP
sccache: KEEP
Uncontrolled target/ cache: DO NOT ADD
```

The replacement changes cache key authority, not cache purpose. Keys include reviewed profile/runtime/manifest identities while retaining migration restore prefixes. Cold builds remain valid without any cache.

## Current platform boundary

Only this patched target is currently accepted:

```text
x86_64-pc-windows-msvc
```

CSA Manager remains a separate five-platform product. This document does not claim that patched Codex Linux or macOS targets are built, accepted, or published. Future patched targets must be represented as separate manifest artifacts and separate downloadable executables; they must not be bundled into a single five-platform archive.

## Manager discovery boundary

The new compatibility index is authoritative for CI/release routing. It is not yet a remotely consumed Manager protocol. The current Manager compatibility mapping remains unchanged by this package. A dynamic Manager discovery migration requires a separately reviewed Rust implementation and Manager release.

## Production rollout decision

Status at package generation:

```text
STATIC REPLACEMENT VALIDATION: PASS
HOSTED CI VALIDATION: NOT VERIFIED
FULL PATCHED CODEX REBUILD: NOT VERIFIED
FORMAL RELEASE: NOT VERIFIED
PRODUCTION MERGE: CANARY REQUIRED
```

Use `APPLY_AND_ROLLBACK.md` and run the repository validation before the first canary PR.
