# Releasing CSA

CSA has two independent release domains. The Manager and patched Codex must never be published as one aggregate product.

| Domain | Tag | Workflow | Published product |
| --- | --- | --- | --- |
| CSA Manager | `vX.Y.Z` | `.github/workflows/release-csa.yml`, then `.github/workflows/publish-npm.yml` | Five Manager archives and six npm packages |
| Patched Codex | `compat-<compat_id>` | `.github/workflows/release-patched-codex.yml` | Every manifest-declared native Codex CLI plus one exact verification payload |

GitHub Actions is the release authority for both domains. Publishing a formal Manager GitHub Release authorizes the independent npm Trusted Publishing workflow; no maintainer token or local npm login is used by that workflow.

## Current release snapshot

The current published Manager release is [`v0.1.6`](https://github.com/DSLZL/CSA/releases/tag/v0.1.6). The npm meta package is [`@dslzl/csa@0.1.6`](https://www.npmjs.com/package/@dslzl/csa).

The current formal patched release is [`compat-rust-v0.151.0-native-join-p10`](https://github.com/DSLZL/CSA/releases/tag/compat-rust-v0.151.0-native-join-p10):

| Field | Value |
| --- | --- |
| Compatibility ID | `rust-v0.151.0-native-join-p10` |
| Codex version | `0.151.0` |
| Upstream tag | `rust-v0.151.0` |
| Upstream commit | `78c290807ce710180111df227df3b7a4fe845452` |
| Patch-set version | `10` |
| Patch count | `5` |
| Rust toolchain | `1.95.0` |
| Release targets | Windows x64/arm64, Linux x64/arm64 musl, macOS x64/arm64 |
| Windows x64 production SHA-256 | `587cb1ea4753cf32919519b5d12b68001d059e828ce16c9a69b6347ff57cc54b` |
| Windows x64 production size | `314201088` bytes |

The accepted Windows x64 record is [`release/acceptance/rust-v0.151.0-native-join-p10/x86_64-pc-windows-msvc.json`](../release/acceptance/rust-v0.151.0-native-join-p10/x86_64-pc-windows-msvc.json). It references Patch Validation run [`33316162397`](https://github.com/DSLZL/CSA/actions/runs/33316162397) and candidate build run [`33316173523`](https://github.com/DSLZL/CSA/actions/runs/33316173523).

The committed candidate manifest intentionally contains placeholder artifact size and hash fields. The formal workflow finalizes a temporary manifest copy from its own production executable. The published manifest and Release descriptor, not the committed placeholder, describe the production asset.

## Compatibility authority

Compatibility identity is data-driven:

```text
release/compatibility-index.json
        | routing, lifecycle, build and release flags
        v
payload/codex/<compat-id>/manifest.toml
        | upstream, patches, preimages, target, artifact contract
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
       exact Patch Validation
                  |
                  v
       independent formal rebuild
                  |
                  v
       temporary manifest finalization
                  |
                  v
       exact draft asset reconciliation
                  |
                  v
       immutable formal Release
```

The catalog routes a selector to a manifest and the canonical acceptance locks. The manifest owns the complete platform artifact set; it remains the source of upstream, patch, preimage, target, and artifact truth.

Workflow YAML must not own Codex versions, upstream commits, patch generations, npm integrity values, accepted hashes, or runtime package identities.

## Compatibility lifecycle

Each entry in `release/compatibility-index.json` has a lifecycle and independent build and release flags:

| Lifecycle | Meaning |
| --- | --- |
| `legacy` | Retained and statically validated; formal release disabled |
| `candidate` | May run validation and candidate builds; formal release disabled |
| `accepted` | Local acceptance is committed; formal release may be enabled |

Historical payloads are immutable. Removing one from a default heavy-build route does not permit editing or deleting its manifest, patches, hashes, contract, or acceptance record.

## Release the Manager

### 1. Align the version

Update the same version in:

- `Cargo.toml` and the Manager package entry in `Cargo.lock`;
- `release/support-matrix.json`;
- `npm/meta/package.json`, including exact optional dependency versions;
- current-version examples and badges in user documentation.

The workflow rejects a request unless `Cargo.toml`, `release/support-matrix.json`, and `npm/meta/package.json` match the requested version.

### 2. Run the Manager release gate

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --all-targets
node --check npm\meta\bin\csa.js
node --check scripts\stage_npm_packages.mjs
node scripts\test_npm_launcher.mjs
py -3 scripts\test_release_tools.py
```

Commit and push the reviewed version change to the default branch before dispatching a release.

### 3. Dispatch the Manager workflow

Run `Release CSA` from the default branch with:

```text
version=0.1.6
```

The workflow:

1. verifies the requested version and default-branch source;
2. runs the full Rust, npm, and release-tool quality gate;
3. builds and smoke-tests five platform binaries;
4. creates five Manager archives;
5. stages, packs, installs, tests, and uninstalls npm candidates in temporary prefixes;
6. requires the exact archive and npm tarball inventory;
7. creates `SHA256SUMS`;
8. creates an annotated `vX.Y.Z` tag and immutable GitHub Release;
9. dispatches npm Trusted Publishing for that exact tag.

The `v0.1.6` asset pattern is:

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

### 4. Publish npm packages

Each npm package must be bound once to this repository and the exact case-sensitive workflow filename:

```powershell
npm trust github @dslzl/csa-win32-x64 --repo DSLZL/CSA --file publish-npm.yml --allow-publish --yes
npm trust github @dslzl/csa-linux-x64 --repo DSLZL/CSA --file publish-npm.yml --allow-publish --yes
npm trust github @dslzl/csa-linux-arm64 --repo DSLZL/CSA --file publish-npm.yml --allow-publish --yes
npm trust github @dslzl/csa-darwin-x64 --repo DSLZL/CSA --file publish-npm.yml --allow-publish --yes
npm trust github @dslzl/csa-darwin-arm64 --repo DSLZL/CSA --file publish-npm.yml --allow-publish --yes
npm trust github @dslzl/csa --repo DSLZL/CSA --file publish-npm.yml --allow-publish --yes
```

That bootstrap requires a maintainer npm login once. After publishing a formal `vX.Y.Z` Release, `release-csa.yml` explicitly dispatches `.github/workflows/publish-npm.yml`; the workflow's `release: published` trigger also covers formal Releases created outside the Manager workflow. Publication uses GitHub OIDC, so no npm token, OTP, or local login is needed for each release.

The workflow downloads only the selected formal, non-prerelease Manager Release; requires its exact 12-asset inventory; validates `SHA256SUMS` coverage and contents; and verifies every npm tarball name and version. It publishes the five platform packages before the meta package and allows a bounded registry-processing window after each publish. A rerun skips an existing version only when the registry integrity exactly matches the Release tarball, so a partial publication can be resumed without overwriting immutable npm versions.

Current package manifests bind `repository.url` to `https://github.com/DSLZL/CSA`, allowing npm to validate Sigstore provenance for future releases. The immutable `v0.1.4` tarballs predate that metadata, so only the exact `0.1.4` recovery path explicitly disables the provenance statement; authentication still uses the same npm Trusted Publishing OIDC exchange.

To publish an existing formal Release that predates the workflow, or to resume a partial run:

```powershell
gh workflow run publish-npm.yml --ref main -f tag=v0.1.6
```

Verify the registry and launcher:

```powershell
npm view @dslzl/csa version dist-tags.latest --registry=https://registry.npmjs.org
npx --yes @dslzl/csa@0.1.6 --version
```

## Validate a patched compatibility

Validate catalog structure locally:

```powershell
py -3 scripts\compat_catalog.py validate `
  --repository . `
  --workflow .github\workflows\build-patched-codex-target.yml `
  --workflow .github\workflows\build-patched-codex-windows.yml `
  --workflow .github\workflows\validate-patched-codex.yml `
  --workflow .github\workflows\release-patched-codex.yml
```

Resolve the current Windows x64 route:

```powershell
py -3 scripts\compat_catalog.py resolve `
  --repository . `
  --selector current `
  --target x86_64-pc-windows-msvc `
  --output $env:TEMP\csa-compat-resolution.json
```

Unknown selectors, unknown targets, source or lock drift, disabled flags, and missing acceptance records fail closed.

`Validate patched Codex CLI` is a formal-release gate. It is never triggered by an ordinary branch push, pull request, or development build. Pushing the exact annotated `compat-<compat_id>` tag makes the Release workflow call this gate for the tagged commit and wait for it before starting native builds.

The standalone dispatch remains available for diagnosis or recovery:

```powershell
gh workflow run validate-patched-codex.yml `
  --ref main `
  -f compat_selector=<exact-accepted-id> `
  -f target=x86_64-pc-windows-msvc
```

It clones the exact reviewed upstream commit into an ephemeral root, prepares pinned tools and runtime inputs, applies the payload, runs the complete generation and test contract, and uploads exact validation evidence for 14 days. A standalone run does not publish anything.

## Build a fast Windows test candidate

For routine local testing, dispatch `Build patched Codex CLI for Windows testing` from the pushed ref that contains the change:

```powershell
gh workflow run build-patched-codex-windows.yml `
  --ref <branch-or-tag> `
  -f compat_selector=<exact-candidate-id>
```

This path resolves the exact committed compatibility, calls the same single-target builder used by the formal six-platform matrix, and builds only `x86_64-pc-windows-msvc` on `windows-2025`. It uses the repository-wide patched-Codex sccache namespace and uploads `patched-codex-windows-test-<compat_id>` for 14 days.

Download one run directly into a disposable directory:

```powershell
gh run download <run-id> `
  -n patched-codex-windows-test-<compat_id> `
  -D .dev\windows-test-candidate
```

The artifact contains the canonical local acceptance inputs:

```text
bundle/bin/codex.exe
bundle/target-record.json
candidate-record.json
resolution.json
```

This workflow deliberately does not run Patch Validation, build the other five targets, finalize a manifest, or create a Release. Run the downloaded executable only by absolute path with an isolated `CODEX_HOME` and working directory. Ordinary pushes do not start Validation or Release. After local acceptance is committed and pushed to `main`, create and push the annotated compatibility tag described below.

## Run an optional formal multi-platform preflight

Dispatch `Release patched Codex CLI` from the default branch with:

```text
compat_selector=<exact-candidate-id>
target=x86_64-pc-windows-msvc
publish=false
```

The workflow runs Patch Validation for the same default-branch commit before starting the native matrix. This explicit preflight is optional; routine development should use the Windows-only workflow.

The workflow concurrently builds the six targets declared by the manifest:

```text
aarch64-apple-darwin
x86_64-apple-darwin
aarch64-unknown-linux-musl
x86_64-unknown-linux-musl
aarch64-pc-windows-msvc
x86_64-pc-windows-msvc
```

The resulting 14-day artifact is `patched-codex-acceptance-<compat_id>`. It preserves the canonical Windows acceptance inputs and adds every native target for manual inspection:

```text
bundle/
candidate-record.json
resolution.json
targets/
matrix.json
```

The candidate record binds the executable to the manifest, build profile, runtime lock, workflow run, job, and CSA source commit. It is build evidence, not acceptance by itself.

## Accept a candidate

Download the exact candidate artifact and test it in a disposable Windows environment. Use an isolated `CODEX_HOME`, fixture, logs, state, npm prefix, Manager root, and child-only `PATH`. Never replace the official Codex installation.

After sanitizing the evidence, bind the candidate:

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

`accept` requires a candidate lifecycle, an exact artifact and candidate record, matching manifest, build-profile, and runtime-lock hashes, and an explicit evidence file.

Review the acceptance record and catalog changes together. The evidence must state which lanes passed and which remain unverified. It must not contain credentials, authentication files, tokens, session content, or private environment dumps.

Commit and push the accepted state before requesting formal publication.

## Publish a patched Codex Release

After the accepted state is merged into `main`, create the exact annotated tag declared by the compatibility entry and push it:

```powershell
$CompatId = 'rust-v0.151.0-native-join-p10'
$Tag = "compat-$CompatId"
git tag -a $Tag -m $Tag
git push origin $Tag
```

The tag must match the resolved `release_tag`, be annotated, and point to a commit contained in `main`. The tag push starts one ordered run: resolve authority, run Patch Validation, build every native target, aggregate and verify the exact inventory, then publish the Release. A validation or target failure stops every later stage.

The selected entry must be accepted and release-enabled. The workflow does not reuse candidate executables or copy their accepted hashes into the production build. It independently builds each manifest target on its matching native hosted runner:

```text
cargo build --target <manifest-target> --release --bin codex
```

It does not publish Codex App, Desktop, app-server, exec-server, MCP server, or unrelated binaries.

The workflow fails if any matrix job fails or if the collected target set differs from the manifest. It then finalizes a temporary manifest copy, reverifies every production artifact, packs the flat compatibility payload, enforces the exact CLI-only executable set, and uploads the local set to a draft.

Each patched Codex Release contains:

- `compatibility-release.json`;
- six target-qualified CLIs, for example `<compat_id>--x86_64-pc-windows-msvc--codex.exe` and `<compat_id>--aarch64-apple-darwin--codex`;
- the finalized manifest and expected source hashes;
- the ordered patch files;
- the exact test contract;
- `SHA256SUMS`;
- `install-catalog-v1.json`.

The schema-2 install catalog is generated from committed, release-enabled compatibility records, each manifest's complete target set, and formal non-draft, non-prerelease Releases. It is display metadata for version selection and is intentionally excluded from `SHA256SUMS` and `compatibility-release.json`. The Manager still accepts legacy schema-1 single-target Releases.

## Recover a draft safely

A failed upload can leave a draft. Rerunning the same exact release:

1. verifies or repairs the unpublished tag to the requested source commit;
2. creates or resumes the matching draft;
3. removes only unexpected draft assets;
4. uploads the reviewed local asset set idempotently;
5. rereads remote names, sizes, and GitHub digests;
6. publishes only after local and remote inventories match exactly.

A published compatibility Release is immutable. If it already exists, the workflow verifies its exact assets and succeeds without mutation. Any mismatch fails closed.

## Cache policy

Caches reduce hosted build time but are never release authority. Patch Validation and every native Release matrix job use `mozilla-actions/sccache-action` with:

```text
SCCACHE_GHA_ENABLED=on
SCCACHE_GHA_VERSION=csa-patched-codex-v1
```

This is one repository-wide GitHub Actions object-cache namespace. Compiler identity, target, flags, and source content remain part of each sccache object key, so incompatible platform objects do not collide. The workflows do not create per-platform `actions/cache` archives, cache `target/`, or reserve separate multi-gigabyte cache entries that compete with the repository's 10 GiB cache quota.

A miss or cache-service failure only costs compile time. Correctness requires a cold build to pass, and the workflow still verifies every artifact hash and complete target inventory after compilation.

## Manager discovery and trust boundary

The Manager discovers formal `compat-*` tags through unauthenticated Git smart-HTTP refs, then probes a bounded number of newest tags for `install-catalog-v1.json`. Direct GitHub and the existing China mirror route are supported without requiring `GITHUB_TOKEN` or `GH_TOKEN`; the reviewed embedded catalog covers immutable Releases that predate the catalog asset.

The catalog is not installation authority. Its source tag, source commit, candidate refs, and target list are cross-checked only to build the picker; after selection, the Manager independently proves the exact tag, source commit, schema-1 or schema-2 descriptor, target-specific manifest artifact, asset set, sizes, and hashes. Unexpected redirects, duplicate metadata, malformed tags, missing or extra assets, and checksum drift fail closed.

Adding a compatibility does not require hard-coding its ID in the Manager, but it does require a formal compatible Release and a Manager build whose target and runtime detection support that entry.

## Required checks before merge

Run the local gate:

```powershell
py -3 scripts\test_validation_evidence.py
py -3 scripts\test_compat_catalog.py
py -3 scripts\test_verify_release_asset_set.py
py -3 scripts\test_verify_patch_payload.py
py -3 scripts\test_release_tools.py
```

Also run the Rust and npm gates when affected:

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --all-targets
node scripts\test_npm_launcher.mjs
```

Local checks do not replace hosted Patch Validation, candidate build, disposable runtime acceptance, independent formal rebuild, exact draft verification, or the GitHub OIDC npm publication gate.
