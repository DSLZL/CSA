# CSA architecture

CSA runs a version-pinned patched Codex CLI beside the official installation. The Manager owns discovery, verification, preparation, activation, and removal. It does not modify the official package.

## Four separate identities

```text
Official Codex installation, read-only
                 |
                 | discover and fingerprint
                 v
            CSA Manager
                 |
                 | prepare and plug
                 v
      CSA command dispatcher
        | user PATH: <manager-root>/bin/codex
        | machine conflict: %ProgramFiles%\DSLZL\CSA\bin\codex.exe
          | valid binding   -> patched Codex + verified official runtime
          | invalid binding -> official Codex launcher
```

| Component | Owner | Responsibility |
| --- | --- | --- |
| Official Codex | Existing package-manager installation | Configuration, authentication, user state, runtime tools, and fallback |
| CSA Manager | CSA | Discovery, verification, installation, activation, status, and removal |
| Patched Codex | Manager-owned artifact directory | Native Join and TUI changes for one exact Codex release |
| `codex` shim | Manager-owned `bin` directory | Revalidate the binding and start patched or official Codex |
| Elevated dispatcher | CSA-owned Program Files directory | Win Machine PATH precedence without modifying package-manager launchers |

The official launcher, native executable, package metadata, and companion tools are external read-only resources. CSA records their paths, sizes, versions, and SHA-256 values. It rejects a Manager root that overlaps the official installation.

## Online installation

An online installation follows one bounded path:

```text
locate official Codex
  -> record exact version, target, package layout, sizes, and hashes
  -> read public CSA-codex compatibility refs
  -> build the matching version picker
  -> verify the selected formal Release and descriptor
  -> download the current-target executable
  -> verify declared size and SHA-256
  -> publish a content-addressed artifact and minimal runtime manifest
  -> create and verify the shim
```

The picker catalog is display metadata. It can tell the user which versions exist, but it cannot authorize an installation. After selection, CSA independently verifies:

- the selected annotated tag and commit;
- the Release descriptor and complete asset inventory;
- the declared upstream version and tag identity;
- the current build target and artifact filename;
- every declared size and SHA-256 value.

Online install persists only a minimal runtime manifest and the current platform's patched executable. Patch files, source hashes, test contracts, and source manifests stay in the CSA-codex GitHub Release for build and audit work.

## Direct and mirrored downloads

CSA does not require a GitHub token for installation. It reads public Git smart-HTTP refs instead of the GitHub REST API.

Before the first GitHub request, CSA runs two region probes in parallel, each with a five-second limit:

- Cloudflare trace;
- Alibaba's Taobao IP region service.

If either probe explicitly reports mainland China (`CN`), CSA enables a fixed mirror pool:

```text
gh-proxy.org
v4.gh-proxy.org
v6.gh-proxy.org
cdn.gh-proxy.org
axisnow.gh-proxy.org
gh-proxy.com
ghfast.top
```

The pool is first checked with the exact CSA Git refs request, including the Git protocol headers. This avoids rejecting a mirror simply because it does not proxy ordinary repository HTML.

After CSA knows the exact Release artifact and declared size, every active mirror concurrently requests the first 256 KiB with a three-second total limit. A sample is ranked only when it returns:

- an approved final host;
- HTTP `206`;
- a complete range;
- a `Content-Range` whose total matches the declared artifact size.

Valid samples are ordered by the full request and transfer time, including connection and first-byte delay. Nodes without a valid sample remain later fallbacks. A failed full transfer, size check, or SHA-256 check removes that node and tries the next one.

Outside mainland China, CSA starts with GitHub directly. Connection, timeout, protocol, `403`, `408`, `429`, and `5xx` failures may switch the rest of the installation to the mirror pool. Failed region probes do not imply `CN`; direct GitHub remains the default.

Region results and speed rankings are not stored in logs or Manager state. CSA does not send GitHub credentials to mirrors. Redirect hosts remain restricted, and every downloaded file must still pass Release size and checksum validation.

## Preparation and activation

Preparation runs before activation. It holds an exclusive Manager lock, stages files in a Manager-owned directory, and publishes state only after both the patched artifact and the official runtime fingerprints pass.

The artifact directory is content-addressed. Reinstalling the same verified executable can reuse it, but CSA validates the new input before accepting a cache hit.

