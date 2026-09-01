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

Bare `install` detects the official Codex version, resolves the Manager target to the
published artifact target, then installs the matching public Release with the greatest
numeric terminal `-pN` revision. Use
`--compat` only to pin an older exact match, and use absolute paths for local
compatibility manifests and artifacts/sources. On Windows, a successful install puts
the verified managed bin first in the user PATH and silently rechecks `where.exe codex`.

Interactive terminals use concise Human output and show install progress. Pass
`--json`, or redirect stdout, for stable machine reports. `status` summarizes the
installed and active command; `doctor` reports ordered PASS/WARN/FAIL checks and exits
0 for PASS/WARN, 1 for diagnosed failures, or 2 for incomplete diagnostics.
