# CSA

CSA 提供一个 fail-closed 的 Rust 管理器和一份绑定具体 Codex 版本的 Native Join 补丁。它与官方 Codex CLI 并排安装，不覆盖官方安装。

当前状态：**发布由托管 Release workflow 门禁决定**。Windows x64 已完成 Codex `0.148.0` candidate 的本地构建和 focused 验证；compatibility workflow 会在发布前重跑完整 contract，CSA workflow 会构建全部五个平台。详见 [release-readiness.md](release-readiness.md)。

[English README](README.md)

## 解决的问题

Patched Codex 允许 Parent 对精确 child run 提交一次原生 Join，并一直等待到该 run 进入 terminal 状态。这样无需客户端反复 wait/status，同时保留 approval、cancellation、replay 和 shutdown 语义。

CSA 新增的 `join_agent` 以普通 function 暴露，因为 provider 会拒绝在保留的 `collaboration` namespace 中增加新工具名；上游 multi-agent 工具仍保留原 namespace。

开发时必须区分两个角色：

```text
Official Codex       运行 Trellis，并作为开发控制面
Patched Codex SUT    只通过绝对路径、isolated exec 或显式 shim 运行
```

安装 `@dslzl/csa` 只暴露 `csa`，且不运行 lifecycle script；npm 安装阶段不会下载、构建、打补丁、激活或修改 PATH/profile。之后用户显式执行 `csa install` 时，管理器会发现官方 CLI，只接受 OpenAI 当前最新的正式 `rust-vX.Y.Z` Release，并从固定的 `dslzl/CSA` 下载对应正式 `compat-<compat_id>` 资产。官方 `codex` 始终保持外部只读且不变。

GitHub Release 分成两条独立流：`vX.Y.Z` 只放 CSA manager/npm 产物，`compat-<compat_id>` 只放 patched Codex 兼容资产。整点 watcher 只把上游 Codex clone 到 runner 临时目录，不会放进 CSA 仓库。

## 已验证范围

| 平台 | 管理器/npm | Patched Codex payload |
| --- | --- | --- |
| Windows x64 | 本地 PASS；`windows-2025` CI 已配置 | `rust-v0.148.0-native-join-p1` focused PASS；Release 必须通过完整 contract |
| Linux x64 | CI 已配置，未验证 | 无 |
| Linux arm64 | CI 已配置，未验证 | 无 |
| macOS x64 | CI 已配置，未验证 | 无 |
| macOS arm64 | CI 已配置，未验证 | 无 |

当前兼容 candidate 固定到上游 tag `rust-v0.148.0`、commit `3ba0f711642a888aec92a611a3f3b2211157ff89`、Rust `1.95.0` 和 `x86_64-pc-windows-msvc`。上游、preimage 或 artifact 发生漂移时会 fail closed。

## 前置条件

- 保留可用的官方 Codex CLI `0.148.0`。
- Node.js `>=18`；CI 覆盖 Node 22、24、26。
- 当前平台的管理器包，以及已发布且哈希匹配的 CSA compatibility Release。目前只有 Windows x64 的 patched target 得到验证。
- 只有从源码构建管理器或 payload 时才需要 Rust `1.95.0`。

包尚未发布。开发阶段只能将本地 tarball 安装到临时 prefix：

```powershell
$Prefix = Join-Path $env:TEMP 'csa-prefix'
npm install --prefix $Prefix --offline --no-audit --no-fund `
  C:\absolute\dslzl-csa-win32-x64-0.1.0.tgz `
  C:\absolute\dslzl-csa-0.1.0.tgz
$Manager = Join-Path $Prefix 'node_modules\.bin\csa.cmd'
& $Manager --version
```

只有发布状态变为 ready 且 registry 中已存在全部平台包之后，才能使用计划中的 `npm install -g @dslzl/csa@0.1.0`。

## 冷安装与隔离运行

管理器发布后，正常用法是：

```powershell
csa install
csa status
```

只有当发现的官方 CLI 版本等于 OpenAI 当前最新的非 draft、非 prerelease `rust-vX.Y.Z` Release，且 CSA 已发布精确匹配的正式兼容 Release 时才会成功。它不会回退到旧 payload；OpenAI 新版刚发布而 CSA 尚未适配时返回 `latest_not_yet_supported`。

