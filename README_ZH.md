<div align="center">

# CSA

在不替换官方安装的前提下，安装并切换按版本固定的 patched Codex CLI。

[![CI](https://github.com/DSLZL/CSA/actions/workflows/ci.yml/badge.svg)](https://github.com/DSLZL/CSA/actions/workflows/ci.yml)
[![CSA release](https://img.shields.io/github/v/release/DSLZL/CSA?filter=v%2A&label=CSA)](https://github.com/DSLZL/CSA/releases)
[![npm](https://img.shields.io/npm/v/%40dslzl%2Fcsa)](https://www.npmjs.com/package/@dslzl/csa)
[![Patched Codex](https://img.shields.io/badge/patched%20Codex-0.151.0%20p10-white)](https://github.com/DSLZL/CSA-codex/releases/tag/compat-rust-v0.151.0-native-join-p10)

[快速开始](#快速开始) · [工作原理](#工作原理) · [命令](#命令) · [文档](#文档) · [English](README.md)

</div>

CSA 是用于管理 patched Codex CLI 的 Rust 工具。它会检测本机的官方 Codex runtime，列出匹配的正式 Release，下载用户选择的可执行文件，验证身份和 checksum，然后通过受管的 `codex` shim 激活。

官方 Codex package、配置、认证、会话和本地数据库都保留在原位。

> [!IMPORTANT]
> 当前 Manager 版本是 `0.1.8`。当前正式 patched Release 是 Codex `0.151.0` p10，共发布六个平台产物；正式 runtime 验收目前覆盖 Windows x64。

## 补丁增加了什么

- `join_agent` 用一次工具调用等待一个精确的 child run。
- `join_agents` 等待一组固定的精确 run，并按请求顺序返回结果。
- TUI 会保留实时和已经完成的子代理活动，新任务不会合并到旧面板。
- Text、Sixel 和 Kitty Orbit renderer 支持动画与 reduced-motion 模式。
- State database migration 检查兼容已知的跨主机换行 checksum 差异。

补丁负责改变 Codex 行为。CSA 本体负责安装、验证、激活、回退和移除。

## 环境要求与支持范围

npm 分发包需要 Node.js 18 或更高版本，并且本机已经有可正常运行的官方 Codex CLI。

| 产品 | 当前版本 | 已发布平台 |
| --- | --- | --- |
| CSA Manager | `0.1.8` | Windows x64、Linux x64、Linux arm64、macOS x64、macOS arm64 |
| Patched Codex CLI | [`rust-v0.151.0-native-join-p10`](https://github.com/DSLZL/CSA-codex/releases/tag/compat-rust-v0.151.0-native-join-p10) | Windows x64/arm64、Linux x64/arm64 musl、macOS x64/arm64 |

Manager 支持某个平台，不代表该平台一定有 patched Codex 产物。在线安装要求官方 Codex 版本精确匹配，并会将 Linux Manager target 解析到已发布的 musl 产物。

## 快速开始

### 1. 安装 Manager

```powershell
npm install --global @dslzl/csa@0.1.8
csa --version
```

也可以不做全局安装：

```powershell
npx @dslzl/csa@0.1.8 --version
bunx @dslzl/csa@0.1.8 --version
```

`npx --yes` 只会跳过 npm 的 package 安装确认，不会代替你操作 CSA 的版本选择器。需要 CSA 无交互选择推荐版本时，请使用 `csa install --yes`。

> [!NOTE]
> 安装 `@dslzl/csa` 只会提供 `csa` 命令。安装 package 时不会下载 patched Codex、修改 `PATH`、创建 `codex` shim 或改动官方 package。

### 2. 诊断并安装

```powershell
csa doctor
csa install
csa status
```

在交互式终端中，`install` 会先按当前 target 和官方 Codex 版本筛选 Release，再打开固定五行的选择器。可以使用方向键、翻页键、Home/End 或搜索，按 Enter 确认。Escape 或 Ctrl+C 会在下载大型可执行文件之前取消，并以状态码 130 退出。

`csa install --yes`、`--json` 和非交互 stream 会自动选择 numeric `-pN` 修订号最大的唯一匹配项。

安装不需要登录 GitHub。CSA 通过公开 Git refs 发现 Release，根据网络环境选择 GitHub 直连或固定的国内镜像池，对精确的 Release 产物进行分片测速，再按实测顺序下载。无论使用哪个来源，size 和 SHA-256 校验都不会省略。

### 3. 确认实际运行的 Codex

在 Windows 上，`install` 会先把受管的 `bin` 目录放到用户 `PATH` 最前面。如果更高优先级的系统条目仍然抢占命令，CSA 会请求 UAC，在 Program Files 下安装自己的 dispatcher，并把这个受保护目录放到系统 `PATH` 最前面。它不会改写 npm、Bun 或 pnpm 的启动器。

安装后请关闭所有终端窗口，并完全退出 VS Code 等承载终端的应用，再重新打开。只在同一个 VS Code 窗口中新建集成终端还会继承旧环境。

```powershell
csa status
where.exe codex
Get-Command codex -All
codex --version
```

`where.exe codex` 的第一项应当是 CSA 自有的 `codex.exe`。patched 版本激活时，`codex --version` 会输出 `codex-cli X.Y.Z (CSA <compat-id>)`。只有系统 `PATH` 原本会抢占命令时 CSA 才请求管理员权限；拒绝授权会让激活失败并回滚。

### 4. 固定较旧的兼容修订

```powershell
csa install --compat rust-v0.150.1-native-join-p8
```

指定的 Release 仍须匹配本机官方 Codex 版本和当前 target。CSA 不会为了满足 compatibility 而降级或覆盖官方 Codex。

### 5. 移除 CSA 受管状态

```powershell
csa uninstall
where.exe codex
Get-Command codex -All
codex --version
npm uninstall --global @dslzl/csa
```

`uninstall` 会移除受管 shim、prepared installation、state，以及 CSA 精确添加的用户和系统 dispatcher `PATH` 项。Windows 可能会再次请求 UAC 来清理 Program Files dispatcher；官方 Codex 和用户数据不会被删除。

## 工作原理

```text
官方 Codex 安装，只读
          |
          | 发现并记录指纹
          v
      CSA Manager
          |
          | 验证、prepare、plug
          v
<manager-root>/bin/codex
    | binding 有效   -> patched Codex + 官方 runtime
    | binding 无效   -> 官方 Codex launcher
```

CSA 把四个身份分开处理：

| 组件 | 作用 |
| --- | --- |
| 官方 Codex | 现有配置、认证、用户状态、runtime 工具和回退 |
| CSA Manager | 发现、验证、安装、状态检查、激活和移除 |
| Patched Codex | Manager 受管目录中按版本固定的 Native Join 与 TUI 修改 |
| 受管 shim | 每次启动前重新验证 binding，再选择 patched 或官方 Codex |

Windows 下可选的 Program Files dispatcher 是受管 shim 的受保护副本，不是另一套 Codex 安装。

在线安装只下载 `SHA256SUMS`、`compatibility-release.json` 和当前 target 的可执行文件。完整 patch、source contract 和正式 compatibility Release 由 [`DSLZL/CSA-codex`](https://github.com/DSLZL/CSA-codex) 维护。

通过 shim 正常启动时会复用当前 `CODEX_HOME`。测试和验收应使用 `csa exec --isolated` 与一次性目录。

完整的信任边界、下载、runtime overlay、数据库和发布模型见 [CSA 架构](docs/architecture.md)。

## 命令

| 命令 | 用途 |
| --- | --- |
| `csa doctor` | 诊断官方 Codex、prepared state、激活和命令优先级 |
| `csa install` | 选择、验证、prepare 并激活正式 Release 或精确本地 payload |
| `csa uninstall` | 撤回 shim 并删除 prepared installation |
| `csa prepare` | 验证精确的本地 artifact 或 source payload，但不激活 |
| `csa plug` | 激活已验证的 prepared state |
| `csa unplug` | 移除 shim，同时保留 prepared state |
| `csa status` | 报告安装、激活、命令解析和 drift |
| `csa purge` | 删除所有 Manager 受管数据 |
| `csa exec --isolated` | 使用明确的隔离路径和 evidence 运行 prepared artifact |

Human 输出跟随检测到的系统语言。`zh` locale 统一使用简体中文，其他 locale 使用英文。把 `--json` 放在命令前或命令后，都可以获得稳定的机器可读报告。

完整语法、状态值、退出码、路径、平台和常见错误见 [CLI 参考](docs/reference.md)。

## 文档

- [操作与故障排查](docs/operations.md)
- [CLI 与平台参考](docs/reference.md)
- [架构与安全模型](docs/architecture.md)
- [开发与隔离测试](docs/development.md)
- [Manager 发布流程](docs/release.md)
- [Patched Codex 生产仓库](https://github.com/DSLZL/CSA-codex)
- [Manager support matrix](release/support-matrix.json)

轻量级 [Ratatui UI harness](https://github.com/DSLZL/CSA-codex/tree/main/tests/ui) 由 patched Codex 生产仓库一并维护。

## 友链

- [LINUX DO](https://linux.do/)
