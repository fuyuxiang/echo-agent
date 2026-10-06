<div align="center">

# Echo Agent

**能长期记忆、持续学习、并真正执行任务的开源 AI Agent**

运行在你自己的电脑或服务器上：既可以 7×24 常驻在微信、Telegram、Slack 等消息入口，也可以通过桌面工作台直接操作文件、浏览器和本地应用。

**Self-hosted · Persistent Memory · Desktop Agent · Multi-channel · Human-in-the-loop**

[![PyPI](https://img.shields.io/pypi/v/echo-agent)](https://pypi.org/project/echo-agent/)
[![Python](https://img.shields.io/pypi/pyversions/echo-agent)](https://pypi.org/project/echo-agent/)
[![CI](https://github.com/fuyuxiang/echo-agent/actions/workflows/ci.yml/badge.svg)](https://github.com/fuyuxiang/echo-agent/actions/workflows/ci.yml)
[![Docs](https://img.shields.io/badge/docs-latest-blue)](https://fuyuxiang.github.io/echo-agent/)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Downloads](https://static.pepy.tech/badge/echo-agent)](https://pepy.tech/project/echo-agent)
[![GitHub stars](https://img.shields.io/github/stars/fuyuxiang/echo-agent?style=social)](https://github.com/fuyuxiang/echo-agent)

[快速开始](#快速开始) · [Echo Agent Desktop](#echo-agent-desktop桌面工作台) · [中文](README.md) · [English](README.en.md) · [完整文档](https://fuyuxiang.github.io/echo-agent/)

</div>

---

## 什么是 Echo Agent

一个仓库，两种形态，同一套内核能力（认知记忆 · 自进化技能 · 工具审批 · 模型路由）：

```text
                     Echo Agent
         ┌──────────────┴──────────────┐
    Agent Runtime                 Echo Agent Desktop
    （Python 包 · pip install）     （Tauri 桌面应用 · 内嵌同一运行时）
    7×24 常驻助理                   在你电脑上干活的工作台
    ├── 14 通道：微信 / Telegram …   ├── 代码工作台
    ├── CLI / Webhook / 定时任务     ├── Browser Use / Computer Use
    └── Dashboard Web 管理面板       ├── 文件 · 知识库 · 定时自动化
                                   └── 微信远程接管
```

### Agent Runtime（常驻助理）

`pip install` 装到你自己的电脑或服务器上，`echo-agent gateway` 起一个常驻网关：微信、Telegram、钉钉、飞书等 14 个通道共享同一份记忆与权限，配合 CLI、Webhook 与定时任务。**你在哪里，它就在哪里；入口可以切换，长期记忆不会从零开始。**

### Echo Agent Desktop（桌面工作台）

Tauri 2 桌面应用，内嵌 Agent 运行时（源码快照在 `client/vendor/`，无需先装 Python 包）。**不只是和 AI 聊天，而是把任务交给它，让它在你电脑上完成**：处理本地文件与项目、受控的浏览器 / 电脑操作、并行任务看板、知识库与定时自动化。人在外面时，用微信远程继续任务、批准权限请求。每个高风险动作执行前都会先请你确认。

<div align="center">
  <img src="client/docs/images/echoagent-home.png" alt="Echo Agent Desktop" width="820" />
</div>

> Dashboard 是网关自带的 Web 管理面板：启动网关后浏览器直接访问（默认 `http://127.0.0.1:58123/`，端口由 `gateway.port` 决定），系统总览、会话、记忆、技能、知识库、任务与日志全在一处，详见 [Dashboard 指南](https://fuyuxiang.github.io/echo-agent/guides/dashboard/)。

---

## 它能做什么

四个真实任务形态（能力均已落地，边界见各节说明）：

- 💻 **「分析这个项目，修掉失败的测试」** — Desktop 代码工作台：读取项目 → 定位 → 修改代码 → 跑测试 → 展示结果，任务与权限在面板上一目了然。
- 📊 **「每个工作日早上，把昨天的数据整理成日报发到飞书」** — Runtime 定时任务：按计划触发 Agent 执行，结果以消息推送到你指定的通道。
- 🌐 **「调研 10 家竞品，整理成表格」** — Desktop Browser Use：受控浏览器逐家访问、提取信息、汇总产出；每个站点操作前经你确认。
- 📱 **「我出门了，刚才那个任务继续跑」** — 微信接管：桌面任务移交微信，路上继续对话、看进度、批准权限请求；桌面关闭时微信指令不会执行。

---

## 核心能力

- **记得住你** — 对话、偏好与任务经验跨会话沉淀，换通道也不丢。长期运行自动衰减低价值记忆，并修正相互矛盾的信息，而不是无限膨胀。
- **越用越强** — 从真实执行轨迹中沉淀可复用技能：候选改进经评测集对照验证后才晋升，带冷却期与一键回滚，技能随使用进化而非出厂定型。
- **真正执行** — Desktop 端直接操作文件、浏览器与本地应用完成任务；浏览器 / 电脑的每次点击、输入、上传执行前单独确认，默认聚焦拒绝。
- **数据归自己** — 完全自托管：数据留在你自己的机器上，凭证加密存储，不依赖任何云端服务。
- **安全可控** — 高风险工具调用统一审批，出站请求经共享 SSRF 防护，执行日志可审计。

混合检索、模型路由、MCP / A2A（入站任务端点；当前 Agent 运行时不提供 A2A 出站委派入口）、插件体系与输出保全等其余能力，见[文档](#文档)；记忆与技能的实现机制见[记忆系统](https://fuyuxiang.github.io/echo-agent/concepts/memory-system/)与[技能进化](https://fuyuxiang.github.io/echo-agent/concepts/evolution-evaluation/)。

---

## 快速开始

### Agent Runtime

环境要求：Python 3.11+，至少一个模型 API Key。

```bash
# 安装
pip install "echo-agent[all]"

# 交互式配置向导（引导录入模型 API Key，数据默认存放在 ~/.echo-agent）
echo-agent setup

# 启动交互式对话
echo-agent run
```

中国大陆网络环境可指定 PyPI 镜像：`pip install "echo-agent[all]" -i https://mirrors.aliyun.com/pypi/simple/`。Windows 下相同的三条命令在 PowerShell 中执行即可。

<details>
<summary>源码安装脚本（Linux / macOS / WSL2）</summary>

`scripts/install.sh` 从 Git 仓库克隆源码、创建独立虚拟环境并安装依赖，可一步完成常驻部署；重复执行即为升级。脚本先下载到本地再执行，便于执行前审阅：

```bash
curl -fsSL -o install.sh https://raw.githubusercontent.com/fuyuxiang/echo-agent/master/scripts/install.sh
# Gitee 镜像（中国大陆网络更快）
curl -fsSL -o install.sh https://gitee.com/fuyuxiang/echo-agent/raw/master/scripts/install.sh

less install.sh && bash install.sh          # 完整选项见 bash install.sh --help
```

</details>

安装方式取舍、依赖分组与卸载见[安装文档](https://fuyuxiang.github.io/echo-agent/getting-started/installation/)；全部子命令见 [CLI 参考](https://fuyuxiang.github.io/echo-agent/reference/cli/)，配置项见 [配置参考](https://fuyuxiang.github.io/echo-agent/reference/configuration/)。

### 常驻运行（后台服务）

`echo-agent run` 是前台进程；想让 Agent 7×24 常驻，把网关注册为系统服务（macOS 用户级 LaunchAgent / Linux 用户级 systemd，无需 root）：

```bash
echo-agent gateway install    # 注册并启动
echo-agent gateway logs -f    # 跟踪日志
echo-agent gateway status     # 查看运行状态
```

网关仅监听本机回环地址（127.0.0.1），浏览器跨站请求默认拒绝，远程接入请走 ssh。Linux 退出登录后保持运行、无 systemd 环境等细节见[后台常驻服务](https://fuyuxiang.github.io/echo-agent/operations/background-service/)与[网关认证](https://fuyuxiang.github.io/echo-agent/integrations/gateway/authentication/)。

### 构建 Echo Agent Desktop

安装包分发渠道建设中，当前从源码构建（需要 Node.js、pnpm、Rust 1.92+ 与 `protoc`，平台前置详见[客户端文档](client/docs/release-workflow.md)）：

```bash
cd client
pnpm install
pnpm tauri dev
```

`pnpm test` 运行前端测试，`pnpm build` 执行 TypeScript 检查与生产构建，`pnpm dist` 生成平台安装包。发布流程与自动化平台安全边界见 [release-workflow.md](client/docs/release-workflow.md) 与 [automation-platform-support.md](client/docs/automation-platform-support.md)。

---

## 14 个通道

一个 Agent 实例可同时接入多个通道，共享同一份记忆与权限：

| | 通道 |
|------|------|
| 海外 IM | Telegram · Discord · Slack · Matrix · WhatsApp · Email |
| 国内 IM | 微信 · 企业微信 · 钉钉 · 飞书 / Lark · QQ 机器人 |
| 其他入口 | CLI · Webhook · 定时任务 |

Telegram、Discord、Slack 支持消息编辑与表情回应；微信与 QQ 机器人（视配置）可收发文件。各通道的完整能力矩阵与接入方式见[通道文档](https://fuyuxiang.github.io/echo-agent/integrations/channels/)。

---

## 架构

<div align="center">
  <img src="docs/assets/architecture.png" alt="Echo Agent 架构图" width="820" />
</div>

各组件的职责边界与数据流见[架构总览](https://fuyuxiang.github.io/echo-agent/concepts/architecture/)，仓库目录结构见[代码地图](https://fuyuxiang.github.io/echo-agent/development/repository-map/)。

---

## 文档

完整文档在 **[fuyuxiang.github.io/echo-agent](https://fuyuxiang.github.io/echo-agent/)**，本 README 覆盖安装与上手部分。

| 主题 | 入口 |
|------|------|
| 开始使用 | [安装](https://fuyuxiang.github.io/echo-agent/getting-started/installation/) · [升级与卸载](https://fuyuxiang.github.io/echo-agent/getting-started/upgrade-uninstall/) |
| 核心概念 | [架构](https://fuyuxiang.github.io/echo-agent/concepts/architecture/) · [记忆系统](https://fuyuxiang.github.io/echo-agent/concepts/memory-system/) · [技能进化](https://fuyuxiang.github.io/echo-agent/concepts/evolution-evaluation/) · [安全模型](https://fuyuxiang.github.io/echo-agent/concepts/security-model/) |
| 使用指南 | [模型接入](https://fuyuxiang.github.io/echo-agent/guides/models/) · [工具与权限](https://fuyuxiang.github.io/echo-agent/guides/tools-permissions/) · [知识库](https://fuyuxiang.github.io/echo-agent/guides/knowledge-base/) · [成本](https://fuyuxiang.github.io/echo-agent/guides/cost-control/) |
| 集成 | [通道](https://fuyuxiang.github.io/echo-agent/integrations/channels/) · [网关](https://fuyuxiang.github.io/echo-agent/integrations/gateway/) · [MCP](https://fuyuxiang.github.io/echo-agent/integrations/mcp/) · [A2A](https://fuyuxiang.github.io/echo-agent/integrations/a2a/) · [插件](https://fuyuxiang.github.io/echo-agent/integrations/plugins/using-plugins/) |
| 运维与参考 | [部署](https://fuyuxiang.github.io/echo-agent/operations/) · [CLI](https://fuyuxiang.github.io/echo-agent/reference/cli/) · [配置项](https://fuyuxiang.github.io/echo-agent/reference/configuration/) |

---

## 开发与贡献

从源码搭建开发环境：

```bash
git clone https://github.com/fuyuxiang/echo-agent.git   # 或 https://gitee.com/fuyuxiang/echo-agent.git
cd echo-agent
uv venv venv --python 3.11 && source venv/bin/activate
uv pip install -e ".[all,dev]"
```

提交前在本地运行与 CI 相同的检查：

```bash
ruff check .
pytest
```

### 提交 PR

- 从 `master` 切出特性分支，一个 PR 只处理一个主题。
- 涉及面向用户的改动时，同步更新 `README.md` 与 `README.en.md`；改动文档时同步中英文两份。
- 修改配置项后运行 `echo-agent config gen-docs` 重新生成配置参考。
- PR 模板中的检查项请逐条确认；CI 会运行 lint、测试、安全扫描、Dashboard 构建、文档构建与打包六项检查。

完整约定见 [CONTRIBUTING](CONTRIBUTING.md)，开发环境与调试方式见[开发文档](https://fuyuxiang.github.io/echo-agent/development/setup/)。

### 参与方向

| 方向 | 入口 |
|------|------|
| 通道适配器 | [新增通道](https://fuyuxiang.github.io/echo-agent/development/add-channel/) |
| 内置工具 | [新增工具](https://fuyuxiang.github.io/echo-agent/development/add-tool/) |
| 模型 Provider | [新增 Provider](https://fuyuxiang.github.io/echo-agent/development/add-provider/) |
| 技能与插件 | [技能编写](https://fuyuxiang.github.io/echo-agent/development/skill-authoring/) · [插件 API](https://fuyuxiang.github.io/echo-agent/development/plugin-api/) |
| 评测数据集 | [测试与评测](https://fuyuxiang.github.io/echo-agent/development/testing-evaluation/) |
| 文档 | [文档贡献](https://fuyuxiang.github.io/echo-agent/development/documentation/) |

### 交流

| 渠道 | 用途 |
|------|------|
| [GitHub Issues](https://github.com/fuyuxiang/echo-agent/issues) | 缺陷报告与功能提案，含 Bug / Feature 两类模板 |
| [GitHub Discussions](https://github.com/fuyuxiang/echo-agent/discussions) | 使用问题、设计讨论与经验分享 |
| QQ 群 [47572014](https://qm.qq.com/q/JWOPDBNssw) | 即时交流 |

行为准则见 [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md)。

---

## 版本与兼容性

当前 `0.3.x`，处于 Beta。版本号采用语义化版本格式；具体兼容性以各版本更新说明为准。

- **PATCH**（`0.3.x`）通常用于修复；升级前仍应阅读更新说明并备份。
- **MINOR**（`0.x.0`）在 Beta 阶段可能包含配置或数据结构的破坏性变更；逐版本阅读更新说明。
- 配置项与插件 / 技能接口的调整会在 [CHANGELOG](CHANGELOG.md) 中逐项标注。

升级前建议备份工作区目录。详细流程见[升级与迁移](https://fuyuxiang.github.io/echo-agent/operations/upgrade-migrations/)，各接口的稳定级别见[兼容性说明](https://fuyuxiang.github.io/echo-agent/reference/compatibility/)。

## 安全

漏洞请通过 GitHub [私密安全报告](https://github.com/fuyuxiang/echo-agent/security/advisories/new)提交，我们在 48 小时内确认接收。披露流程与支持版本见 [SECURITY.md](SECURITY.md)。

部署侧的安全边界与加固清单见[安全模型](https://fuyuxiang.github.io/echo-agent/concepts/security-model/)与[安全加固](https://fuyuxiang.github.io/echo-agent/operations/security-hardening/)。

---

## 协议

[MIT License](LICENSE)
