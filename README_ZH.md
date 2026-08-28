<div align="center">

# CSA

一个与官方 Codex 安装并存、按版本管理 patched CLI 的 fail-closed 工具。

[![CI](https://github.com/DSLZL/CSA/actions/workflows/ci.yml/badge.svg)](https://github.com/DSLZL/CSA/actions/workflows/ci.yml)
[![CSA release](https://img.shields.io/github/v/release/DSLZL/CSA?filter=v%2A&label=CSA)](https://github.com/DSLZL/CSA/releases)
[![npm](https://img.shields.io/npm/v/%40dslzl%2Fcsa)](https://www.npmjs.com/package/@dslzl/csa)
[![Patched Codex](https://img.shields.io/badge/patched%20Codex-0.150.1%20accepted-white)](https://github.com/DSLZL/CSA/releases/tag/compat-rust-v0.150.1-native-join-p8)

[快速开始](#快速开始) · [当前支持范围](#当前支持范围) · [命令速查](#命令速查) · [开发与文档](#开发与文档) · [English](README.md)

</div>

CSA 为 Codex 加入原生子代理 Join 和实时子代理视图，但不会替换官方 CLI。管理器会验证本机的官方 runtime，从正式 GitHub Release 下载一个精确匹配的 patched compatibility，并把所有受管文件放在独立目录中。

> [!IMPORTANT]
> CSA Manager `0.1.3` 已发布到 `@dslzl/csa` 和 GitHub `v0.1.3` Release。当前正式 patched compatibility 是面向 Windows x64 的 Codex `0.150.1` p8。

## 为什么使用 CSA

CSA 让这些补丁可用，同时避免把官方安装当作可以直接修改的构建目录：

- `join_agent` 用一次工具调用等待一个精确的 child run 结束。
- `join_agents` 等待多个精确 run，并按请求顺序返回结果。
- TUI 会显示子代理的实时活动、完成状态，并支持返回子会话。
- Patched executable 复用经过验证的官方 Codex runtime 和 companion tools。
- 绑定 checksum 的 shim 在 prepared binding 失效时回退到官方 Codex。

管理器不会覆盖官方 Codex 文件、复制默认 `CODEX_HOME`、修改 shell profile 或改写 `PATH`。

## 当前支持范围

| 产品 | 当前版本 | 平台 |
| --- | --- | --- |
| CSA Manager | `0.1.3` | Windows x64、Linux x64、Linux arm64 glibc、macOS x64、macOS arm64 |
| Patched Codex CLI | [`rust-v0.150.1-native-join-p8`](https://github.com/DSLZL/CSA/releases/tag/compat-rust-v0.150.1-native-join-p8) | Windows x64 |

Manager 可用于某个平台，并不代表该平台已有 patched Codex compatibility。[兼容索引](release/compatibility-index.json)是仓库 payload 的权威来源；正常安装则会发现已发布的 `compat-*` Release。

在线安装只接受精确匹配。只有 target 和 Codex 版本同时匹配 Manager 与本机官方 runtime 的 Release 才能选择。激活前还会校验 tag、commit、manifest、文件大小和 SHA-256。

## 快速开始

### 安装 Manager

你需要 Node.js 18 或更高版本，并确保官方 Codex CLI 已经可以正常运行。

```powershell
npm install --global @dslzl/csa@0.1.3
csa --version
```

也可以不做全局安装，直接运行 CLI：

```powershell
npx @dslzl/csa@0.1.3 --version
```

为 `npx` 加上 `--yes` 只会跳过 npm 的安装确认，不会替你选择 patched Codex Release：

```powershell
npx --yes @dslzl/csa@0.1.3 --version
```

[`v0.1.3` Release](https://github.com/DSLZL/CSA/releases/tag/v0.1.3) 还提供预编译的 Manager 压缩包和 `SHA256SUMS`。

> [!NOTE]
> 安装 npm package 只会暴露 `csa` 命令，不会替换 `codex`，也不会在安装 package 时下载 patched build 或激活 shim。

### 检查并安装 patched Codex

在交互式终端中运行：

```powershell
csa doctor
csa install
csa status
```

`csa install` 会按版本顺序列出正式、非草稿、非预发布的 patched Release。无法安装的条目会标出原因，然后让你选择可安装版本。

自动化环境必须传入完整的 compatibility ID：

```powershell
csa install --compat rust-v0.150.1-native-join-p8
```

> [!WARNING]
> CSA 不会为了满足 compatibility 而下载、降级或覆盖官方 Codex。如果没有匹配条目，请自行安装所需的官方 Codex 版本，或等待 CSA 发布对应 compatibility。

### 使用受管 shim

`install` 会创建经过验证的 shim，但不会修改 `PATH`。在 Windows 上，先只在当前 PowerShell 进程中测试：

```powershell
$Status = csa status | ConvertFrom-Json
$env:PATH = $Status.activation.managed_bin + [IO.Path]::PathSeparator + $env:PATH

Get-Command codex -All
codex --version
codex
```

请把官方 Codex launcher 保留在受管目录之后的 `PATH` 中。Binding 失效时，shim 会使用官方 launcher，而不会运行未经验证的 patched executable。

### 移除 CSA 受管状态

```powershell
csa uninstall

Get-Command codex -All
codex --version
npm uninstall --global @dslzl/csa
```

`uninstall` 会撤回 shim 并删除 Manager 自己的 prepared data。它不会删除官方 Codex、用户配置、认证信息、npm 状态或用户手动添加的 `PATH` 项。

## Native Join 与 TUI

当前 p8 patch 包含：

- 精确的单 run 和批量 Native Join 工具；
- 可重放的终止结果，以及保持请求顺序的批量结果；
- 子代理 transport fallback 继承；
- 显示 starting、running、waiting、approval、completed、failed 和 cancelled 工作状态的实时子代理面板；
- 支持 reduced motion 的 text、Sixel 和 Kitty Orbit 渲染。

正式 Windows x64 验收记录覆盖了精确 executable hash、官方 runtime binding、官方文件不变性和一次经过认证的单子代理 Native Join。多子代理 Native Join、Ultra runtime 行为和交互式 TUI 验收仍明确标记为未验证。

如果只想快速查看画面和动画，不想编译 Codex，可运行独立的 [Ratatui UI harness](tests/ui/README.md)。

## CSA 如何工作

```text
官方 Codex 安装，只读
          |
          | 发现并校验指纹
          v
      CSA Manager
          |
          | prepare and plug
          v
<manager-root>/bin/codex
    | binding 有效   -> patched codex.exe + 官方 runtime
    | binding 无效   -> 官方 Codex launcher
```

CSA 把四个身份分开管理：

| 组件 | 所有者 | 作用 |
| --- | --- | --- |
| 官方 Codex | 现有 package-manager 安装 | 配置、认证、runtime 文件和回退 |
| CSA Manager | CSA | 发现、验证、安装、激活、状态检查和移除 |
| Patched Codex | Manager 受管目录 | 绑定版本的 Native Join 与 TUI 修改 |
| `codex` shim | Manager 受管的 `bin` 目录 | 重新验证 binding，并选择 patched 或官方 Codex |

通过 shim 正常启动时，会像官方启动一样继承当前的 `CODEX_HOME`、工作目录、终端和环境。需要隔离的测试必须改用明确的一次性目录。

## 命令速查

| 命令 | 用途 |
| --- | --- |
| `csa doctor` | 检查官方安装和可选 compatibility inputs，不修改状态 |
| `csa install` | 列出正式 Release 并安装一个精确匹配项，或安装精确的本地 payload |
| `csa uninstall` | 撤回 shim，删除 Manager 自己的 prepared state |
| `csa prepare` | 验证或构建精确的本地 payload，但不激活 |
| `csa plug` | 在 `<manager-root>/bin` 中发布绑定 checksum 的 shim |
| `csa unplug` | 撤回 shim，保留 prepared data |
| `csa status` | 报告 prepared state、激活状态、路径和 drift |
| `csa purge` | 删除所有 Manager 受管的 prepared、source、build、shim 和 state 数据 |
| `csa exec --isolated` | 使用明确的隔离目录运行 prepared binary，并记录 evidence |

运行 `csa --help` 可查看完整参数。Manager 命令向 stdout 写入机器可读的 JSON。参数或验证失败时，会向 stderr 写入结构化错误并以退出码 2 结束。

## 开发与文档

Manager 最低需要 Rust `1.89`。当前 Release 和 patched Codex build profile 固定使用 Rust `1.95.0`。

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --all-targets
py -3 scripts\test_validation_evidence.py
py -3 scripts\test_release_tools.py
```

- [操作、恢复与故障排查](docs/operations.md)
- [开发与测试隔离](docs/development.md)
- [兼容与发布流程](docs/release.md)
- [兼容目录](release/compatibility-index.json)
- [Manager 平台支持矩阵](release/support-matrix.json)

CSA 使用两条独立的发布流：`vX.Y.Z` 用于 Manager，`compat-<compat_id>` 用于一个经过评审的 patched Codex compatibility。

## 友链

- [LINUX DO](https://linux.do/)
