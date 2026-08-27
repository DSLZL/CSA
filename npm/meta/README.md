# @dslzl/csa

`csa` installs a Rust manager beside the official Codex CLI. The
Node launcher only selects the matching platform package, verifies its SHA-256,
and forwards the process.

Installing this npm package performs no patched-build download, build, activation, PATH change, or
profile edit. It exposes only the `csa` command and never replaces
the official `codex` npm bin.

```text
csa doctor --manifest <absolute-manifest>
csa install
csa install --compat <compat-id>
csa install --manifest <absolute-manifest> --artifact <absolute-binary>
csa uninstall
csa prepare --manifest <absolute-manifest> --artifact <absolute-binary>
csa status
csa exec --isolated ... -- <codex-args>
csa plug
csa unplug
csa purge
```

Bare `install` lists formal patched Codex Releases and prompts for an installable
version; automation must pass an exact `--compat`. Use absolute paths for local
compatibility manifests and artifacts/sources. `install` never edits PATH. `uninstall`
withdraws the shim before removing manager-owned data. Adding the managed bin
directory to PATH remains an explicit, separate user action. See the release
documentation for supported compatibility IDs and rollback procedures.
