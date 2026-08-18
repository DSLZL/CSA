# @dslzl/csa

`csa` installs a Rust manager beside the official Codex CLI. The
Node launcher only selects the matching platform package, verifies its SHA-256,
and forwards the process.

Installation performs no download, build, patch, activation, PATH change, or
profile edit. It exposes only the `csa` command and never replaces
the official `codex` npm bin.

```text
csa doctor --manifest <absolute-manifest>
csa install --manifest <absolute-manifest> --artifact <absolute-binary>
csa uninstall
csa prepare --manifest <absolute-manifest> --artifact <absolute-binary>
csa status
csa exec --isolated ... -- <codex-args>
csa plug
csa unplug
csa purge
```

Use absolute paths for compatibility manifests and local artifacts/sources.
`install` runs prepare plus plug without downloading or editing PATH. `uninstall`
withdraws the shim before removing manager-owned data. Adding the managed bin
directory to PATH remains an explicit, separate user action. See the release
documentation for supported compatibility IDs and rollback procedures.
