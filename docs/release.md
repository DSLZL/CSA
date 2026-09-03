# Releasing CSA Manager

This repository publishes only CSA Manager releases. Patched Codex source, payloads, native builds, acceptance evidence, and `compat-<compat_id>` releases are owned by [`DSLZL/CSA-codex`](https://github.com/DSLZL/CSA-codex) and its six platform build repositories.

| Product | Repository | Tag | Workflow |
| --- | --- | --- | --- |
| CSA Manager | `DSLZL/CSA` | `vX.Y.Z` | `release-csa.yml`, then `publish-npm.yml` |
| Patched Codex | `DSLZL/CSA-codex` | `compat-<compat_id>` | Producer-controlled orchestration across six build repositories |

GitHub Actions is the Manager release authority. Local builds are evidence, not permission to publish.

## Current Manager release

The current Manager release is [`v0.1.8`](https://github.com/DSLZL/CSA/releases/tag/v0.1.8), and the npm meta package is [`@dslzl/csa@0.1.8`](https://www.npmjs.com/package/@dslzl/csa).

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

Run `Release CSA` from the default branch with the exact SemVer value, for example:

```text
version=0.1.8
```

The workflow:

1. verifies the requested version and source branch;
2. runs the Rust, npm, and Manager release-tool gates;
3. builds and smoke-tests five native Manager binaries;
4. creates five archives;
5. stages and tests npm packages in temporary prefixes;
6. requires the exact archive and tarball inventory;
7. creates `SHA256SUMS`;
8. creates an annotated `vX.Y.Z` tag and GitHub Release;
9. explicitly dispatches npm Trusted Publishing for that tag.

For version `0.1.8`, the asset set is:

```text
csa-v0.1.8-windows-x86_64.zip
csa-v0.1.8-linux-x86_64.tar.gz
csa-v0.1.8-linux-aarch64.tar.gz
csa-v0.1.8-macos-x86_64.tar.gz
csa-v0.1.8-macos-aarch64.tar.gz
dslzl-csa-0.1.8.tgz
dslzl-csa-win32-x64-0.1.8.tgz
dslzl-csa-linux-x64-0.1.8.tgz
dslzl-csa-linux-arm64-0.1.8.tgz
dslzl-csa-darwin-x64-0.1.8.tgz
dslzl-csa-darwin-arm64-0.1.8.tgz
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

`publish-npm.yml` accepts either a formal, non-prerelease Manager Release `published` event or a manual exact `vX.Y.Z` tag for recovery. It downloads the Release instead of packing the workspace again, verifies the exact inventory and checksums, then publishes the five platform packages before `@dslzl/csa`.

A rerun skips an existing version only when registry SRI matches the exact Release tarball. This permits recovery from partial publication without overwriting npm's immutable versions.

```powershell
gh workflow run publish-npm.yml --ref main -f tag=v0.1.8
npm view @dslzl/csa version dist-tags.latest --registry=https://registry.npmjs.org
npx --yes @dslzl/csa@0.1.8 --version
```

The exact immutable `0.1.4` recovery path is the only exception that disables the Sigstore provenance statement because those old tarballs lack `repository.url`. It still uses OIDC.

## Release notes

`scripts/generate_release_notes.py` generates Manager notes from committed Git history and compares only reachable `vX.Y.Z` tags.

Release notes:

- do not repeat the GitHub Release title;
- do not add a generic `Release Information` footer;
- link the exact Manager comparison range;
- omit empty sections;
- fail closed when meaningful changes unexpectedly produce no notes.

Compatibility release notes are generated and maintained in `DSLZL/CSA-codex`; they are not part of this repository's changelog namespace.

## Final checks

Run the release-tool, Rust, and npm gates:

```powershell
py -3 scripts\test_release_tools.py
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --all-targets
node scripts\test_npm_launcher.mjs
```

Local checks do not replace hosted native Manager builds, release inventory verification, or npm OIDC publication.