离线诊断或本地 payload 开发仍使用绝对路径，并显式传入 manager root：

```powershell
$ManagerRoot = Join-Path $env:LOCALAPPDATA 'csa\managed'
$Manifest = (Resolve-Path '.\payload\codex\rust-v0.148.0-native-join-p1\manifest.toml').Path
$Artifact = 'C:\绝对路径\patched\codex.exe'
$Official = (Get-Command codex -CommandType Application | Select-Object -First 1).Source
$OfficialNative = 'C:\官方 native codex.exe 的绝对路径'

& $Manager doctor --manager-root $ManagerRoot --official $Official `
  --official-native $OfficialNative --manifest $Manifest
& $Manager install --manager-root $ManagerRoot --official $Official `
  --official-native $OfficialNative --manifest $Manifest --artifact $Artifact
& $Manager status --manager-root $ManagerRoot
```

两种 install 模式都复用同一套 prepare 和 plug 事务。无输入模式只下载经过评审的正式 Release manifest、manifest 引用文件和 patched artifact，并验证 release/tag/commit/target/size/SHA-256 后清理下载暂存；传入 `--manifest` 加且仅加一个 `--artifact` 或 `--source` 时进入纯本地诊断模式。两种模式都不修改 PATH。

日常开发使用 isolated exec，不创建 shim，也不修改 PATH：

```powershell
& $Manager exec --isolated --manager-root $ManagerRoot `
  --codex-home C:\绝对路径\isolated-codex-home `
  --cwd C:\绝对路径\fixture `
  --logs-dir C:\绝对路径\logs `
  --state-dir C:\绝对路径\state `
  --record C:\绝对路径\evidence.json `
  --npm-prefix C:\绝对路径\npm-prefix `
  -- --version
```

## 可逆激活

`plug` 只会把管理器复制为 manager 自有 `bin` 目录中的 `codex` shim；`install` 已经调用它，底层命令用于重试和诊断。应先在当前 Shell 或测试子 Shell 中验证：

```powershell
& $Manager plug --manager-root $ManagerRoot
$env:PATH = (Join-Path $ManagerRoot 'bin') + [IO.Path]::PathSeparator + $env:PATH
Get-Command codex -CommandType Application
codex --version

& $Manager uninstall --manager-root $ManagerRoot
Get-Command codex -CommandType Application
codex --version
```

`uninstall` 先撤回 shim，再清除 manager 自有的激活和 prepare 数据。PATH 会自然回落到未修改的官方 launcher；它不卸载 npm 包，也不删除官方 Codex。

## 恢复与卸载

按以下顺序执行：

```powershell
& $Manager uninstall --manager-root $ManagerRoot
Get-Command codex -CommandType Application
codex --version
npm uninstall --prefix $Prefix @dslzl/csa @dslzl/csa-win32-x64
```

如果用户曾手动添加持久 PATH，只能在确认 official fallback 正常后删除 manager `bin` 那一项。恢复流程不得删除或覆盖官方 Codex。

## 文档

- [操作与故障恢复](docs/operations.md)
- [开发与 Trellis 隔离](docs/development.md)
- [兼容更新、发布和 production plug runbook](docs/release.md)
- [当前发布就绪状态](release-readiness.md)

## 安全边界和非目标

- manifest、preimage、artifact、state 和 activation shim 均绑定 SHA-256。
- 缺失、漂移、路径重叠或未验证状态会 fail closed，或安全回落到 official Codex。
- 自动化测试使用 disposable HOME、`CODEX_HOME`、npm prefix、cwd、logs、state 和 child-only PATH。
- 不复制 auth、token、cookie、session 到仓库、日志或发布 artifact。
- 不热替换正在运行的 Codex，不静默修改 profile，不从任意来源下载，也不支持任意 Codex 版本。兼容 Release 只有在对应候选 PR 经评审并合并到默认分支后才允许发布。

许可证为 [MIT](LICENSE)，上游和依赖声明见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。
