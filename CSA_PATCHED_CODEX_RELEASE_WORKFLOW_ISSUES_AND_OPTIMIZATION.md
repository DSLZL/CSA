# CSA Patched Codex CLI Release Workflow 问题分析与优化方案

## 1. 背景

在 GitHub Actions run：

- `https://github.com/DSLZL/CSA/actions/runs/33032333155`

中，当前 `release-patched-codex.yml` 暴露出了两个比较明显的问题：

1. **同一份大型 Codex CLI 构建产物被上传成两份高度重复的 GitHub Actions artifact**
2. **发布流程中先执行完整的 patch/contract 测试编译，再执行正式 release 编译，导致同一次 release 出现两轮大规模 Rust 编译**

本文件用于记录问题成因、影响，以及建议的优化方向。

---

# 2. 问题一：重复上传两个接近相同大小的 Artifact

本次 workflow 最终产生了两个约 285 MB 的 artifact：

```text
patched-codex-cli-release-rust-v0.149.1-native-join-p7
patched-codex-cli-bundle-rust-v0.149.1-native-join-p7
```

虽然二者的 SHA-256 digest 不同，因此不是字节级完全相同的压缩包，但它们都包含了同一份占据绝大多数体积的 `codex.exe`。

大致结构为：

```text
bundle/
├── bin/
│   └── codex.exe
├── contract-result.json
├── build-environment.txt
├── resolution.json
├── SHA256SUMS
└── sccache stats...

release-assets/
├── <正式命名的 codex.exe>
├── manifest / descriptor
├── patch payload
├── checksums
└── 其他 release 元数据
```

其中真正占空间的是：

```text
codex.exe ≈ 285 MB
```

因此 GitHub Actions UI 中两个 artifact 最终都显示为约 285 MB。

## 2.1 当前上传逻辑

当前 workflow 同时存在：

```yaml
- name: Upload release candidate assets
  uses: actions/upload-artifact@...
  with:
    name: patched-codex-cli-release-${{ ... }}
    path: ...
```

和：

```yaml
- name: Upload production build bundle
  uses: actions/upload-artifact@...
  with:
    name: patched-codex-cli-bundle-${{ ... }}
    path: ...
```

其中：

- `release-*` 会被后续 `publish` job 下载，并最终上传到 GitHub Release；
- `bundle-*` 当前主要是构建/调试/审计信息，不参与后续正式发布。

因此目前实际发生的是：

```text
Build
 │
 ├── bundle/bin/codex.exe ───────────────┐
 │                                       │
 └── release-assets/<codex.exe copy> ────┤
                                         │
             upload-artifact 两次         │
                                         ▼
                    Actions Artifact Storage
```

---

## 2.2 影响

主要问题包括：

- 每次 release 多上传约 285 MB；
- 增加 GitHub Actions artifact 存储；
- 增加上传网络 I/O；
- 增加 workflow 尾部执行时间；
- Actions 页面中出现两个看起来几乎一样的 artifact，容易误导维护者；
- `bundle-*` 当前没有后续消费者，因此收益很低。

---

# 3. Artifact 优化方案

## 3.1 删除冗余的 `bundle-*` Artifact 上传

建议删除：

```yaml
- name: Upload production build bundle
  uses: actions/upload-artifact@...
  with:
    name: patched-codex-cli-bundle-${{ ... }}
    path: |
      ...
```

保留：

```text
patched-codex-cli-release-*
```

用于：

```text
build job
    │
    ▼
upload-artifact
    │
    ▼
publish job
    │
    ▼
download-artifact
    │
    ▼
gh release upload
```

这样仍然可以正常完成跨 job 的正式发布。

---

## 3.2 Bundle 保持为 Runner 内部临时目录

优化后的数据流：

```text
                  build runner
                       │
        ┌──────────────┴──────────────┐
        │                             │
        ▼                             ▼
     bundle/                    release-assets/
        │                             │
  本地验证/元数据                  upload-artifact
        │                             │
        X                             ▼
   不上传保存                     publish job
                                      │
                                      ▼
                                 GitHub Release
```

即：

- `bundle/` 仅作为当前 runner 内的中间构建目录；
- `release-assets/` 才作为 job 间传输内容；
- 不再保存第二份携带同一个 `codex.exe` 的临时 artifact。

