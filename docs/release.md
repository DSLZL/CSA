# Releasing CSA

CSA publishes two products from the same repository. They use separate tags, workflows, assets, and authority.

| Product | Tag | Workflow | Output |
| --- | --- | --- | --- |
| CSA Manager | `vX.Y.Z` | `release-csa.yml`, then `publish-npm.yml` | Five Manager archives and six npm packages |
| Patched Codex | `compat-<compat_id>` | `release-patched-codex.yml` | Every manifest target and one exact compatibility payload |

GitHub Actions is the release authority. Local builds and candidate artifacts are evidence, not permission to publish.

## Current release snapshot

The current Manager release is [`v0.1.6`](https://github.com/DSLZL/CSA/releases/tag/v0.1.6), and the npm meta package is [`@dslzl/csa@0.1.6`](https://www.npmjs.com/package/@dslzl/csa).

The current formal patched Release is [`compat-rust-v0.151.0-native-join-p10`](https://github.com/DSLZL/CSA/releases/tag/compat-rust-v0.151.0-native-join-p10).

| Field | Value |
| --- | --- |
| Compatibility ID | `rust-v0.151.0-native-join-p10` |
| Codex version | `0.151.0` |
| Upstream tag | `rust-v0.151.0` |
| Upstream commit | `78c290807ce710180111df227df3b7a4fe845452` |
| Patch revision | `10` |
| Patch files | `5` |
| Rust toolchain | `1.95.0` |
| Native targets | Windows x64/arm64, Linux x64/arm64 musl, macOS x64/arm64 |
| Published Windows x64 SHA-256 | `91552915db89f64621c309c4dabfead4fe46725a253b9860958a4a6a07e1f8f3` |
| Published Windows x64 size | `314201088` bytes |

The committed compatibility manifest intentionally keeps placeholder production hashes and sizes. The formal workflow finalizes a temporary copy from its own native builds. The published manifest, descriptor, and checksums define production identity.

The Windows acceptance record is [`release/acceptance/rust-v0.151.0-native-join-p10/x86_64-pc-windows-msvc.json`](../release/acceptance/rust-v0.151.0-native-join-p10/x86_64-pc-windows-msvc.json). It binds the separately tested development candidate, not the independently rebuilt production executable.

## Compatibility authority

Compatibility data flows through reviewed files:

```text
release/compatibility-index.json
        | routing, lifecycle, and enablement
        v
compatibility manifest
        | upstream, patches, targets, and artifact contract
        +------------------+
        v                  v
build profile         runtime lock
        |                  |
        +---------+--------+
                  v
       disposable candidate build
                  |
                  v
       sanitized local acceptance
                  |
                  v
       committed acceptance record
                  |
                  v
       annotated compatibility tag
                  |
                  v
       same-commit Patch Validation
                  |
                  v
       independent native rebuilds
                  |
                  v
       temporary manifest finalization
                  |
                  v
       exact draft reconciliation
                  |
                  v
       formal GitHub Release
```

`release/compatibility-index.json` routes a selector and hash-binds the manifest, build profile, runtime lock, and optional acceptance record. The compatibility manifest owns upstream identity, the full target set, payload, and artifact locations. A published Release owns the finalized production sizes and hashes.

Workflow YAML must not contain compatibility IDs, upstream commits, accepted executable hashes, npm integrity values, or runtime package identities.

## Compatibility lifecycle

| Lifecycle | Meaning |
| --- | --- |
| `legacy` | Kept for static validation; formal release disabled |
| `candidate` | Eligible for development builds; formal release disabled |
| `accepted` | Sanitized local acceptance is committed; formal release may be enabled |

Historical payloads and old patch-family bytes are immutable. Upstream drift creates a new exact entry or family binding.

## Release the Manager

### 1. Align the version

Update the same version in:

- `Cargo.toml` and the Manager package entry in `Cargo.lock`;
- `release/support-matrix.json`;
- `npm/meta/package.json`, including every optional platform dependency;
- current release examples and badges in user documentation.

Search for both plain and escaped old-version literals. The release workflow rejects a request when the Cargo, support matrix, and npm meta versions differ.

### 2. Run the local gate

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --all-targets
node --check npm\meta\bin\csa.js
node --check scripts\stage_npm_packages.mjs
node scripts\test_npm_launcher.mjs
py -3 scripts\test_release_tools.py
```

Commit and push the reviewed version change to the default branch.

### 3. Dispatch the Manager release

Run `Release CSA` from the default branch:

```text
version=0.1.6
```

The workflow:

1. verifies the requested version and source branch;
2. runs the Rust, npm, and release-tool gates;
3. builds and smoke-tests five native Manager binaries;
4. creates five archives;
5. stages and tests npm packages in temporary prefixes;
6. requires the exact archive and tarball inventory;
7. creates `SHA256SUMS`;
8. creates an annotated `vX.Y.Z` tag and GitHub Release;
9. explicitly dispatches npm Trusted Publishing for that tag.

The `v0.1.6` asset set is:

```text
csa-v0.1.6-windows-x86_64.zip
csa-v0.1.6-linux-x86_64.tar.gz
csa-v0.1.6-linux-aarch64.tar.gz
csa-v0.1.6-macos-x86_64.tar.gz
csa-v0.1.6-macos-aarch64.tar.gz
dslzl-csa-0.1.6.tgz
dslzl-csa-win32-x64-0.1.6.tgz
dslzl-csa-linux-x64-0.1.6.tgz
dslzl-csa-linux-arm64-0.1.6.tgz
dslzl-csa-darwin-x64-0.1.6.tgz
dslzl-csa-darwin-arm64-0.1.6.tgz
SHA256SUMS
```

An existing Manager Release is not overwritten.

## Publish npm packages

Each package must be bound once to the repository and exact case-sensitive workflow filename:

```powershell
npm trust github @dslzl/csa-win32-x64 --repo DSLZL/CSA --file publish-npm.yml --allow-publish --yes
npm trust github @dslzl/csa-linux-x64 --repo DSLZL/CSA --file publish-npm.yml --allow-publish --yes
npm trust github @dslzl/csa-linux-arm64 --repo DSLZL/CSA --file publish-npm.yml --allow-publish --yes
npm trust github @dslzl/csa-darwin-x64 --repo DSLZL/CSA --file publish-npm.yml --allow-publish --yes
npm trust github @dslzl/csa-darwin-arm64 --repo DSLZL/CSA --file publish-npm.yml --allow-publish --yes
npm trust github @dslzl/csa --repo DSLZL/CSA --file publish-npm.yml --allow-publish --yes
```

This bootstrap requires a maintainer npm login once. Later releases use GitHub OIDC and do not require a local npm login, OTP, `NPM_TOKEN`, or `NODE_AUTH_TOKEN`.

`publish-npm.yml` accepts:

- a formal, non-prerelease Manager Release `published` event;
- a manual exact `vX.Y.Z` tag for recovery.

It downloads the Release instead of packing the workspace again. Before publication, it requires the exact 12-asset inventory, validates checksum coverage and contents, and checks each tarball name and version.

The five platform packages publish before `@dslzl/csa`. A rerun skips an existing version only when registry SRI matches the exact Release tarball. This allows recovery after a partial publication without overwriting npm's immutable versions.

To resume an existing formal Release:

```powershell
gh workflow run publish-npm.yml --ref main -f tag=v0.1.6
```

Verify the registry and launcher:

```powershell
npm view @dslzl/csa version dist-tags.latest --registry=https://registry.npmjs.org
npx --yes @dslzl/csa@0.1.6 --version
```

The exact immutable `0.1.4` recovery path is the only exception that disables the Sigstore provenance statement because those old tarballs lack `repository.url`. It still uses OIDC.

## Validate compatibility data

Run the catalog and workflow guard:

```powershell
py -3 scripts\compat_catalog.py validate `
  --repository . `
  --workflow .github\workflows\build-patched-codex-target.yml `
  --workflow .github\workflows\build-patched-codex-windows.yml `
  --workflow .github\workflows\validate-patched-codex.yml `
  --workflow .github\workflows\release-patched-codex.yml
```

Resolve the canonical Windows route:

```powershell
py -3 scripts\compat_catalog.py resolve `
  --repository . `
  --selector current `
  --target x86_64-pc-windows-msvc `
  --output $env:TEMP\csa-compat-resolution.json
```

Unknown selectors, unsupported targets, hash drift, disabled flags, and missing required authority fail before source clone or toolchain setup.

Patch Validation runs on `windows-2025`. It checks out the exact CSA commit, clones the exact reviewed upstream tag outside the repository, verifies the official runtime archive, applies the complete patch contract, runs generation and tests, and uploads machine-readable evidence.

It can be called by formal Release or dispatched manually:

```powershell
gh workflow run validate-patched-codex.yml `
  --ref main `
  -f compat_selector=<exact-compatibility-id> `
  -f target=x86_64-pc-windows-msvc
```

Ordinary pushes, pull requests, and Windows development builds do not trigger Patch Validation.

## Build a Windows development candidate

For routine testing, use the Windows-only workflow from any pushed ref:

```powershell
gh workflow run build-patched-codex-windows.yml `
  --ref <branch-or-tag> `
  -f compat_selector=<exact-candidate-id>
```

It calls the same reusable native target workflow used by formal release, builds only `x86_64-pc-windows-msvc`, and uploads:

```text
bundle/bin/codex.exe
bundle/target-record.json
candidate-record.json
resolution.json
```

The artifact is named `patched-codex-windows-test-<compat_id>` and is retained for 14 days.

Download it:

```powershell
gh run download <run-id> `
  -n patched-codex-windows-test-<compat_id> `
  -D .dev\windows-test-candidate
```

This workflow does not run Patch Validation, build other targets, finalize a manifest, or publish.

## Run a multi-platform preflight

Dispatch `Release patched Codex CLI` from the default branch with publication disabled:

```text
compat_selector=<exact-candidate-id>
target=x86_64-pc-windows-msvc
publish=false
replace_published=false
```

The workflow runs same-commit Patch Validation, then builds every target declared by the manifest:

```text
aarch64-apple-darwin
x86_64-apple-darwin
aarch64-unknown-linux-musl
x86_64-unknown-linux-musl
aarch64-pc-windows-msvc
x86_64-pc-windows-msvc
```

The 14-day `patched-codex-acceptance-<compat_id>` artifact contains the canonical Windows acceptance inputs plus every native target:

```text
bundle/
candidate-record.json
resolution.json
targets/
matrix.json
```

This is build evidence, not acceptance.

## Accept a candidate

Download the exact candidate and test it on Windows with an isolated `CODEX_HOME`, fixture, logs, state, npm prefix, Manager root, and child-only `PATH`. Keep official Codex unchanged.

Bind the sanitized evidence:

```powershell
$CompatId = 'rust-vX.Y.Z-native-join-pN'
$Target = 'x86_64-pc-windows-msvc'

py -3 scripts\compat_catalog.py accept `
  --repository . `
  --selector $CompatId `
  --target $Target `
  --candidate-record .\candidate-record.json `
  --artifact .\bundle\bin\codex.exe `
  --acceptance "release\acceptance\$CompatId\$Target.json" `
  --evidence .\sanitized-acceptance-evidence.json `
  --make-current
```

`accept` requires an exact candidate record, artifact, manifest, build profile, runtime lock, and evidence file. Review the acceptance record and compatibility-index change together.

Evidence must distinguish passed and unverified lanes. It must not contain credentials, authentication files, tokens, sessions, or private environment dumps.

Commit and push the accepted state before formal publication.

## Publish a patched Codex Release

After accepted state is on the default branch, create and push the exact annotated tag:

```powershell
$CompatId = 'rust-v0.151.0-native-join-p10'
$Tag = "compat-$CompatId"
git tag -a $Tag -m $Tag
git push origin $Tag
```

The tag must match the catalog, be annotated, and point to a commit contained in the default branch. The tag push starts one ordered run:

1. resolve committed authority;
2. run same-commit Patch Validation;
3. build every manifest target on its native runner;
4. require one unique target record per target;
5. finalize a temporary manifest;
6. pack the exact compatibility payload;
7. generate the install display catalog;
8. reconcile and verify a draft;
9. publish the Release;
10. delete transient Actions artifacts from the successful run.

Formal publication never reuses the accepted candidate executable. Each target is rebuilt with:

```text
cargo build --target <manifest-target> --release --bin codex
```

Linux musl binaries are stripped after Cargo and before hashing or staging. Empty artifacts and artifacts larger than the Manager's 1 GiB download limit are rejected.

The Release contains:

- `compatibility-release.json`;
- one target-qualified Codex CLI per manifest target;
- the finalized compatibility manifest;
- expected source hashes;
- the five ordered patch files;
- the exact test contract;
- `SHA256SUMS`;
- `install-catalog-v1.json`.

The install catalog is display metadata. It is required in the Release inventory but intentionally excluded from `compatibility-release.json` and `SHA256SUMS`.

## Recover a compatibility Release

A failed upload may leave a draft. Rerunning the same exact release:

1. resolves the draft by numeric release ID;
2. repairs an unpublished or orphaned tag when necessary;
3. removes only unexpected draft assets;
4. uploads the reviewed local set idempotently;
5. compares remote names, sizes, and GitHub digests;
6. publishes only after the inventories match.

A normal run never changes a published Release. If an existing published Release differs, the workflow fails closed.

Asset-only repair requires an explicit manual dispatch with:

```text
compat_selector=<exact-compatibility-id>
publish=true
replace_published=true
```

The exact payload must be byte-identical between the published tag commit and the current workflow commit. The tag stays anchored to the published commit, and existing title and notes are preserved. If replacement or verification fails, the Release remains a draft for a retry with the same narrow authority.

## Cache and artifact policy

Patch Validation and the reusable native target workflow share GitHub Actions sccache:

```text
SCCACHE_GHA_ENABLED=on
SCCACHE_GHA_VERSION=csa-patched-codex-v1
```

Only the default branch uses `SCCACHE_GHA_RW_MODE=READ_WRITE`. Tags and other refs are read-only consumers, which avoids creating a separate cache copy for every release tag. Compiler identity, target, flags, and source content remain part of sccache object keys.

The workflows do not cache complete Cargo `target/` directories, create per-platform `actions/cache` archives, or delete repository caches. A miss only increases compile time.

Formal validation, target, and aggregate artifacts retain for one day on failure and are deleted from the successful release run after publication. Manual Windows and multi-platform acceptance bundles retain for 14 days.

## Watch upstream Codex

`watch-codex-release.yml` runs hourly and supports manual dispatch. It detects the latest formal `openai/codex` Release.

When a new stable version needs a port, the watcher:

1. clones the exact tag outside the CSA workspace;
2. ports only exact payload data and binding adapters;
3. registers a release-disabled candidate;
4. creates one review PR.

It does not compile patched Codex or publish a Release. Any clone, patch, registration, or PR failure creates or updates the single open `upstream-patch-blocked` issue. Automatic porting remains blocked until that issue is resolved.

## Release notes

Both release streams use `scripts/generate_release_notes.py` and committed Git history. Manager notes compare strict reachable `vX.Y.Z` tags. Compatibility notes compare lower `pN` tags for the same Codex version and patch family.

Release notes:

- do not repeat the GitHub Release title;
- do not add a generic `Release Information` footer;
- link the exact comparison range;
- keep the Manager and compatibility histories separate;
- preserve existing notes during an authorized published asset repair.

## Final checks

Run the release-tool suite:

```powershell
py -3 scripts\test_validation_evidence.py
py -3 scripts\test_compat_catalog.py
py -3 scripts\test_verify_release_asset_set.py
py -3 scripts\test_verify_patch_payload.py
py -3 scripts\test_release_tools.py
```

Run Rust and npm checks when affected:

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --all-targets
node scripts\test_npm_launcher.mjs
```

Local checks do not replace hosted Patch Validation, native target builds, isolated runtime acceptance, exact draft verification, or npm OIDC publication.
