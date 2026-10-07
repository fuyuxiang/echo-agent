<div align="center">

# Echo Agent

**拥有长期记忆、可自进化、可嵌入的开源 Agent Runtime**

Echo Agent 为持续工作的 AI 助理与应用提供可自托管的运行时。认知记忆帮助 Agent 跨会话找回相关信息，技能自进化从执行经验中提炼并评估可复用技能，权限策略约束工具调用。它可以在终端运行、通过 Gateway 提供服务，也可以嵌入应用；EchoAgent Desktop 将 Agent 能力带入本地工作台。

[![PyPI](https://img.shields.io/pypi/v/echo-agent)](https://pypi.org/project/echo-agent/)
[![Python](https://img.shields.io/pypi/pyversions/echo-agent)](https://pypi.org/project/echo-agent/)
[![CI](https://github.com/fuyuxiang/echo-agent/actions/workflows/ci.yml/badge.svg)](https://github.com/fuyuxiang/echo-agent/actions/workflows/ci.yml)
[![Docs](https://img.shields.io/badge/docs-online-blue)](https://fuyuxiang.github.io/echo-agent/)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![GitHub Stars](https://img.shields.io/github/stars/fuyuxiang/echo-agent?style=flat)](https://github.com/fuyuxiang/echo-agent/stargazers)

[快速开始](#快速开始) · [运行方式](#运行方式) · [记忆与进化](#长期记忆与技能自进化) · [桌面端](#echoagent-desktop) · [扩展与集成](#扩展与集成) · [文档](#文档) · [English](README.en.md)

</div>

## 快速开始

### Agent Runtime

环境要求：Python 3.11+，以及至少一个可用模型服务的 API Key。安装后通过配置向导添加模型，再启动交互式 Agent：

```bash
# 安装
pip install "echo-agent[all]"

# 配置模型
echo-agent setup

# 在终端启动 Agent
echo-agent run
```

中国大陆网络环境可指定 PyPI 镜像：`pip install "echo-agent[all]" -i https://mirrors.aliyun.com/pypi/simple/`。Windows 可在 PowerShell 中运行上述命令，平台依赖与限制见 [安装指南](https://fuyuxiang.github.io/echo-agent/getting-started/installation/)。

<details>
<summary>源码安装脚本（Linux / macOS / WSL2）</summary>

安装脚本会克隆源码、创建独立环境并运行配置向导。建议先下载并查看脚本，再执行：

```bash
curl -fsSL -o install.sh https://raw.githubusercontent.com/fuyuxiang/echo-agent/master/scripts/install.sh
less install.sh
bash install.sh
```

可用 `bash install.sh --help` 查看选项。

</details>

安装方式、依赖分组与卸载见 [安装指南](https://fuyuxiang.github.io/echo-agent/getting-started/installation/)；全部命令见 [CLI 参考](https://fuyuxiang.github.io/echo-agent/reference/cli/)，配置项见 [配置参考](https://fuyuxiang.github.io/echo-agent/reference/configuration/)。

需要 Dashboard、API、消息通道或定时任务时，可在前台启动 Gateway：

```bash
echo-agent gateway
```

Dashboard 默认地址为 `http://127.0.0.1:58123/`。

### 常驻运行（后台服务）

`echo-agent run` 和 `echo-agent gateway` 默认在前台运行。在 macOS 或使用 systemd 的 Linux 上，可将 Gateway 注册为用户级后台服务：

```bash
echo-agent gateway install
echo-agent gateway status
echo-agent gateway logs --follow
```

Gateway 默认监听 `127.0.0.1:58123`；远程访问可使用 SSH 端口转发。Linux 用户退出登录后继续运行服务，需按系统策略启用 linger。详见 [后台常驻服务](https://fuyuxiang.github.io/echo-agent/operations/background-service/)与 [Gateway 认证](https://fuyuxiang.github.io/echo-agent/integrations/gateway/authentication/)。

### EchoAgent Desktop

桌面端从源码运行需要 Node.js 22 或 24、pnpm 10、Rust 1.92+ 和 `protoc`。macOS 用户在仓库根目录下执行：

```bash
cd client
pnpm setup:mac
pnpm install --frozen-lockfile
pnpm tauri dev
```

Windows 的环境准备与启动命令见 [EchoAgent Desktop 中文文档](client/README.md)。`pnpm test` 运行前端测试，`pnpm build` 检查 TypeScript 并构建前端，`pnpm dist` 生成平台安装包；发布要求见 [发布流程](client/docs/release-workflow.md)与 [自动化平台说明](client/docs/automation-platform-support.md)。

## 运行方式

Echo Agent 提供终端交互、Gateway 服务和嵌入应用三种使用形态：

<div align="center">
  <img src="docs/assets/runtime-modes.png" alt="Echo Agent Runtime 的本地运行、Gateway 服务和嵌入应用三种形态" width="920" />
</div>

| 形态 | 使用入口 | 适用场景 |
| --- | --- | --- |
| **本地运行** | `echo-agent run` | 终端交互、开发调试、单机任务 |
| **Gateway** | `echo-agent gateway` | 长期运行、消息通道、Dashboard、API 与自动化 |
| **嵌入应用** | EchoAgent Desktop | 在宿主应用进程内运行 Agent |

## 长期记忆与技能自进化

### 认知记忆

Agent 可跨会话找回相关的对话摘要、用户偏好和项目事实，减少重复交代背景。记忆按当前任务、会话摘要、长期事实和归档分层管理；混合检索负责召回，环境类记忆按时效衰减，冲突事实经检测后更新。详见 [记忆系统](https://fuyuxiang.github.io/echo-agent/concepts/memory-system/)。

### 技能自进化

Echo Agent 可从任务执行轨迹中提炼可复用的 Skills。候选技能经过内容验证和基线评估：低风险且效果更好的候选可自动晋升，高风险候选进入人工审核；已晋升技能支持手动回滚。详见 [进化与评估](https://fuyuxiang.github.io/echo-agent/concepts/evolution-evaluation/)。

## 运行时能力

Echo Agent Runtime 围绕以下能力组织 Agent 的执行与扩展：

| 能力 | 作用 |
| --- | --- |
| **Agent Loop** | 协调模型推理、工具调用和执行反馈，持续推进任务 |
| **会话与上下文** | 管理任务状态，并为每轮执行组织所需上下文 |
| **模型接入与路由** | 连接多个模型服务，配置路由与故障回退 |
| **工具与 MCP** | 使用本地工具，并连接外部 MCP Server |
| **Skills 与 Plugins** | 加载可复用技能，扩展 Runtime 行为 |
| **权限策略** | 检查工具调用，确认敏感操作并记录执行过程 |
| **自动化与消息通道** | 通过定时任务、Webhook 和消息入口触发 Agent |

例如，你可以在终端分析项目，或让 Gateway 定时整理数据并将结果发送到飞书或 Telegram。组件职责见 [架构概览](https://fuyuxiang.github.io/echo-agent/concepts/architecture/)。

## Gateway 与 Dashboard

Gateway 将完整 Runtime 作为服务运行，对外提供 Dashboard、HTTP / WebSocket API、消息通道、Webhook 和定时任务。CLI / TUI 连接已经运行的 Gateway，使用其中的模型、记忆、工具和权限策略：

```bash
echo-agent cli
echo-agent cli --tui
```

Dashboard 用于查看会话、记忆、技能、知识库、定时任务、消息通道和运行状态。使用方式见 [Dashboard 指南](https://fuyuxiang.github.io/echo-agent/guides/dashboard/)和 [消息通道文档](https://fuyuxiang.github.io/echo-agent/integrations/channels/)。

## EchoAgent Desktop

**EchoAgent Desktop** 将 Echo Agent Runtime 集成到桌面工作台，让 Agent 在本地工作区中调用文件、浏览器和桌面能力完成任务，并呈现执行过程与变更。

<div align="center">
  <img src="client/docs/images/echoagent-home.png" alt="EchoAgent Desktop 工作台首页" width="920" />
</div>

| 场景 | 桌面端能力 |
| --- | --- |
| **代码开发** | 集成 Eclipse Theia IDE 与 Coding Agent，支持任务计划、代码修改、Diff 审阅、验证和交付 |
| **浏览器与电脑操作** | Browser Use 使用任务隔离的浏览器环境；Computer Use 根据屏幕快照操作桌面，敏感操作逐项确认 |
| **工作区与知识** | 管理项目文件、会话、知识库和任务产物，让 Agent 持续使用已有上下文 |
| **模型与扩展** | 选择模型服务，并按需接入 MCP、Skills、Plugins、专家和子 Agent |
| **远程继续任务** | 绑定微信后，可在桌面端运行期间继续会话、查看进展并处理权限请求 |

桌面端可独立于 Gateway 启动。源码构建、平台要求和完整功能见 [EchoAgent Desktop 中文文档](client/README.md)。

## 扩展与集成

将 Echo Agent 接入现有产品时，可选择两种集成方式：

- **进程内嵌入**：宿主应用集成 Runtime，并负责界面、任务状态和本地能力。桌面端提供了可参考的 [应用架构](client/README.md#工作原理)与 [运行时桥接代码](client/src-tauri/src/agent_runtime.rs)。
- **连接 Gateway**：让 Runtime 独立运行，通过 [HTTP / WebSocket API](https://fuyuxiang.github.io/echo-agent/reference/gateway-api/) 与应用通信。

进程内嵌入目前以桌面端集成为参考，具体接口与兼容性以所选源码版本为准。

Echo Agent Runtime 还提供以下扩展点：

| 扩展点 | 用途 | 文档 |
| --- | --- | --- |
| Tools | 增加原生执行能力 | [工具参考](https://fuyuxiang.github.io/echo-agent/reference/tools/) |
| MCP | 连接外部工具服务 | [MCP 集成](https://fuyuxiang.github.io/echo-agent/integrations/mcp/) |
| A2A | 接收入站任务；当前 Agent 运行时不提供 A2A 出站委派入口 | [A2A 集成](https://fuyuxiang.github.io/echo-agent/integrations/a2a/) |
| Skills 与 Plugins | 复用工作流、扩展运行行为 | [Skills](https://fuyuxiang.github.io/echo-agent/integrations/skills/using-skills/) · [Plugins](https://fuyuxiang.github.io/echo-agent/integrations/plugins/using-plugins/) |
| Models | 接入模型服务 | [模型接入](https://fuyuxiang.github.io/echo-agent/guides/models/) |
| Channels | 增加消息入口 | [消息通道](https://fuyuxiang.github.io/echo-agent/integrations/channels/) |

## 数据与安全

Echo Agent 可部署在自有设备或服务器。会话、记忆和配置等状态保存在部署环境中；工具调用受权限策略约束，需要确认的操作会等待用户决策并记录执行过程。

使用远程模型、MCP Server 或第三方消息通道时，交互所需的数据会发送给相应服务。建议为同一工作区只运行一个完整 Runtime 实例；远程访问的配置与边界见 [安全模型](https://fuyuxiang.github.io/echo-agent/concepts/security-model/)和 [安全加固指南](https://fuyuxiang.github.io/echo-agent/operations/security-hardening/)。

## 文档

完整文档：[fuyuxiang.github.io/echo-agent](https://fuyuxiang.github.io/echo-agent/)

| 主题 | 入口 |
| --- | --- |
| 开始使用 | [安装指南](https://fuyuxiang.github.io/echo-agent/getting-started/installation/) · [快速上手](https://fuyuxiang.github.io/echo-agent/getting-started/quickstart/) |
| 核心原理 | [架构概览](https://fuyuxiang.github.io/echo-agent/concepts/architecture/) · [Agent Loop](https://fuyuxiang.github.io/echo-agent/concepts/agent-loop/) · [记忆系统](https://fuyuxiang.github.io/echo-agent/concepts/memory-system/) · [进化与评估](https://fuyuxiang.github.io/echo-agent/concepts/evolution-evaluation/) |
| 配置与运维 | [配置参考](https://fuyuxiang.github.io/echo-agent/reference/configuration/) · [运行方式](https://fuyuxiang.github.io/echo-agent/operations/runtime-modes/) · [安全加固](https://fuyuxiang.github.io/echo-agent/operations/security-hardening/) |
| 版本与兼容性 | [更新记录](CHANGELOG.md) · [兼容性说明](https://fuyuxiang.github.io/echo-agent/reference/compatibility/) |
| 桌面端 | [EchoAgent Desktop](client/README.md) |

## 开发与贡献

仓库中的 `echo_agent/` 提供终端与 Gateway 的核心代码，`web/` 是 Dashboard 前端，`client/` 是桌面端源码，`skills/` 提供内置 Skills。完整目录说明见 [代码地图](https://fuyuxiang.github.io/echo-agent/development/repository-map/)。

在 macOS 或 Linux 上准备开发环境并运行检查：

```bash
git clone https://github.com/fuyuxiang/echo-agent.git
cd echo-agent
uv venv venv --python 3.11
source venv/bin/activate
uv pip install -e ".[all,dev]"
ruff check .
pytest
```

欢迎贡献 Bug 修复、Runtime 改进、工具、Skills、Plugins、消息通道和文档。提交约定见 [CONTRIBUTING.md](CONTRIBUTING.md)。

## 社区与支持

- 使用交流与设计讨论：[GitHub Discussions](https://github.com/fuyuxiang/echo-agent/discussions)
- 问题反馈与功能建议：[GitHub Issues](https://github.com/fuyuxiang/echo-agent/issues)
- 中文交流：[QQ 群 47572014](https://qm.qq.com/q/JWOPDBNssw)
- 安全漏洞：[GitHub 私密安全报告](https://github.com/fuyuxiang/echo-agent/security/advisories/new)，披露流程见 [SECURITY.md](SECURITY.md)

## 许可证

Echo Agent 基于 [MIT License](LICENSE) 开源。桌面端所含第三方组件的许可信息见 [第三方许可说明](client/THIRD_PARTY_NOTICES.md)。
