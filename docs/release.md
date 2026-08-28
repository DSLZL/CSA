# Release process

CSA has two independent release domains:

1. **CSA Manager and npm packages** are published by `.github/workflows/release-csa.yml`.
2. **Patched Codex CLI compatibility artifacts** are published by `.github/workflows/release-patched-codex.yml`.

Do not merge these release streams. A Manager release is a five-platform CSA product release. A patched-Codex release publishes only the reviewed patched `codex` CLI executable for targets explicitly declared by the selected compatibility contract.

## Patched-Codex authority model

The patched-Codex pipeline is data driven. Workflow YAML is not allowed to own compatibility versions, upstream commits, npm integrity values, patch generations, or accepted artifact hashes.

The authority chain is:

```text
release/compatibility-index.json
        │ routing and lifecycle only
        ▼
payload/codex/**/manifest.toml
        │ upstream, patch, source-preimage, target, artifact contract
        ├──────────────┐
        ▼              ▼
build profile      runtime lock
        │              │
        └──────┬───────┘
               ▼
       exact candidate build
               │
               ▼
       machine-readable candidate record
               │
               ▼
       disposable local acceptance
               │
               ▼
       committed acceptance record
               │
               ▼
       independent formal rebuild
               │
               ▼
       temporary manifest finalization
               │ exact local/remote asset match
               ▼
       draft reconciliation and publication
```

The compatibility index selects a manifest and target-specific locks. It does not replace the manifest as the source of upstream, patch, or artifact truth.

## Current reviewed compatibility

The current selector for `x86_64-pc-windows-msvc` resolves to:

```text
rust-v0.149.0-native-join-p3
```

The current reviewed contract contains:

```text
Codex version: 0.149.0
Upstream tag: rust-v0.149.0
Patch-set version: 6
Patch count: 14
Target: x86_64-pc-windows-msvc
Product: codex-cli / codex
Accepted artifact SHA-256:
e3302a04e8bc6062c5d092692e7d38239986453c599dcdf128fd1d9598f596fd
Accepted artifact size: 298215424 bytes
```

This repository currently declares only the Windows x64 patched-Codex target. The workflow is target-indexed, but an additional target is not publishable until its manifest artifact entry, build profile, runtime contract where applicable, acceptance record, Manager support, and hosted verification are all reviewed.

## Compatibility lifecycle

Each compatibility in `release/compatibility-index.json` has a lifecycle and independent build/release flags.

- `legacy`: retained and statically validated; an exact heavy build may be requested, but formal release is disabled.
- `candidate`: a newly ported compatibility that may be built to produce candidate evidence; formal release is disabled.
- `accepted`: local acceptance has been recorded and the exact artifact identity is frozen; formal release may be enabled.

Historical compatibility payloads are immutable. Removing a historical version from the default heavy-build path does not permit changing or deleting its payload.

## Resolve and validate

Validate the whole catalog:

```bash
python scripts/compat_catalog.py validate \
  --repository . \
  --workflow .github/workflows/validate-patched-codex.yml \
  --workflow .github/workflows/release-patched-codex.yml
```

Resolve the current Windows target:

```bash
python scripts/compat_catalog.py resolve \
  --repository . \
  --selector current \
  --target x86_64-pc-windows-msvc \
  --output /tmp/csa-compat-resolution.json
```

Resolve the exact accepted p3 route and require release authority:

```bash
python scripts/compat_catalog.py resolve \
  --repository . \
  --selector rust-v0.149.0-native-join-p3 \
  --target x86_64-pc-windows-msvc \
  --require-acceptance \
  --require-release \
  --output /tmp/csa-compat-resolution.json
```

Unknown selectors, unknown targets, hash drift, disabled build/release flags, and missing acceptance records fail closed.

## GitHub Actions candidate build

The `Release patched Codex CLI` workflow owns both build modes, but each dispatch has one purpose. With `publish=false` it produces only a disposable local-acceptance candidate; with `publish=true` it independently rebuilds an accepted entry and owns formal publication.

Dispatch the workflow from the default branch with:

```text
compat_selector=<exact-candidate-id>
target=x86_64-pc-windows-msvc
publish=false
```

The dispatch requires successful Patch Validation evidence from the same default-branch commit. Use `validation_run_id` only to select one exact successful run when automatic selection is not appropriate. One dispatch resolves and builds exactly one compatibility and target.

The 14-day workflow artifact is named `patched-codex-acceptance-<compat_id>` and contains:

```text
bundle/
candidate-record.json
resolution.json
```

The candidate record binds the artifact to the manifest, build profile, runtime lock, provider pipeline/job identity, and source commit. It is not an acceptance record by itself.

## Port a new compatibility

First create and review the complete payload manifest and runtime lock. Then register it as a non-releasable candidate:

```bash
python scripts/compat_catalog.py stage-candidate \
  --repository . \
  --manifest payload/codex/rust-v0.150.0-native-join-p4/manifest.toml \
  --target x86_64-pc-windows-msvc \
  --runtime-lock release/runtime-locks/rust-v0.150.0-native-join-p4.json
```