---

# 4. 问题二：同一次 Release 出现两轮大规模 Rust 编译

本次 run 中两个耗时最明显的步骤为：

```text
Run patch generation and contract tests      27m 23s
Build canonical patched Codex CLI bundle     20m 42s
```

二者合计：

```text
48m 05s
```

而整个 workflow 总耗时约：

```text
51m 24s
```

因此几乎整个 release 时间都消耗在这两个步骤中。

---

# 5. 第一轮编译：Patch Generation + Contract Tests

workflow 中首先调用类似：

```bash
bash scripts/build_patched_codex_bundle.sh tests ...
```

该阶段最终会进入：

```text
run_patch_contract.py --phase tests
```

问题在于这里所谓的 “contract tests” 并不是单纯的轻量 patch contract 校验。

当前 `test-contract.json` 中包含大量真实的 Rust 编译和测试操作，例如：

```bash
cargo test -p codex-app-server-protocol ...
cargo test -p codex-tui ...
cargo test -p codex-core ...
cargo test -p codex-core ...
cargo test -p codex-tui --lib ...
...
cargo clippy -p codex-tui --lib --tests -- -D warnings
```

因此第一阶段实际执行的是：

```text
Apply Patch
   │
   ├── 编译 codex-app-server-protocol tests
   ├── 编译 codex-core tests
   ├── 编译 codex-tui tests
   ├── 编译 integration tests
   ├── 编译 install-context tests
   └── 执行 clippy
```

这已经相当于一次完整或接近完整的 Codex Rust CI 验证过程。

因此它耗时约 27 分钟并不奇怪。

---

# 6. 第二轮编译：正式 Windows Release Build

紧接着 workflow 又执行：

```bash
bash scripts/build_patched_codex_bundle.sh build ...
```

该阶段最终进入：

```text
run_patch_contract.py --phase build
```

虽然通过：

```text
--resume <test-report>
```

复用了第一阶段的测试结果，没有再次运行同样的测试命令，但是正式构建本身仍然需要执行：

```bash
cargo build \
  -p codex-cli \
  --bin codex \
  --release \
  --target x86_64-pc-windows-msvc
```

在当前 Linux + xwin 构建模式下，大致等价于：

```bash
cargo xwin build \
  -p codex-cli \
  --bin codex \
  --release \
  --target x86_64-pc-windows-msvc
```

因此会产生第二轮完整的 Rust 编译。

---

# 7. 为什么第一轮编译成果不能直接用于第二轮

表面上看两个阶段都在“编译 Codex”，但它们实际上属于不同的 Rust compilation domain。

## 第一阶段

主要是：

```text
Host:
x86_64-unknown-linux-gnu

Profile:
debug / test

Configuration:
cfg(test)
test harness
Linux linker
Linux dependencies
test-specific compilation units
```

输出通常位于类似：

```text
cargo-target/debug/
```

---

## 第二阶段

正式发布是：

```text
Target:
x86_64-pc-windows-msvc

Profile:
release

Configuration:
non-test
Windows MSVC ABI
xwin linker/runtime
release codegen
different rustc flags
```

输出类似：

```text
cargo-target/x86_64-pc-windows-msvc/release/
```

因此：

```text
Linux debug/test objects
          │
          X
          │ 不能直接转化
          ▼
Windows MSVC release codex.exe
```

Cargo 无法直接复用第一阶段的 `.o` / `.rlib` 作为第二阶段正式产物。

---

# 8. SCCACHE 当前也进行了人为隔离

目前 workflow 中类似存在：

```text
SCCACHE_TEST_DIR
SCCACHE_RELEASE_DIR
```

测试阶段使用：

```text
sccache-test
```

正式发布阶段使用：

```text
sccache-release
```

因此缓存结构类似：

```text
Linux / debug / tests
        │
        ▼
   sccache-test


Windows / MSVC / release
        │
        ▼
  sccache-release
```

这进一步阻止了不同阶段偶尔可以复用的少量 compilation cache entry。

但是：

> 不建议仅仅通过把两个 `SCCACHE_DIR` 粗暴合并来解决问题。

原因是两边的：

