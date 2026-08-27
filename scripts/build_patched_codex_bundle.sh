#!/usr/bin/env bash
set -euo pipefail

phase=all
if [[ $# -eq 6 ]]; then
  phase="$1"
  shift
elif [[ $# -ne 5 ]]; then
  echo "usage: $0 [tools|rust|xwin|runtime|tests|build|all] RESOLUTION_JSON SOURCE CARGO_TARGET OUTPUT STATS_OUTPUT" >&2
  exit 2
fi
case "$phase" in
  tools|rust|xwin|runtime|tests|build|all) ;;
  *) echo "unsupported build phase: $phase" >&2; exit 2 ;;
esac

resolution="$1"
source_root="$2"
cargo_target="$3"
output="$4"
stats_output="$5"
repository="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
test_report="${output}.tests.json"
test_stats_output="${stats_output%.json}-tests.json"

: "${CARGO_HOME:?CARGO_HOME must name an absolute cache directory}"
: "${RUSTUP_HOME:?RUSTUP_HOME must name an absolute cache directory}"
: "${SCCACHE_DIR:?SCCACHE_DIR must name an absolute cache directory}"
: "${XWIN_CACHE_DIR:?XWIN_CACHE_DIR must name an absolute cache directory}"
: "${CSA_TOOL_BIN:?CSA_TOOL_BIN must name an absolute disposable tool directory}"
: "${CSA_TOOL_CACHE:?CSA_TOOL_CACHE must name an absolute cache directory}"
: "${CSA_RUNTIME_CACHE:?CSA_RUNTIME_CACHE must name an absolute cache directory}"
: "${TMPDIR:?TMPDIR must name an absolute disposable temporary directory}"

for path in \
  "$resolution" "$source_root" "$cargo_target" "$output" "$stats_output" \
  "$CARGO_HOME" "$RUSTUP_HOME" "$SCCACHE_DIR" "$XWIN_CACHE_DIR" \
  "$CSA_TOOL_BIN" "$CSA_TOOL_CACHE" "$CSA_RUNTIME_CACHE" "$TMPDIR"; do
  [[ "$path" = /* ]] || { echo "all paths must be absolute: $path" >&2; exit 2; }
done
[[ -f "$resolution" && -d "$source_root" && -d "$(dirname "$output")" ]] || {
  echo "resolution/source/output parent must exist" >&2
  exit 2
}
case "$phase" in
  all|tests)
    [[ ! -e "$cargo_target" && ! -e "$output" && ! -e "$test_report" ]] || {
      echo "test phase requires new cargo target, output, and phase report paths" >&2
      exit 2
    }
    ;;
  build)
    [[ -d "$cargo_target" && ! -e "$output" && -f "$test_report" ]] || {
      echo "build phase requires an existing test target/report and a new output" >&2
      exit 2
    }
    ;;
  *)
    [[ ! -e "$output" ]] || { echo "output must remain new before the build phase" >&2; exit 2; }
    ;;
esac

mkdir -p \
  "$CARGO_HOME" "$RUSTUP_HOME" "$SCCACHE_DIR" "$XWIN_CACHE_DIR" \
  "$CSA_TOOL_BIN" "$CSA_TOOL_CACHE" "$CSA_RUNTIME_CACHE" "$TMPDIR"
chmod 0700 "$TMPDIR"

temp_root="$(mktemp -d "$TMPDIR/csa-cross-build.XXXXXXXX")"
cleanup() {
  [[ ! -x "$CSA_TOOL_BIN/sccache" ]] || "$CSA_TOOL_BIN/sccache" --stop-server >/dev/null 2>&1 || true
  rm -rf "$temp_root"
}
trap cleanup EXIT

env_file="$temp_root/resolution.env"
python3 - "$resolution" "$repository" > "$env_file" <<'PY'
import json
import shlex
import sys
from pathlib import Path

resolution = Path(sys.argv[1]).resolve(strict=True)
repository = Path(sys.argv[2]).resolve(strict=True)
data = json.loads(resolution.read_text(encoding="utf-8"))
if data.get("schema") != 1:
    raise SystemExit("unsupported resolution schema")

profile = data["build_profile"]
runtime = data["runtime"]
values = {
    "COMPAT_ID": data["compat_id"],
    "CODEX_VERSION": data["codex_version"],
    "UPSTREAM_TAG": data["upstream_tag"],
    "UPSTREAM_COMMIT": data["upstream_commit"],
    "MANIFEST": str((repository / data["manifest_path"]).resolve(strict=True)),
    "MANIFEST_SHA256": data["manifest_sha256"],
    "BUILD_PROFILE": str((repository / data["build_profile_path"]).resolve(strict=True)),
    "BUILD_PROFILE_SHA256": data["build_profile_sha256"],
    "RUNTIME_LOCK": str((repository / data["runtime_lock_path"]).resolve(strict=True)),
    "RUNTIME_LOCK_SHA256": data["runtime_lock_sha256"],
    "RUST_TOOLCHAIN": data["rust_toolchain"],
    "RUSTC_COMMIT": data["rustc_commit"],
    "BUILD_TARGET": data["build_target"],
    "ARTIFACT_FILENAME": data["artifact_filename"],
    "CARGO_PACKAGE": profile["product"]["cargo_package"],
    "CARGO_BIN": profile["product"]["cargo_bin"],
    "CARGO_BUILD_JOBS_PROFILE": profile["build"]["cargo_build_jobs"],
    "CARGO_INCREMENTAL_PROFILE": profile["build"]["cargo_incremental"],
    "SCCACHE_CACHE_SIZE_PROFILE": profile["build"]["sccache_cache_size"],
    "RUSTUP_VERSION": profile["rust"]["rustup_init"]["version"],
    "RUSTUP_URL": profile["rust"]["rustup_init"]["url"],
    "RUSTUP_SHA256": profile["rust"]["rustup_init"]["sha256"],
    "CARGO_XWIN_VERSION": profile["tools"]["cargo_xwin"]["version"],
    "CARGO_XWIN_URL": profile["tools"]["cargo_xwin"]["url"],
    "CARGO_XWIN_SHA256": profile["tools"]["cargo_xwin"]["sha256"],
    "CARGO_XWIN_MEMBER": profile["tools"]["cargo_xwin"]["archive_member"],
    "SCCACHE_VERSION": profile["tools"]["sccache"]["version"],
    "SCCACHE_URL": profile["tools"]["sccache"]["url"],
    "SCCACHE_SHA256": profile["tools"]["sccache"]["sha256"],
    "SCCACHE_MEMBER": profile["tools"]["sccache"]["archive_member"],
    "XWIN_VERSION_PROFILE": profile["xwin"]["version"],
    "XWIN_ARCH_PROFILE": profile["xwin"]["arch"],
    "XWIN_VARIANT_PROFILE": profile["xwin"]["variant"],
    "LLVM_VERSION": profile["llvm"]["version"],
    "LLVM_MAJOR": profile["llvm"]["major"],
    "LLVM_APT_KEY_URL": profile["llvm"]["apt_key_url"],
    "LLVM_APT_KEY_FINGERPRINT": profile["llvm"]["apt_key_fingerprint"],
    "LLVM_APT_REPOSITORY": profile["llvm"]["apt_repository"],
    "RUNTIME_ARCHIVE_URL": runtime["archive_url"],
    "RUNTIME_INTEGRITY": runtime["integrity"],
}
for key, value in values.items():
    print(f"{key}={shlex.quote(str(value))}")
PY
# shellcheck disable=SC1090
source "$env_file"

[[ "$(git -C "$source_root" rev-parse HEAD)" == "$UPSTREAM_COMMIT" ]] || {
  echo "source HEAD differs from the resolved upstream commit" >&2
  exit 1
}
[[ "$(git -C "$source_root" rev-parse "refs/tags/$UPSTREAM_TAG^{commit}")" == "$UPSTREAM_COMMIT" ]] || {
  echo "upstream tag does not peel to the resolved commit" >&2
  exit 1
}
if [[ "$phase" != build ]] && [[ -n "$(git -C "$source_root" status --porcelain=v1 --untracked-files=all)" ]]; then
  echo "upstream source must be pristine before the fail-closed patch contract runs" >&2
  exit 1
fi

export PATH="$CARGO_HOME/bin:$CSA_TOOL_BIN:/usr/lib/llvm-$LLVM_MAJOR/bin:$PATH"
export CARGO_BUILD_JOBS="$CARGO_BUILD_JOBS_PROFILE"
export CARGO_INCREMENTAL="$CARGO_INCREMENTAL_PROFILE"
export RUSTC_WRAPPER="$CSA_TOOL_BIN/sccache"
export SCCACHE_CACHE_SIZE="${CSA_SCCACHE_CACHE_SIZE:-$SCCACHE_CACHE_SIZE_PROFILE}"
export SCCACHE_IDLE_TIMEOUT=0
export XWIN_ACCEPT_LICENSE=1
export XWIN_ARCH="$XWIN_ARCH_PROFILE"
export XWIN_VARIANT="$XWIN_VARIANT_PROFILE"
export XWIN_VERSION="$XWIN_VERSION_PROFILE"

verify_sha256() {
  local file="$1" expected="$2"
  printf '%s  %s\n' "$expected" "$file" | sha256sum --check --strict >/dev/null
}

cached_download() {
  local name="$1" url="$2" expected_sha256="$3"
  local cached="$CSA_TOOL_CACHE/$expected_sha256-$name"
  if [[ -f "$cached" ]]; then
    if verify_sha256 "$cached" "$expected_sha256"; then
      printf '%s\n' "$cached"
      return
    fi
    rm -f "$cached"
  fi
  local partial="$CSA_TOOL_CACHE/.${expected_sha256}-${name}.$$.partial"
  rm -f "$partial"
  curl --proto '=https' --tlsv1.2 --fail --location --retry 3 --retry-all-errors \
    --output "$partial" "$url"
  verify_sha256 "$partial" "$expected_sha256"
  chmod 0644 "$partial"
  if [[ -e "$cached" ]]; then
    rm -f "$partial"
  else
    mv "$partial" "$cached"
  fi
  verify_sha256 "$cached" "$expected_sha256"
  printf '%s\n' "$cached"
}

install_release_binary() {
  local name="$1" url="$2" expected_sha256="$3"
  local cached
  cached="$(cached_download "$name" "$url" "$expected_sha256")"
  install -m 0755 "$cached" "$CSA_TOOL_BIN/$name"
}

install_release_tool() {
  local name="$1" url="$2" expected_sha256="$3" member="$4"
  local archive unpacked
  archive="$(cached_download "$name.tar.gz" "$url" "$expected_sha256")"
  unpacked="$temp_root/$name"
  mkdir -p "$unpacked"
  tar -xzf "$archive" --directory "$unpacked" "$member"
  install -m 0755 "$unpacked/$member" "$CSA_TOOL_BIN/$name"
}

llvm_matches() {
  local clang_version lld_version
  command -v clang-cl >/dev/null 2>&1 &&
    command -v lld-link >/dev/null 2>&1 &&
    command -v llvm-lib >/dev/null 2>&1 &&
    command -v ninja >/dev/null 2>&1 || return 1
  clang_version="$(clang-cl --version)" || return 1
  lld_version="$(lld-link --version)" || return 1
  [[ "$clang_version" == *"$LLVM_VERSION"* && "$lld_version" == *"$LLVM_VERSION"* ]]
}

require_exact_identity() {
  [[ "$3" == "$2" ]] || {
    printf '%s identity mismatch; expected exactly %s, got:\n%s\n' "$1" "$2" "$3" >&2
    exit 1
  }
}
require_identity_contains() {
  [[ "$3" == *"$2"* ]] || {
    printf '%s identity mismatch; expected output to contain %s, got:\n%s\n' "$1" "$2" "$3" >&2
    exit 1
  }
}

install_pinned_tools() {
  printf 'ci_stage=tools status=started\n'
  install_release_tool \
    cargo-xwin "$CARGO_XWIN_URL" "$CARGO_XWIN_SHA256" "$CARGO_XWIN_MEMBER"
  install_release_tool \
    sccache "$SCCACHE_URL" "$SCCACHE_SHA256" "$SCCACHE_MEMBER"
  install_release_binary \
    rustup-init "$RUSTUP_URL" "$RUSTUP_SHA256"
  require_exact_identity cargo-xwin "cargo-xwin $CARGO_XWIN_VERSION" "$(cargo-xwin --version)"
  require_exact_identity sccache "sccache $SCCACHE_VERSION" "$(sccache --version)"
  printf 'ci_stage=tools status=completed\n'
}

prepare_rust() {
  local rustup_log="$temp_root/rustup.log"
  printf 'ci_stage=rustup status=started\n'
  if ! {
    rustup-init --no-modify-path --profile minimal --default-toolchain none -y &&
      rustup toolchain install "$RUST_TOOLCHAIN" --profile minimal &&
      rustup default "$RUST_TOOLCHAIN" &&
      rustup target add --toolchain "$RUST_TOOLCHAIN" "$BUILD_TARGET"
  } 2>&1 | tee "$rustup_log"; then
    exit 1
  fi
  require_identity_contains rustc "commit-hash: $RUSTC_COMMIT" "$(rustc -Vv)"
  printf 'ci_stage=rustup status=completed\n'
}

prepare_xwin() {
  local xwin_cache_log="$temp_root/xwin-cache.log"
  printf 'ci_stage=xwin_cache status=started\n'
  if ! cargo xwin cache xwin 2>&1 | tee "$xwin_cache_log"; then
    exit 1
  fi
  printf 'ci_stage=xwin_cache status=completed\n'
}

prepare_llvm() {
  if ! llvm_matches; then
    printf 'ci_stage=llvm_toolchain status=started\n'
    local llvm_key="$temp_root/llvm-snapshot.gpg.key"
    local llvm_key_info="$temp_root/llvm-key-info.txt"
    local llvm_source="$temp_root/apt-llvm.list"
    curl --proto '=https' --tlsv1.2 --fail --location --retry 3 --retry-all-errors \
      --output "$llvm_key" "$LLVM_APT_KEY_URL"
    gpg --show-keys --with-colons "$llvm_key" > "$llvm_key_info"
    local fingerprint
    fingerprint="$(awk -F: '$1 == "fpr" { print $10; exit }' "$llvm_key_info")"
    [[ "$fingerprint" == "$LLVM_APT_KEY_FINGERPRINT" ]] || {
      echo "LLVM apt key fingerprint mismatch" >&2
      exit 1
    }
    printf '%s\n' "$LLVM_APT_REPOSITORY" > "$llvm_source"
    sudo install -m 0644 "$llvm_key" /usr/share/keyrings/apt.llvm.org.asc
    sudo install -m 0644 "$llvm_source" "/etc/apt/sources.list.d/apt-llvm-noble-$LLVM_MAJOR.list"
    local apt_log="$temp_root/apt.log"
    if ! {
      sudo apt-get update &&
        sudo apt-get install --yes \
          "clang-$LLVM_MAJOR" "lld-$LLVM_MAJOR" "llvm-$LLVM_MAJOR" ninja-build
    } 2>&1 | tee "$apt_log"; then
      exit 1
    fi
    printf 'ci_stage=llvm_toolchain status=completed\n'
  else
    printf 'ci_stage=llvm_toolchain status=ready\n'
  fi
  llvm_matches || { echo "exact LLVM $LLVM_VERSION toolchain is unavailable" >&2; exit 1; }
}

verify_build_toolchain() {
  require_identity_contains rustc "commit-hash: $RUSTC_COMMIT" "$(rustc -Vv)"
  require_exact_identity cargo-xwin "cargo-xwin $CARGO_XWIN_VERSION" "$(cargo-xwin --version)"
  require_exact_identity sccache "sccache $SCCACHE_VERSION" "$(sccache --version)"
  require_identity_contains clang-cl "$LLVM_VERSION" "$(clang-cl --version)"
  require_identity_contains lld-link "$LLVM_VERSION" "$(lld-link --version)"
}

verify_runtime_integrity() {
  python3 - "$1" "$RUNTIME_INTEGRITY" <<'PY'
import base64
import hashlib
import hmac
import sys
from pathlib import Path
archive = Path(sys.argv[1])
algorithm, expected = sys.argv[2].split("-", 1)
actual = base64.b64encode(hashlib.new(algorithm, archive.read_bytes()).digest()).decode("ascii")
if not hmac.compare_digest(actual, expected):
    raise SystemExit(f"official runtime integrity mismatch: {archive}")
PY
}

prepare_runtime() {
  local runtime_cache_key="$RUNTIME_LOCK_SHA256-$(basename "$RUNTIME_ARCHIVE_URL")"
  local official_archive="$CSA_RUNTIME_CACHE/$runtime_cache_key"
  printf 'ci_stage=official_runtime status=started\n'
  if [[ -f "$official_archive" ]]; then
    if ! verify_runtime_integrity "$official_archive"; then
      rm -f "$official_archive"
    fi
  fi
  if [[ ! -f "$official_archive" ]]; then
    local partial="$CSA_RUNTIME_CACHE/.${runtime_cache_key}.$$.partial"
    rm -f "$partial"
    curl --proto '=https' --tlsv1.2 --fail --location --retry 3 --retry-all-errors \
      --output "$partial" "$RUNTIME_ARCHIVE_URL"
    verify_runtime_integrity "$partial"
    chmod 0644 "$partial"
    if [[ -e "$official_archive" ]]; then
      rm -f "$partial"
    else
      mv "$partial" "$official_archive"
    fi
  fi
  verify_runtime_integrity "$official_archive"

  local -a runtime_members
  mapfile -t runtime_members < <(python3 - "$resolution" <<'PY'
import json
import sys
from pathlib import Path
data = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
for item in data["runtime"]["required_files"]:
    print(item)
PY
  )
  (( ${#runtime_members[@]} > 0 )) || { echo "runtime lock has no required members" >&2; exit 1; }
  local archive_listing="$temp_root/runtime-archive.txt"
  tar -tzf "$official_archive" > "$archive_listing"
  local member
  for member in "${runtime_members[@]}"; do
    grep -Fxq "$member" "$archive_listing" || {
      echo "official runtime archive is missing reviewed member: $member" >&2
      exit 1
    }
  done
  local official_root="$temp_root/official-runtime"
  mkdir -p "$official_root"
  tar -xzf "$official_archive" --directory "$official_root" --strip-components=3 "${runtime_members[@]}"
  printf 'ci_stage=official_runtime status=completed\n'
}

start_sccache() {
  sccache --start-server
  sccache --zero-stats || true
}

report_sccache() {
  local destination="$1"
  mkdir -p "$(dirname "$destination")"
  if sccache --show-stats --stats-format json > "$destination"; then
    local stats_args=(--stats "$destination")
    if [[ -n "${CSA_MINIMUM_RUST_HIT_RATE:-}" ]]; then
      stats_args+=(--minimum-rust-hit-rate "$CSA_MINIMUM_RUST_HIT_RATE")
    fi
    if [[ -n "${CSA_SCCACHE_PROFILE:-}" ]]; then
      stats_args+=(--profile "$CSA_SCCACHE_PROFILE")
    fi
    if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
      stats_args+=(--github-step-summary "$GITHUB_STEP_SUMMARY")
    fi
    python3 "$repository/scripts/check_sccache_stats.py" "${stats_args[@]}" || true
  else
    echo "warning: sccache statistics are unavailable" >&2
  fi
  sccache --show-stats || true
  sccache --stop-server || true
}

run_tests() {
  verify_build_toolchain
  start_sccache
  local started finished
  started="$(date +%s)"
  python3 "$repository/scripts/run_patch_contract.py" \
    --manifest "$MANIFEST" \
    --source "$source_root" \
    --cargo-target "$cargo_target" \
    --output "$test_report" \
    --cross-windows-msvc \
    --portable-evidence \
    --phase tests
  finished="$(date +%s)"
  echo "contract_tests_seconds=$((finished - started))"
  report_sccache "$test_stats_output" || true
}

run_build() {
  verify_build_toolchain
  start_sccache
  local contract_result="$temp_root/contract-result.json"
  local build_started build_finished
  build_started="$(date +%s)"
  python3 "$repository/scripts/run_patch_contract.py" \
    --manifest "$MANIFEST" \
    --source "$source_root" \
    --cargo-target "$cargo_target" \
    --output "$contract_result" \
    --cross-windows-msvc \
    --portable-evidence \
    --phase build \
    --resume "$test_report"
  build_finished="$(date +%s)"

  local artifact="$cargo_target/$BUILD_TARGET/release/$ARTIFACT_FILENAME"
  [[ -f "$artifact" ]] || { echo "expected CLI artifact is missing: $artifact" >&2; exit 1; }
  local actual_artifact_sha256 actual_artifact_size
  actual_artifact_sha256="$(sha256sum "$artifact" | cut -d ' ' -f 1)"
  actual_artifact_size="$(stat -c '%s' "$artifact")"

  mkdir -p "$output/bin"
  cp "$artifact" "$output/bin/$ARTIFACT_FILENAME"
  cp "$contract_result" "$output/contract-result.json"
  cat > "$output/build-environment.txt" <<EOF
schema=2
compat_id=$COMPAT_ID
manifest_sha256=$MANIFEST_SHA256
build_profile_sha256=$BUILD_PROFILE_SHA256
runtime_lock_sha256=$RUNTIME_LOCK_SHA256
upstream_tag=$UPSTREAM_TAG
upstream_commit=$UPSTREAM_COMMIT
rust_toolchain=$RUST_TOOLCHAIN
rustc_commit=$RUSTC_COMMIT
cargo_xwin=$CARGO_XWIN_VERSION
sccache=$SCCACHE_VERSION
msvc=$XWIN_VERSION_PROFILE
llvm=$LLVM_VERSION
build_target=$BUILD_TARGET
cargo_package=$CARGO_PACKAGE
cargo_bin=$CARGO_BIN
cargo_build_jobs=$CARGO_BUILD_JOBS
cargo_incremental=$CARGO_INCREMENTAL
windows_runtime=@openai/codex@$CODEX_VERSION-win32-x64
windows_runtime_integrity=$RUNTIME_INTEGRITY
artifact_sha256=$actual_artifact_sha256
artifact_size=$actual_artifact_size
build_seconds=$((build_finished - build_started))
EOF
  python3 - "$output" "$ARTIFACT_FILENAME" <<'PY'
import hashlib
import sys
from pathlib import Path
root = Path(sys.argv[1])
artifact = sys.argv[2]
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
    "build-environment.txt", "contract-result.json", "SHA256SUMS", f"bin/{artifact}",
}
if actual != expected:
    raise SystemExit(
        f"canonical bundle mismatch; missing={sorted(expected-actual)}, unknown={sorted(actual-expected)}"
    )
PY
  report_sccache "$stats_output" || true
}

case "$phase" in
  tools)
    install_pinned_tools
    ;;
  rust)
    install_pinned_tools
    prepare_rust
    ;;
  xwin)
    install_pinned_tools
    require_identity_contains rustc "commit-hash: $RUSTC_COMMIT" "$(rustc -Vv)"
    prepare_xwin
    prepare_llvm
    verify_build_toolchain
    ;;
  runtime)
    prepare_runtime
    ;;
  tests)
    install_pinned_tools
    run_tests
    ;;
  build)
    install_pinned_tools
    run_build
    ;;
  all)
    install_pinned_tools
    prepare_rust
    prepare_xwin
    prepare_llvm
    verify_build_toolchain
    prepare_runtime
    run_tests
    run_build
    ;;
esac