Activation copies the Manager executable into `<manager-root>/bin/codex[.exe]`. The same Rust binary detects that it was launched as the shim and enters forwarding mode. On Windows, CSA first uses the user `PATH`; only a higher-priority machine entry causes a UAC prompt and installation of the same dispatcher under Program Files. There is no package-manager launcher rewrite or resident service.

On every launch, the shim checks:

1. prepared and active state;
2. its own identity;
3. the minimal runtime manifest;
4. the patched overlay;
5. every referenced official runtime component.

If the binding is valid, the shim starts the patched executable by absolute path. Exact `--version` requests are answered as `codex-cli X.Y.Z (CSA <compat-id>)` after the same validation. If validation fails, the shim excludes CSA paths and resolves the real official launcher. It never starts an unverified patched binary.

## Runtime overlay

The patched executable is an overlay rather than a copied Codex installation. CSA-owned or modified files resolve from the managed artifact first. Unchanged helpers and resources resolve from the verified official platform package.

This keeps the managed payload smaller and avoids copying, hard-linking, or symlinking official companion binaries. A later official Codex upgrade changes the recorded fingerprints and intentionally invalidates the old binding.

Arguments, standard streams, exit codes, working directory, terminal, and the rest of the environment retain normal launcher behavior.

## User data and database compatibility

Normal shim launches use the current `CODEX_HOME`. Configuration, authentication, sessions, and local databases remain where official Codex expects them. CSA does not copy or maintain a second everyday profile.

The p10 patch canonicalizes SQLx migration bytes for the target platform and can atomically repair only the known checksum produced by the earlier line-ending variant. It does not accept an arbitrary checksum, skip migrations, rebuild the database, or delete user rows. Unknown migration drift still fails through SQLx validation.

This design supports switching between official and patched Codex without maintaining two user histories. Compatibility-specific acceptance status is published by `DSLZL/CSA-codex`; the Manager does not bundle or reinterpret producer evidence.

Development and manual acceptance use a disposable `CODEX_HOME`. Credentials may be reused only with explicit operator authorization, and their contents must not enter logs, evidence, or the repository.

## Local preparation and isolated execution

Local mode uses an exact compatibility manifest and either a prepared artifact or a clean source checkout. Unlike online install, it validates the full payload:

- upstream tag, commit, and source preimages;
- ordered patches and exact file ownership;
- toolchain, generation commands, and tests;
- target, artifact filename, size, and hash.

Patch preflight accumulates the full ordered series in a temporary Git index. It does not use fuzzy or three-way application.

`csa exec --isolated` runs the prepared artifact without activating a shim. Separate absolute paths are required for `CODEX_HOME`, cwd, logs, state, and the evidence record. This keeps the official Codex control plane separate from the patched system under test.

## Compatibility authority

A compatibility binds one Codex version, upstream tag and commit, source preimages, patch revision, Rust toolchain, build targets, and artifacts. Those payloads and patch-family rules live in [`DSLZL/CSA-codex`](https://github.com/DSLZL/CSA-codex).

The Manager consumes only a published compatibility descriptor during online installation or an exact external manifest during local preparation. It has no bundled current payload and does not use version ranges, fuzzy patch application, or three-way patch fallback.

## Release boundaries

CSA uses two independent repository and Release namespaces:

| Repository and namespace | Product |
| --- | --- |
| `DSLZL/CSA`, `vX.Y.Z` | CSA Manager archives and npm packages |
| `DSLZL/CSA-codex`, `compat-<compat_id>` | Patched Codex binaries and compatibility payload |

A Manager Release never contains patched Codex. A compatibility Release never contains the Manager or npm packages.

Formal patched publication and its six independent native build repositories are outside the Manager repository. The Manager trusts only a descriptor and artifacts that pass its exact identity, size, and checksum checks.

## Failure model

CSA fails closed at each trust boundary:

- no matching official version or target means no installation;
- ambiguous highest patch revision requires an exact `--compat`;
- malformed catalog or Release metadata stops before activation;
- size or checksum drift removes the download and preserves the old state;
- preparation failure does not create a shim;
- activation failure withdraws the shim;
- official runtime drift invalidates patched mode;
- corrupt or unknown state never causes an unverified patched launch.

`unplug`, `uninstall`, and `purge` remove progressively more Manager-owned data. None of them deletes official Codex, user configuration, authentication, sessions, local databases, or external package-manager state.