- target
- profile
- rustc 参数
- cfg
- linker
- codegen 配置

本身差别很大。

即便共享同一个 sccache CAS，也不会让 Linux test 编译直接变成 Windows release 构建。

因此 SCCACHE 合并最多属于次要优化，而不是根治方案。

---

# 9. 根本问题：Validation CI 与 Release Build 职责混在了一起

当前 `release-patched-codex.yml` 同时承担了：

```text
Patch generation
Patch correctness validation
Contract tests
Cargo tests
TUI tests
Integration tests
Clippy
Schema / metadata generation
Windows MSVC release build
Packaging
Artifact upload
GitHub Release publish
```

这意味着：

```text
                 Release workflow
                        │
                        ▼
                   Apply patch
                        │
                        ▼
             Full validation / tests
                  Linux/debug
                     ~27m
                        │
                        ▼
                  test report
                        │
                        ▼
           Windows MSVC release build
                     ~21m
                        │
                        ▼
                    codex.exe
```

因此每次手动触发正式发布，都必须重新支付完整测试编译的成本。

---

# 10. 推荐架构：将 Validation 与 Release 分离

建议将 workflow 分成两个逻辑职责。

---

## 10.1 Patch Validation CI

在：

```text
push
pull_request
patch 修改
contract 修改
payload 修改
```

时运行完整验证：

```text
┌─────────────────────────────┐
│ Patch Validation CI         │
│                             │
│ apply patch                 │
│ contract verification       │
│ cargo test                  │
│ clippy                      │
│ integration tests           │
│ patch-specific tests        │
└──────────────┬──────────────┘
               │
               ▼
      Validation Evidence
```

它负责证明：

> 某一个确定的 CSA commit + upstream Codex version + patch payload + contract 配置已经通过完整测试。

---

# 11. Validation Evidence

为了避免 Release workflow 直接相信“某个旧测试曾经成功”，Validation CI 应生成可验证的 evidence。

建议至少包含：

```text
CSA repository commit SHA
upstream Codex version
upstream Codex commit SHA
compat_id
patch manifest hash
patch payload hash / tree hash
test-contract.json hash
toolchain information
validation workflow version
validation timestamp
test result
clippy result
```

例如：

```json
{
  "schema_version": 1,
  "csa_commit": "...",
  "upstream_codex_commit": "...",
  "compat_id": "rust-v0.149.1-native-join-p7",
  "patch_manifest_sha256": "...",
  "test_contract_sha256": "...",
  "validation": {
    "tests": "passed",
    "clippy": "passed"
  }
}
```

Release workflow 必须重新计算相关 hash，并确认完全一致。

这样可以避免：

```text
Patch A 测试通过
       │
       ├── 后来 patch 被修改
       ▼
Patch B 没测试却直接 release
```

---

# 12. Patched Codex Release Workflow

Release workflow 的职责应尽量缩小为：

```text
workflow_dispatch
       │
       ▼
Resolve version
       │
       ▼
Verify validation evidence
       │
       ▼
Verify:
- CSA commit
- upstream commit
- manifest hash
- patch hash
- contract hash
       │
       ▼
Apply patch
       │
       ▼
cargo xwin build --release
       │
       ▼
codex.exe
       │
       ▼
Package
       │
       ▼
Upload temporary release artifact
       │
       ▼
Publish GitHub Release
```

即正式 Release 只进行 **一次真正的大规模 production compilation**。

---

# 13. 推荐最终结构

```text
                  ┌──────────────────────────────┐
                  │      Patch Validation CI     │
                  │                              │
push / PR ───────►│ apply patch                  │
                  │ cargo test                   │
                  │ clippy                       │
                  │ contract tests               │
                  │ integration tests            │
                  └──────────────┬───────────────┘
                                 │
                                 ▼
                     Validation Evidence
                                 │
                                 │ exact SHA/hash binding
                                 ▼
                    ┌──────────────────────────┐
workflow_dispatch ─►│ Patched Codex Release    │
                    │                          │
                    │ verify evidence          │
                    │ verify hashes            │
                    │ apply patch               │
                    │                          │
                    │ cargo xwin build          │
                    │      --release            │
                    │          │                │
                    │          ▼                │
                    │      codex.exe            │
                    │          │                │
                    │          ▼                │
                    │       package             │
                    │          │                │
                    │          ▼                │
                    │ GitHub Release            │
                    └──────────────────────────┘
```

