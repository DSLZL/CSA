# Releasing CSA

CSA has two independent release domains. The Manager and patched Codex must never be published as one aggregate product.

| Domain | Tag | Workflow | Published product |
| --- | --- | --- | --- |
| CSA Manager | `vX.Y.Z` | `.github/workflows/release-csa.yml`, then `.github/workflows/publish-npm.yml` | Five Manager archives and six npm packages |
| Patched Codex | `compat-<compat_id>` | `.github/workflows/release-patched-codex.yml` | One reviewed target-specific Codex CLI plus verification payload |

GitHub Actions is the release authority for both domains. Publishing a formal Manager GitHub Release authorizes the independent npm Trusted Publishing workflow; no maintainer token or local npm login is used by that workflow.

## Current release snapshot

The current published Manager release is [`v0.1.4`](https://github.com/DSLZL/CSA/releases/tag/v0.1.4). The npm meta package is [`@dslzl/csa@0.1.4`](https://www.npmjs.com/package/@dslzl/csa).

The current formal patched release is [`compat-rust-v0.150.1-native-join-p9`](https://github.com/DSLZL/CSA/releases/tag/compat-rust-v0.150.1-native-join-p9):

| Field | Value |
| --- | --- |
| Compatibility ID | `rust-v0.150.1-native-join-p9` |
| Codex version | `0.150.1` |
| Upstream tag | `rust-v0.150.1` |
| Upstream commit | `90854393966b21e9ebfd21b122334eb09a20c93d` |
| Patch-set version | `9` |
| Patch count | `17` |
| Rust toolchain | `1.95.0` |
| Target | `x86_64-pc-windows-msvc` |
| Production executable SHA-256 | `ce3cfe861f974c37b2217c0625c2e41574a5cfb48373b499f76c1108e1a86e76` |
| Production executable size | `311046656` bytes |

The accepted record is [`release/acceptance/rust-v0.150.1-native-join-p9/x86_64-pc-windows-msvc.json`](../release/acceptance/rust-v0.150.1-native-join-p9/x86_64-pc-windows-msvc.json). It references Patch Validation run [`33236190927`](https://github.com/DSLZL/CSA/actions/runs/33236190927) and candidate build run [`33238015308`](https://github.com/DSLZL/CSA/actions/runs/33238015308).

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
       exact Patch Validation
                  |
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

The catalog routes a selector to a manifest and target-specific locks. It does not replace the manifest as the source of upstream, patch, preimage, target, or artifact truth.

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
version=0.1.4
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

The `v0.1.4` asset pattern is:

```text
csa-v0.1.4-windows-x86_64.zip
csa-v0.1.4-linux-x86_64.tar.gz
csa-v0.1.4-linux-aarch64.tar.gz
csa-v0.1.4-macos-x86_64.tar.gz
csa-v0.1.4-macos-aarch64.tar.gz
dslzl-csa-0.1.4.tgz
dslzl-csa-win32-x64-0.1.4.tgz
dslzl-csa-linux-x64-0.1.4.tgz
dslzl-csa-linux-arm64-0.1.4.tgz
dslzl-csa-darwin-x64-0.1.4.tgz
dslzl-csa-darwin-arm64-0.1.4.tgz
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

The workflow downloads only the selected formal, non-prerelease Manager Release; requires its exact 12-asset inventory; validates `SHA256SUMS` coverage and contents; and verifies every npm tarball name and version. It publishes the five platform packages before the meta package. A rerun skips an existing version only when the registry integrity exactly matches the Release tarball, so a partial publication can be resumed without overwriting immutable npm versions.

Current package manifests bind `repository.url` to `https://github.com/DSLZL/CSA`, allowing npm to validate Sigstore provenance for future releases. The immutable `v0.1.4` tarballs predate that metadata, so only the exact `0.1.4` recovery path explicitly disables the provenance statement; authentication still uses the same npm Trusted Publishing OIDC exchange.

To publish an existing formal Release that predates the workflow, or to resume a partial run:

```powershell
gh workflow run publish-npm.yml --ref main -f tag=v0.1.4
```

Verify the registry and launcher:

```powershell
npm view @dslzl/csa version dist-tags.latest --registry=https://registry.npmjs.org
npx --yes @dslzl/csa@0.1.4 --version
```

## Validate a patched compatibility

Validate catalog structure locally:

```powershell
py -3 scripts\compat_catalog.py validate `
  --repository . `
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

The `Validate patched Codex CLI` workflow runs on relevant pull requests and default-branch pushes, and can also be dispatched manually. It clones the exact reviewed upstream commit into an ephemeral root, prepares pinned tools and runtime inputs, applies the payload, runs the complete generation and test contract, and uploads exact validation evidence for 14 days.

## Build an acceptance candidate

Dispatch `Release patched Codex CLI` from the default branch with:

```text
compat_selector=<exact-candidate-id>
target=x86_64-pc-windows-msvc
publish=false
validation_run_id=<optional exact successful run>
```

The workflow requires successful Patch Validation evidence from the same default-branch commit. Automatic selection chooses the newest matching run; `validation_run_id` pins one exact successful run when needed.

The resulting 14-day artifact is `patched-codex-acceptance-<compat_id>`:

```text
bundle/
candidate-record.json
resolution.json
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

Dispatch `Release patched Codex CLI` from the default branch:

```text
compat_selector=rust-v0.150.1-native-join-p9
target=x86_64-pc-windows-msvc
publish=true
validation_run_id=<optional exact successful run>
```

The selected entry must be accepted and release-enabled. The workflow does not reuse the candidate executable or copy its accepted hash into the production build. It independently rebuilds only:

```text
cargo xwin build --locked --release -p codex-cli --bin codex
```

It does not publish Codex App, Desktop, app-server, exec-server, MCP server, or unrelated binaries.

The workflow finalizes a temporary manifest copy, reverifies clean source and the production artifact, packs the flat compatibility payload, enforces one executable product, and uploads the exact local set to a draft.

Each patched Codex Release contains:

- `compatibility-release.json`;
- one `<compat_id>--codex.exe`;
- the finalized manifest and expected source hashes;
- the ordered patch files;
- the exact test contract;
- `SHA256SUMS`;
- `install-catalog-v1.json`.

The install catalog is generated from committed, release-enabled compatibility records and formal non-draft, non-prerelease Releases. It is display metadata for version selection and is intentionally excluded from `SHA256SUMS` and `compatibility-release.json`; older Managers therefore keep accepting the existing payload contract.

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

Caches reduce hosted build time but are never release authority. Validation and formal release use the same cache classes:

- Cargo registry, package cache, and Git database;
- exact Rustup toolchain;
- exact xwin SDK and LLVM inputs;
- pinned build tools;
- official runtime archive for validation;
- separate test and release sccache objects.

Both patched workflows use the shared xwin path `$RUNNER_TEMP/csa-patched-codex-cache/xwin` and keys bound to the reviewed build-profile hash. The preparation step still verifies and completes the exact SDK and LLVM toolchain after a restore. A miss or partial cache therefore costs time but must not change the result.

Rustup, xwin, build-tool, runtime, Cargo, and sccache saves are best-effort. Correctness requires a cold build to pass. Do not add an uncontrolled `target/` cache without measured restore and save data plus a compatibility-safety design.

## Manager discovery and trust boundary

The Manager discovers formal `compat-*` tags through unauthenticated Git smart-HTTP refs, then probes a bounded number of newest tags for `install-catalog-v1.json`. Direct GitHub and the existing China mirror route are supported without requiring `GITHUB_TOKEN` or `GH_TOKEN`; the reviewed embedded catalog covers immutable Releases that predate the catalog asset.

The catalog is not installation authority. Its source tag, source commit, and candidate refs are cross-checked only to build the picker; after selection, the Manager independently proves the exact tag, source commit, descriptor, manifest, asset set, sizes, and hashes. Unexpected redirects, duplicate metadata, malformed tags, missing or extra assets, and checksum drift fail closed.

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