This command reads the exact compatibility ID and upstream facts from the manifest. It does not place version identities in GitHub workflow YAML.

Commit the payload, runtime lock, and catalog candidate route together. Run static validation before requesting a heavy build.

## Candidate acceptance

Download the `patched-codex-acceptance-<compat_id>` GitHub Actions artifact. Perform acceptance only in a disposable Windows environment using an isolated `CODEX_HOME`, working directory, npm prefix, and activation path. Do not replace the official Codex installation. The executable is at `bundle/bin/codex.exe`; the record is `candidate-record.json`.

The sanitized evidence JSON must identify the actual tests performed and must not contain credentials, auth files, tokens, session content, or private paths.

After acceptance, bind the exact candidate artifact:

```powershell
$CompatId = "rust-v0.150.0-native-join-p4"
$Target = "x86_64-pc-windows-msvc"
python scripts/compat_catalog.py accept `
  --repository . `
  --selector $CompatId `
  --target $Target `
  --candidate-record .\candidate-record.json `
  --artifact .\bundle\bin\codex.exe `
  --acceptance ("release/acceptance/{0}/{1}.json" -f $CompatId, $Target) `
  --evidence .\sanitized-acceptance-evidence.json `
  --make-current
```

`accept` requires:

- a catalog entry still in `candidate` lifecycle;
- an artifact matching the candidate record;
- a candidate record bound to the current manifest, build-profile, and runtime-lock hashes;
- an explicit evidence file.

Review the acceptance JSON and compatibility-index changes as one security-sensitive change. The accepted candidate hash records what was tested locally; it does not constrain the later independent production build.

## Formal patched-Codex release

Dispatch `.github/workflows/release-patched-codex.yml` from the default branch with:

```text
compat_selector=rust-v0.149.0-native-join-p3
target=x86_64-pc-windows-msvc
```

The workflow does not accept copied npm integrity, development candidate artifacts, or accepted SHA values. It resolves committed source/runtime authority and performs an independent CLI-only production build.

The formal build command remains limited to:

```text
cargo xwin build --locked --release -p codex-cli --bin codex
```

It does not publish Codex App, Desktop, app-server, exec-server, MCP server, or unrelated binaries.

The workflow finalizes a temporary manifest copy from its own executable, verifies that staged production authority, and then calls `scripts/compat_release.py pack`. The committed manifest and optional development acceptance record are not rewritten.

## Draft recovery and immutable publication

A failed upload can leave a draft Release. Rerunning the same exact release:

1. verifies the tag points to the same source commit;
2. creates or resumes the matching draft;
3. removes only unexpected draft assets;
4. uploads the reviewed local asset set idempotently;
5. re-reads remote names, sizes, and GitHub digests;
6. publishes only after the remote set exactly matches the local set.

A published compatibility Release is never mutated. If it already exists, the workflow verifies the exact asset set and exits successfully; a mismatch fails closed.

## Release assets

The user-facing executable remains an independent asset. Compatibility payload files, descriptor, and checksums are verification assets required by CSA Manager. The asset guard enforces exactly one executable product for the current target and rejects unrelated Codex binaries.

The formal release is intentionally not a five-platform aggregate. When additional patched targets are eventually accepted, each target must remain an independently named downloadable executable and must be represented by an exact target-specific contract.

## Cache policy

The replacement preserves the existing cache classes. Manager platform CI also discovers npm tarball filenames from `npm pack --json`; it does not assume a package version in workflow YAML.

The retained cache classes are:

- Cargo registry/index/cache and git DB;
- Rustup toolchain;
- cargo-xwin SDK;
- pinned build-tool archives;
- official runtime archive;
- sccache compiler objects.

Cache keys are bound to reviewed build-profile, runtime-lock, manifest, target, and upstream identities. A cache miss must never affect correctness; a cold build remains authoritative.

Do not add an uncontrolled `target/` cache without a measured restore/save benchmark and a compatibility-safety design.

## Manager compatibility discovery boundary

This release architecture removes compatibility/version authority from CI and release workflow YAML. The current CSA Manager still contains its existing compatibility-selection behavior. Replacing Manager online discovery with a remotely fetched compatibility index is a separate client protocol and trust migration requiring Rust integration tests, downgrade/rollback rules, signature or immutable-release verification, and an independent Manager release.

Do not silently mix that client change into a CI configuration replacement. Until that migration is implemented, adding a new compatibility may still require the existing Manager mapping update and a Manager release.

## Required validation

Before merging:

```bash
python validation/validate_replacements.py --repository .
python scripts/test_compat_catalog.py
python scripts/test_verify_release_asset_set.py
python scripts/test_verify_patch_payload.py
python scripts/test_release_tools.py
bash -n scripts/build_patched_codex_bundle.sh
```

Also run when available:

```bash
actionlint
```

Hosted Patch Validation, the GitHub Actions acceptance-candidate build, the independent formal rebuild, and formal draft publication remain mandatory canary validations; local static checks do not substitute for them.
