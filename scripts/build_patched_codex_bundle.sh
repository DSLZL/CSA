#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 6 ]]; then
  echo "usage: $0 MANIFEST SOURCE CARGO_TARGET OUTPUT NPM_INTEGRITY STATS_OUTPUT" >&2
  exit 2
fi

manifest="$1"
source_root="$2"
cargo_target="$3"
output="$4"
official_windows_npm_integrity="$5"
stats_output="$6"
repository="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tool_bin="${CSA_TOOL_BIN:?CSA_TOOL_BIN must name an absolute disposable tool directory}"
: "${CARGO_HOME:?}"
: "${RUSTUP_HOME:?}"
: "${SCCACHE_DIR:?}"
: "${XWIN_CACHE_DIR:?}"
: "${TMPDIR:?}"

for path in "$manifest" "$source_root" "$cargo_target" "$output" "$stats_output" "$tool_bin"; do
  [[ "$path" = /* ]] || { echo "all paths must be absolute: $path" >&2; exit 2; }
done
[[ -f "$manifest" && -d "$source_root" && ! -e "$cargo_target" && ! -e "$output" ]] || {
  echo "manifest/source must exist and cargo target/output must be new" >&2
  exit 2
}

mkdir -p "$tool_bin" "$CARGO_HOME" "$RUSTUP_HOME" "$SCCACHE_DIR" "$XWIN_CACHE_DIR" "$TMPDIR"
export PATH="$CARGO_HOME/bin:$tool_bin:$PATH"
export CARGO_BUILD_JOBS=4
export CARGO_INCREMENTAL=0
export RUSTC_WRAPPER="$tool_bin/sccache"
export SCCACHE_CACHE_SIZE=4G
export XWIN_ACCEPT_LICENSE=1
export XWIN_ARCH=x86_64
export XWIN_VARIANT=desktop
export XWIN_VERSION=17

temp_root="$(mktemp -d "$TMPDIR/csa-cross-build.XXXXXXXX")"
cleanup() {
  "$tool_bin/sccache" --stop-server >/dev/null 2>&1 || true
  rm -rf "$temp_root"
}
trap cleanup EXIT

install_release_binary() {
  local name="$1" url="$2" expected_sha256="$3"
  local download="$temp_root/$name"
  curl --fail --location --retry 3 --output "$download" "$url"
  printf '%s  %s\n' "$expected_sha256" "$download" | sha256sum --check --strict
  install -m 0755 "$download" "$tool_bin/$name"
}

install_release_tool() {
  local name="$1" url="$2" expected_sha256="$3" member="$4"
  local archive="$temp_root/$name.tar.gz" unpacked="$temp_root/$name"
  curl --fail --location --retry 3 --output "$archive" "$url"
  printf '%s  %s\n' "$expected_sha256" "$archive" | sha256sum --check --strict
  mkdir -p "$unpacked"
  tar -xzf "$archive" --directory "$unpacked" "$member"
  install -m 0755 "$unpacked/$member" "$tool_bin/$name"
}

install_release_tool \
  cargo-xwin \
  https://github.com/rust-cross/cargo-xwin/releases/download/v0.23.0/cargo-xwin-v0.23.0.x86_64-unknown-linux-musl.tar.gz \
  74a216f64f10ea81c909f02d6b1a84cd0fda8de4c87ee52fe63ba76ab2392b75 \
  cargo-xwin
install_release_tool \
  sccache \
  https://github.com/mozilla/sccache/releases/download/v0.16.0/sccache-v0.16.0-x86_64-unknown-linux-musl.tar.gz \
  aec995a83ad3dff3d14b6314e08858b7b73d35ca85a5bcf3d3a9ec07dee35588 \
  sccache-v0.16.0-x86_64-unknown-linux-musl/sccache
install_release_binary \
  rustup-init \
  https://static.rust-lang.org/rustup/archive/1.29.0/x86_64-unknown-linux-gnu/rustup-init \
  4acc9acc76d5079515b46346a485974457b5a79893cfb01112423c89aeb5aa10

rustup_log="$temp_root/rustup.log"
if ! {
  rustup-init --no-modify-path --profile minimal --default-toolchain none -y &&
    rustup toolchain install 1.95.0 --profile minimal &&
    rustup default 1.95.0 &&
    rustup target add --toolchain 1.95.0 x86_64-pc-windows-msvc
} >"$rustup_log" 2>&1; then
  tail -n 200 "$rustup_log" >&2
  exit 1
fi
xwin_cache_log="$temp_root/xwin-cache.log"
if ! cargo xwin cache xwin >"$xwin_cache_log" 2>&1; then
  tail -n 200 "$xwin_cache_log" >&2
  exit 1
fi

missing=()
for executable in clang-cl lld-link llvm-lib ninja; do
  command -v "$executable" >/dev/null || missing+=("$executable")
done
if (( ${#missing[@]} )); then
  apt_log="$temp_root/apt.log"
  if ! { sudo apt-get update && sudo apt-get install --yes clang lld llvm ninja-build; } >"$apt_log" 2>&1; then
    tail -n 200 "$apt_log" >&2
    exit 1
  fi
fi

mapfile -t identity < <(python3 - "$manifest" "$repository" <<'PY'
import sys
from pathlib import Path

sys.path.insert(0, str(Path(sys.argv[2]) / "scripts"))
from verify_patch_payload import _digest, _load_payload

path = Path(sys.argv[1]).resolve(strict=True)
payload = _load_payload(path)
manifest = payload.manifest
artifact = manifest["artifacts"][manifest["build_target"]]
for value in (
    manifest["compat_id"], manifest["codex_version"], manifest["upstream_tag"],
    manifest["upstream_commit"], manifest["rust_toolchain"], manifest["rustc_commit"],
    manifest["build_target"], artifact["filename"], _digest(path.read_bytes()),
):
    print(value)
PY
)
[[ ${#identity[@]} -eq 9 ]] || { echo "cannot resolve exact manifest identity" >&2; exit 2; }
compat_id="${identity[0]}"
codex_version="${identity[1]}"
upstream_tag="${identity[2]}"
upstream_commit="${identity[3]}"
rust_toolchain="${identity[4]}"
rustc_commit="${identity[5]}"
build_target="${identity[6]}"
artifact_filename="${identity[7]}"
binding_sha256="${identity[8]}"

[[ "$rust_toolchain" == 1.95.0 ]]
[[ "$rustc_commit" == 59807616e1fa2540724bfbac14d7976d7e4a3860 ]]
[[ "$build_target" == x86_64-pc-windows-msvc && "$artifact_filename" == codex.exe ]]
require_exact_identity() {
  [[ "$3" == "$2" ]] || { printf '%s identity mismatch; expected exactly %s, got:\n%s\n' "$1" "$2" "$3" >&2; exit 1; }
}
require_identity_contains() {
  [[ "$3" == *"$2"* ]] || { printf '%s identity mismatch; expected output to contain %s, got:\n%s\n' "$1" "$2" "$3" >&2; exit 1; }
}
require_identity_contains rustc "commit-hash: $rustc_commit" "$(rustc -Vv)"
require_exact_identity cargo-xwin "cargo-xwin 0.23.0" "$(cargo-xwin --version)"
require_exact_identity sccache "sccache 0.16.0" "$(sccache --version)"
require_identity_contains clang-cl 21.1.8 "$(clang-cl --version)"
require_identity_contains lld-link 21.1.8 "$(lld-link --version)"
command -v llvm-lib >/dev/null || { echo 'llvm-lib is unavailable' >&2; exit 1; }
command -v ninja >/dev/null || { echo 'ninja is unavailable' >&2; exit 1; }

if [[ "$official_windows_npm_integrity" == registry ]]; then
  official_windows_npm_integrity="$(python3 - "$codex_version" <<'PY'
import base64
import json
import sys
import urllib.parse
import urllib.request

package = urllib.parse.quote("@openai/codex", safe="")
with urllib.request.urlopen(
    f"https://registry.npmjs.org/{package}/{sys.argv[1]}-win32-x64", timeout=30
) as response:
    value = json.load(response)["dist"]["integrity"]
algorithm, encoded = value.split("-", 1)
if algorithm != "sha512" or len(base64.b64decode(encoded, validate=True)) != 64:
    raise SystemExit("npm registry returned an invalid Windows runtime integrity")
print(value)
PY
)"
fi

official_archive="$temp_root/codex-$codex_version-win32-x64.tgz"
official_root="$temp_root/official-windows"
curl --fail --location --retry 3 --output "$official_archive" \
  "https://registry.npmjs.org/@openai/codex/-/codex-$codex_version-win32-x64.tgz"
python3 - "$official_archive" "$official_windows_npm_integrity" <<'PY'
import base64
import hashlib
import hmac
import sys
from pathlib import Path

archive = Path(sys.argv[1])
algorithm, expected = sys.argv[2].split("-", 1)
actual = base64.b64encode(hashlib.new(algorithm, archive.read_bytes()).digest()).decode("ascii")
if not hmac.compare_digest(actual, expected):
    raise SystemExit(f"official Windows npm integrity mismatch for {archive}")
PY
mkdir -p "$official_root"
tar -xzf "$official_archive" --directory "$official_root" --strip-components=3 \
  package/vendor/x86_64-pc-windows-msvc/bin/codex-code-mode-host.exe \
  package/vendor/x86_64-pc-windows-msvc/codex-resources/codex-command-runner.exe \
  package/vendor/x86_64-pc-windows-msvc/codex-resources/codex-windows-sandbox-setup.exe \
  package/vendor/x86_64-pc-windows-msvc/codex-path/rg.exe

sccache --start-server
sccache --zero-stats
contract_result="$temp_root/contract-result.json"
python3 "$repository/scripts/run_patch_contract.py" \
  --manifest "$manifest" \
  --source "$source_root" \
  --cargo-target "$cargo_target" \
  --output "$contract_result" \
  --cross-windows-msvc \
  --portable-evidence

artifact="$cargo_target/$build_target/release/$artifact_filename"
[[ -f "$artifact" ]]
actual_codex_sha256="$(sha256sum "$artifact" | cut -d ' ' -f 1)"
if [[ "$compat_id" == rust-v0.148.0-native-join-p2 ]]; then
  [[ "$actual_codex_sha256" == d1368d4a94c7ac4bf09296f68516343a76ce11aa375363d4fcddc7fe8ef09730 ]]
fi
if [[ "$compat_id" == rust-v0.149.0-native-join-p3 ]]; then
  [[ "$actual_codex_sha256" == 64badb66f88d0cee23276dd81e26fee3f2a490803a48c9c63bc55bca40b9174d ]]
fi

mkdir -p "$output/bin"
cp "$artifact" "$output/bin/codex.exe"
cp "$contract_result" "$output/contract-result.json"
cat > "$output/build-environment.txt" <<EOF
schema=1
compat_id=$compat_id
binding_sha256=$binding_sha256
upstream_tag=$upstream_tag
upstream_commit=$upstream_commit
rust_toolchain=1.95.0
rustc_commit=59807616e1fa2540724bfbac14d7976d7e4a3860
cargo_xwin=0.23.0
msvc=17
llvm=21.1.8
build_target=x86_64-pc-windows-msvc
cargo_build_jobs=4
cargo_incremental=0
windows_runtime=@openai/codex@$codex_version-win32-x64
windows_runtime_integrity=$official_windows_npm_integrity
EOF
python3 - "$output" <<'PY'
import hashlib
import sys
from pathlib import Path

root = Path(sys.argv[1])
paths = sorted(path for path in root.rglob("*") if path.is_file())
(root / "SHA256SUMS").write_text(
    "".join(
        f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.relative_to(root).as_posix()}\n"
        for path in paths
    ),
    encoding="ascii",
)
actual = {path.relative_to(root).as_posix() for path in root.rglob("*") if path.is_file()}
expected = {
    "build-environment.txt", "contract-result.json", "SHA256SUMS", "bin/codex.exe",
}
if actual != expected:
    raise SystemExit(f"canonical bundle mismatch; missing={sorted(expected-actual)}, unknown={sorted(actual-expected)}")
PY

mkdir -p "$(dirname "$stats_output")"
sccache --show-stats --stats-format json > "$stats_output"
stats_args=(--stats "$stats_output")
if [[ -n "${CSA_MINIMUM_RUST_HIT_RATE:-}" ]]; then
  stats_args+=(--minimum-rust-hit-rate "$CSA_MINIMUM_RUST_HIT_RATE")
fi
python3 "$repository/scripts/check_sccache_stats.py" "${stats_args[@]}"
sccache --show-stats
sccache --stop-server
