<div align="center">

# CSA

一个 fail-closed 管理器，在不替换官方 Codex CLI 的前提下运行绑定特定版本的 patched CLI。

[![CI](https://github.com/DSLZL/CSA/actions/workflows/ci.yml/badge.svg)](https://github.com/DSLZL/CSA/actions/workflows/ci.yml)
[![CSA release](https://img.shields.io/github/v/release/DSLZL/CSA?filter=v%2A&label=CSA)](https://github.com/DSLZL/CSA/releases)
[![Patched Codex](https://img.shields.io/badge/patched%20Codex-0.149.0%20accepted-white)](https://github.com/DSLZL/CSA/releases/tag/compat-rust-v0.149.0-native-join-p3)

[快速开始](#快速开始) · [工作原理](#工作原理) · [兼容状态](#兼容状态) · [命令](#命令) · [开发](#开发) · [English](README.md)

</div>

CSA 为 Codex 加入原生子代理 Join 和实时子代理视图，但不会替换官方 CLI。管理器会找到本机的 Codex 安装，校验 runtime 文件，并把 patched executable 放进独立的受管目录。

> [!IMPORTANT]
> CSA 管理器 `0.1.2` 通过 `@dslzl/csa` 和 GitHub `v0.1.2` Release 分发。Patched Codex `0.149.0` p3 是当前已接受并发布的 Windows x64 版本；`0.149.1` p6 和 p7 仍是只开放构建的候选版本。

## 补丁改了什么

- `join_agent` 用一次工具调用等待一个精确的 child run 结束。
- `join_agents` 等待多个精确 run，并按请求顺序返回结果。
- Native Join 处于 pending 状态时，Parent 不需要反复查询 child status。
- TUI 可以显示子代理的实时进度和完成状态，并支持跳转到对应的子会话。
- Patched executable 复用官方 Codex runtime package 和 companion tools，不携带第二套副本。

`0.149.1` p7 候选版本还包含最新的子代理面板和终端无损 Orbit 动画。它目前不是正式的 compatibility release。

## 工作原理

```text
官方 Codex 安装（只读）
          │
          │ 发现并校验指纹
          ▼
      CSA 管理器
          │ prepare + plug
          ▼
<manager-root>/bin/codex
    ├─ binding 有效 -> patched codex.exe + 官方 runtime
    └─ binding 无效 -> 官方 Codex launcher
```

CSA 把四个部分分开管理：

| 组件 | 所有者 | 用途 |
| --- | --- | --- |
| 官方 Codex | OpenAI package manager 安装 | 配置、认证、runtime helpers 和安全回退 |
| CSA 管理器 | CSA | 验证、prepare、激活、状态检查和卸载 |
| Patched Codex | CSA 受管目录 | 绑定版本的 Native Join 与 TUI 修改 |
| `codex` shim | CSA 受管目录 | 选择已验证的 patched binary，验证失败时回退到官方 Codex |

管理器不会覆盖官方文件、复制用户的 Codex Home 或修改 `PATH`。通过 shim 正常启动时，patched Codex 会像官方 Codex 一样继承当前的 `CODEX_HOME`、配置、认证信息、工作目录和终端环境。

## 兼容状态

| Compatibility | Codex | Target | 状态 |
| --- | --- | --- | --- |
| [`rust-v0.149.0-native-join-p3`](https://github.com/DSLZL/CSA/releases/tag/compat-rust-v0.149.0-native-join-p3) | `0.149.0` | Windows x64 | 已接受并发布 |
| `rust-v0.149.1-native-join-p6` | `0.149.1` | Windows x64 | 候选，未开放发布 |
| `rust-v0.149.1-native-join-p7` | `0.149.1` | Windows x64 | 候选，未开放发布 |
| `rust-v0.149.1-native-join-p8` | `0.149.1` | Windows x64 | 候选，未开放发布 |

[兼容索引](release/compatibility-index.json)是仓库 payload 的权威来源。正常在线安装会发现所有正式 `compat-*` GitHub Release；只开放构建的候选版本不会出现在列表里。

在线安装仍然严格失败关闭。CSA 会列出所有正式 patched Release，但只有 target、Codex 版本与当前管理器及本机只读官方 runtime 完全匹配的条目才能选择。选中后还会重新验证 tag、upstream commit、manifest、size 和 SHA-256。

## 前置条件

- 本机已安装 CSA 能够发现的官方 Codex CLI。
- 当前已接受的 patched Codex target 仅支持 Windows x64。
- 从源码构建当前管理器或 patched payload 时需要 Rust `1.95.0`。
- 从 npm 安装管理器时需要 Node.js `18` 或更高版本。

公开 GitHub API 请求默认不需要认证。如果公共限额已耗尽，可只为当前 `csa install` 进程设置 `GITHUB_TOKEN` 或 `GH_TOKEN`；CSA 只会把它发送给 `api.github.com`，不会保存。

## 快速开始

### 获取管理器

使用 npm 安装标准 CLI：

```powershell
npm install --global @dslzl/csa
csa --version
```

[Releases 页面](https://github.com/DSLZL/CSA/releases)也提供预编译的管理器压缩包和 `SHA256SUMS`。下载对应平台的压缩包，校验后解压出 `csa` executable。

也可以构建当前源码：

```powershell
git clone https://github.com/DSLZL/CSA.git
Set-Location CSA
cargo build --release --locked

$Manager = (Resolve-Path '.\target\release\csa.exe').Path
& $Manager --version
```

> [!NOTE]
> npm package 只暴露 `csa`，安装 package 时不会替换 `codex`，也不会自动激活 patched build；仍需显式执行 `csa install`。

### 验证并安装

测试时建议传入明确的 manager root，便于检查和清理文件：

```powershell
$ManagerRoot = Join-Path $env:LOCALAPPDATA 'CSA\managed'

csa doctor --manager-root $ManagerRoot
csa install --manager-root $ManagerRoot
csa status --manager-root $ManagerRoot
```

在交互式终端中，直接运行 `csa install` 会拉取正式兼容目录，列出 patched Codex 版本及可安装状态，并让用户输入编号。自动化环境必须显式指定完整 ID：

```powershell
csa install --compat rust-v0.149.0-native-join-p3
```

> [!WARNING]
> 只有 target 和 Codex 版本同时匹配当前管理器与本机官方 runtime 的 Release 才能选择。CSA 不会下载或覆盖另一个官方 Codex 版本。本地 payload 模式只用于开发和验收，不能用来绕过版本限制。

开发本地 payload 时，需要传入 manifest，以及一个本地 artifact 或 source 目录：

```powershell
$CompatId = 'rust-v0.149.0-native-join-p3'
$Manifest = Join-Path 'C:\absolute\payload' "$CompatId\manifest.toml"
$Artifact = 'C:\absolute\patched\codex.exe'

& $Manager install --manager-root $ManagerRoot `
  --manifest $Manifest `
  --artifact $Artifact
```

Compatibility 目录名必须与其中的 `compat_id` 一致。Candidate manifest 只能在一次性 payload 副本中 finalize，仓库内提交的 candidate 文件应保持不变。

### 使用 patched CLI

`install` 会创建受管 shim，但不会修改 `PATH`。先只把它加入当前 PowerShell 进程：

```powershell
$ManagedBin = Join-Path $ManagerRoot 'bin'
$env:PATH = $ManagedBin + [IO.Path]::PathSeparator + $env:PATH

Get-Command codex -All
codex --version
codex
```

自动化或一次性测试应使用 `exec --isolated`，不需要激活 shim：

```powershell
& $Manager exec --isolated `
  --manager-root $ManagerRoot `
  --codex-home C:\absolute\isolated\codex-home `
  --cwd C:\absolute\fixture `
  --logs-dir C:\absolute\logs `
  --state-dir C:\absolute\state `
  --record C:\absolute\evidence.json `
  --npm-prefix C:\absolute\npm-prefix `
  -- --version
```

所有隔离目录都必须是绝对规范路径，彼此不能重叠，也不能位于管理器目录或官方 Codex 目录内。

### 卸载

```powershell
& $Manager uninstall --manager-root $ManagerRoot

Get-Command codex -All
codex --version
```

`uninstall` 会移除受管 shim 和管理器自己的 prepare 数据。重复执行是安全的。它不会删除官方 Codex、npm package、用户配置，或用户手动添加的 `PATH` 项。

如果曾把受管 `bin` 目录写入用户 `PATH`，请先确认 `codex` 已回落到官方 launcher，再只删除这一项。

## 命令

| 命令 | 作用 |
| --- | --- |
| `csa doctor` | 检查官方安装和可选 compatibility inputs，不修改状态 |
| `csa install` | 列出正式 patched Release，安装选中的精确匹配项，或接收精确的本地 payload |
| `csa uninstall` | 撤回 shim，移除管理器自己的 prepare 数据 |
| `csa prepare` | 验证或构建精确的本地 payload，但不激活 |
| `csa plug` | 在 `<manager-root>/bin` 中发布绑定 checksum 的 shim |
| `csa unplug` | 撤回 shim，保留 prepared data |
| `csa status` | 报告 prepared state、激活状态和 drift |
| `csa purge` | 移除 shim 以及所有受管的 prepared、source、build 和 state 数据 |
| `csa exec --isolated` | 使用明确的隔离目录运行 prepared Codex binary，并记录 evidence |

运行 `csa --help` 可以查看准确的参数列表。管理器命令返回机器可读的 JSON；参数或验证失败时，会向 stderr 输出结构化错误。

## 安全边界

- 官方 Codex 路径始终位于管理器外部，并保持只读。
- Manifest、source preimages、runtime files、artifacts、state 和 shims 都绑定 checksum。
- 文件缺失、版本漂移、路径重叠或 asset 未验证时，操作会 fail closed。
- Shim 会在启动前重新验证 binding；patched path 不再可信时会回退到官方 Codex。
- 测试使用一次性 Home、工作目录、state、logs、npm prefixes 和 child-only `PATH`。
- 测试 evidence 和 release assets 不得包含认证文件、token、cookie 或完整环境变量。

## 开发

管理器是一个小型 Rust binary。Patched Codex payload 采用数据驱动，并绑定精确的 upstream tag、commit、source hashes、toolchain、target 和 test contract。

运行管理器质量检查：

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --all-targets
```

验证 compatibility 和 release tooling：

```powershell
py -3 validation\validate_replacements.py --repository .
py -3 scripts\test_compat_catalog.py
py -3 scripts\test_verify_release_asset_set.py
py -3 scripts\test_verify_patch_payload.py
py -3 scripts\test_release_tools.py
```

CSA 有两条相互独立的发布流：

- `vX.Y.Z` release 包含管理器和各平台压缩包。
- `compat-<compat_id>` release 包含一个经过评审的 patched Codex compatibility。

CircleCI 负责编译验收 candidate。GitHub Actions 会独立执行 production build，并负责正式发布。两条 pipeline 都不会把对方的 binary 当作发布 authority。

## 文档

- [操作与恢复](docs/operations.md)
- [开发与测试隔离](docs/development.md)
- [兼容与发布流程](docs/release.md)
- [当前发布准备状态](release-readiness.md)
- [兼容目录](release/compatibility-index.json)
- [平台支持矩阵](release/support-matrix.json)

## 友链

- [LINUX DO](https://linux.do/)