---

# 14. 当前实现中不能直接删除 Test Step 的原因

当前 build phase 对 test report 存在硬依赖。

大致逻辑类似：

```text
tests phase
    │
    ▼
test-report.json
    │
    ▼
build phase --resume test-report.json
```

并且 build 脚本会检查：

```text
test report 是否存在
cargo target 是否存在
output 是否尚未生成
```

因此如果直接从 workflow 删除：

```yaml
- name: Run patch generation and contract tests
```

而不修改：

```text
build_patched_codex_bundle.sh
run_patch_contract.py
```

则 `Build canonical patched Codex CLI bundle` 很可能直接失败。

所以正确改法不是：

```text
删掉 tests step
```

而是：

```text
解除 release build 对“同一 runner 刚刚生成的 test report”的强耦合
                     │
                     ▼
改成验证来自 Validation CI 的可信 evidence
```

---

# 15. 建议的具体修改范围

至少需要检查或修改：

```text
.github/workflows/release-patched-codex.yml
```

以及与之相关的：

```text
scripts/build_patched_codex_bundle.sh
scripts/run_patch_contract.py
scripts/compat_release.py
payload/codex/<compat-id>/test-contract.json
```

如果建立独立 CI，建议新增或重构为：

```text
.github/workflows/validate-patched-codex.yml
.github/workflows/release-patched-codex.yml
```

---

# 16. 推荐实施顺序

## Phase 1：去掉 Artifact 重复

优先做低风险修改：

```text
删除 Upload production build bundle
保留 Upload release candidate assets
```

预期收益：

- 每次 release 少约 285 MB artifact 上传；
- 减少 artifact storage；
- 减少 Actions 页面冗余；
- 不影响 publish job。

---

## Phase 2：建立独立 Validation Workflow

将：

```text
cargo test
cargo clippy
contract tests
patch-specific tests
```

从 release workflow 中抽出。

Validation workflow 必须绑定：

```text
CSA commit
upstream Codex commit/version
patch hash
manifest hash
contract hash
```

---

## Phase 3：生成 Validation Evidence

Validation 成功后产生可验证的：

```text
validation-result.json
```

或者等价的 provenance / manifest。

Release workflow 不重新执行完整测试，而是验证 evidence。

---

## Phase 4：简化 Release Workflow

最终 release 路径：

```text
Resolve
  ↓
Verify evidence
  ↓
Apply patch
  ↓
Production release build
  ↓
Pack
  ↓
Publish
```

只保留一次大规模 Rust compilation。

---

## Phase 5：重新审视缓存

在 workflow 职责分离完成之后，再针对：

```text
Cargo registry
Cargo git
target
sccache
xwin
Rust toolchain
```

分别调整缓存。

不要以“强行让 test 和 release 共用 target”为目标。

正确目标是：

> 同一类 compilation 在不同 CI run 之间获得稳定、高命中的缓存复用。

例如：

```text
validation cache
  └── Linux host + tests

release cache
  └── x86_64-pc-windows-msvc + release
```

两种缓存可以独立维护。

这通常比在同一次 workflow 内试图让 Linux debug/test object 被 Windows release build 复用更加合理。

---

# 17. 预期耗时变化

本次实际数据：

```text
Run patch generation and contract tests      27m 23s
Build canonical patched Codex CLI bundle     20m 42s
---------------------------------------------------
合计                                         48m 05s

整个 workflow                               51m 24s
```

当前结构：

```text
Prepare
   ↓
Full validation/test compilation   ~27m
   ↓
Production release compilation     ~21m
   ↓
Pack / upload / publish
```

优化后：

```text
Release workflow:

Prepare
   ↓
Validation evidence verification
   ↓
Production release compilation
   ↓
Pack / upload / publish
```

如果其它条件相近，正式 release 的 wall-clock time 有机会从约：

```text
51 分钟
```

下降到大约：

```text
20～25 分钟量级
```

实际时间仍取决于：

- GitHub runner 性能；
- upstream Codex dependency graph；
- sccache 命中率；
- Cargo registry/git cache；
- xwin cache；
- patch 是否触发核心 crate 大规模失效。

因此不能保证固定数字，但可以确定：

> 当前 27 分钟左右的完整 Validation/Test 阶段可以不再重复发生在每一次正式 Release 中。

---

# 18. 优化后的职责边界

## Validation CI 负责

```text
代码正确性
Patch 正确性
Contract 正确性
Cargo tests
Clippy
Integration tests
Patch-specific tests
生成 validation evidence
```

## Release CI 负责

```text
确认发布输入与已验证输入完全一致
Production build
Packaging
Checksums
Artifact transport
GitHub Release publication
```

这两个职责不应重新混为一个长时间的单体 workflow。

---

# 19. 验收标准

完成优化后，应满足以下条件。

## Artifact

- [x] GitHub Actions 中不再出现两个都包含完整 `codex.exe` 的临时 artifact
- [x] `bundle-*` 不再作为 GitHub Actions artifact 上传
- [x] `release-*` 仍可正常被 publish job 下载
- [x] GitHub Release 中正式资产完整
- [x] checksum / manifest / descriptor 不受影响

## Validation

- [x] Patch 修改后仍会触发完整测试
- [x] `cargo test` 不被取消
- [x] `clippy` 不被取消
- [x] contract tests 不被取消
- [x] validation evidence 与 exact commit/hash 绑定
- [x] 未验证的 patch 无法进入正式 release

## Release

- [x] Release workflow 不再执行完整 Linux debug/test 编译
- [x] Release workflow 只有一次主要 production Rust compilation
- [x] 正式目标仍为 `x86_64-pc-windows-msvc`
- [x] patched Codex CLI 输出保持一致
- [x] 不构建 Codex App 或其它不需要的组件
- [x] workflow_dispatch 手动发布仍正常
- [x] build → publish job 传输仍正常

## Cache

- [x] Cargo registry/git cache 保持
- [x] xwin cache 保持
- [x] release sccache 保持
- [x] validation sccache 保持
- [x] 不通过破坏 cache key 来换取表面上的 workflow 简化
- [x] 不要求 Linux test 与 Windows release 强制共享不可复用的 target objects

## 托管验收记录（2026-08-28）

- 验收提交：`908fa3865f21208a5e0a7d0cec0a1740a515697b`（`main`）。
- Validation：run `33106174014` 成功，完整 patch/Cargo/Clippy/contract 合同通过，耗时 `29m42s`；只上传 `4005` 字节的 `patched-codex-validation-rust-v0.149.0-native-join-p3` evidence artifact。
- Release 演练：run `33108684269` 成功，复验上述 exact-SHA evidence 后只执行一次 canonical production build，完成 finalized manifest、clean-source reverify、asset pack 与 CLI-only guard；build job 耗时 `45m15s`。
- Release 演练只上传一个 transfer artifact：`patched-codex-cli-release-rust-v0.149.0-native-join-p3`，大小 `298912369` 字节；不存在 `bundle-*` artifact。
- 本次按授权使用 `publish=false`：publish job 明确为 `skipped`，演练前后 GitHub Release/草稿列表一致，没有新建或修改 Release。publish job 的下载、远端资产核对和幂等发布逻辑保持不变，并由本地 workflow 回归测试覆盖。

---

# 20. 最终目标

优化前：

```text
             Release
                │
                ▼
         Apply patched Codex
                │
                ▼
        Full Test Compilation
              ~27m
                │
                ▼
       Production Compilation
              ~21m
                │
                ▼
     bundle artifact ~285 MB
                +
     release artifact ~285 MB
                │
                ▼
             Publish
```

优化后：

```text
       Patch / PR
           │
           ▼
   Validation Workflow
           │
      complete tests
           │
           ▼
   Validation Evidence
           │
           │ exact SHA/hash match
           ▼
   Release Workflow
           │
           ▼
    Production Build
      only once
           │
           ▼
      release-assets
           │
   one temporary artifact
           │
           ▼
      GitHub Release
```

核心原则是：

> **完整测试只做一次，正式构建只做一次，大型二进制只上传一份。**

这样既保留 Patch 的严格验证，又避免当前 Release workflow 中明显的重复计算、重复存储和重复网络传输。
