<div align="center">

# Echo Agent

**记得住过去，学得会未来的开源 AI Agent**

<a href="https://github.com/fuyuxiang/echo-agent">
  <img src="docs/assets/echo-agent.png" alt="Echo Agent" width="720" />
</a>

<br/>

[![PyPI](https://img.shields.io/pypi/v/echo-agent)](https://pypi.org/project/echo-agent/)
[![Python](https://img.shields.io/pypi/pyversions/echo-agent)](https://pypi.org/project/echo-agent/)
[![CI](https://github.com/fuyuxiang/echo-agent/actions/workflows/ci.yml/badge.svg)](https://github.com/fuyuxiang/echo-agent/actions/workflows/ci.yml)
[![Docs](https://img.shields.io/badge/docs-latest-blue)](https://fuyuxiang.github.io/echo-agent/)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Downloads](https://static.pepy.tech/badge/echo-agent)](https://pepy.tech/project/echo-agent)
[![GitHub stars](https://img.shields.io/github/stars/fuyuxiang/echo-agent?style=social)](https://github.com/fuyuxiang/echo-agent)

[中文](README.md) · [English](README.en.md) · [完整文档](https://fuyuxiang.github.io/echo-agent/)

</div>

---

## 什么是 Echo Agent

Echo Agent 是一个可自托管的长期运行 AI Agent。与一次性问答不同，它能：

- **跨会话记忆** — 四层认知记忆结构，自动衰减与矛盾检测，解决长期运行下的记忆膨胀问题，对话不再从零开始。
- **自进化技能** — 从真实执行轨迹中生成候选改进，经评测验证后才生效，支持回滚。
- **多入口归一** — CLI、Gateway、Webhook、Cron 及 Telegram / Discord / Slack / 微信 / 企业微信 / 飞书 / 钉钉 / QQ / WhatsApp / 邮件 / Matrix 共 [14 个通道](https://fuyuxiang.github.io/echo-agent/integrations/channels/)共享同一份状态。
- **安全可控** — 高风险工具调用经统一审批，凭证加密存储，执行日志可审计。

---

## 快速开始

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

`scripts/install.sh` 与 `pip install` 是两条独立路径：它从 Git 仓库克隆源码到 `~/.echo-agent`、创建独立虚拟环境、安装 `[all]` 依赖，并可将网关注册为常驻服务。适用于需要修改源码或希望一步完成常驻部署的场景；只想使用发布版本时用 `pip install` 即可。

脚本先下载到本地再执行，便于执行前审阅内容：

```bash
# GitHub
curl -fsSL -o install.sh https://raw.githubusercontent.com/fuyuxiang/echo-agent/master/scripts/install.sh
# Gitee 镜像（中国大陆网络更快）
curl -fsSL -o install.sh https://gitee.com/fuyuxiang/echo-agent/raw/master/scripts/install.sh

less install.sh && bash install.sh
```

```bash
# 脚本实测两个代码托管的响应速度后自动选择克隆源，也可显式指定：
bash install.sh --repo gitee
bash install.sh --repo github

bash install.sh --reconfigure    # 重新执行配置向导
bash install.sh --skip-setup     # 仅安装代码，不进入配置向导

# 完整选项与环境变量：
bash install.sh --help
```

`--repo` 的作用范围限于 `git clone` / `fetch`。嵌入与精排模型包因分卷托管，始终按 Gitee 优先、GitHub 兜底的顺序下载，不受该参数影响。`--no-mirror-probe` 关闭 PyPI 源、代码托管与 Node.js 镜像三处测速，各自使用第一个默认源。

重复执行脚本即为升级：检测到已有可用配置时跳过配置向导并保留现有配置。

</details>

安装方式的取舍、`[all]` 之外的依赖分组与卸载步骤见[安装文档](https://fuyuxiang.github.io/echo-agent/getting-started/installation/)。

### 常用命令

```bash
echo-agent run              # 交互式对话（终端行输入）
echo-agent setup            # 配置向导（模型、通道、权限等，可反复运行）
echo-agent status           # 查看当前配置状态
echo-agent gateway          # 前台启动常驻网关
echo-agent gateway install  # 把网关注册为后台服务（推荐的常驻方式，见下）
echo-agent cli              # 接入本机常驻网关（默认原生 scrollback）
echo-agent cli --tui        # 可选的全屏 Textual 界面
echo-agent cost             # 查看成本归因报告
echo-agent dashboard build  # 构建 Web Dashboard 前端产物（源码安装时按需执行）
```

> 查看配置项：`echo-agent config explain <配置项>` 查看单项说明（含类型、默认值与可选值）、`echo-agent config dump` 查看当前生效配置（密钥自动脱敏）、`echo-agent config validate` 校验配置文件。

完整子命令与参数见 [CLI 参考](https://fuyuxiang.github.io/echo-agent/reference/cli/)，全部配置项见 [配置参考](https://fuyuxiang.github.io/echo-agent/reference/configuration/)。

### 常驻运行（后台服务）

`echo-agent run` 和 `echo-agent gateway` 都是前台进程，关掉终端就退出。想让 agent 7×24 常驻，把网关注册为系统服务即可（macOS 注册用户级 LaunchAgent，Linux 注册用户级 systemd 服务，均无需 root，开机自启、崩溃自动拉起）：

```bash
echo-agent gateway install    # 注册后台服务
echo-agent gateway start      # 启动
echo-agent gateway status     # 查看运行状态
echo-agent gateway logs -f    # 跟踪日志
echo-agent gateway restart    # 重启（升级 echo-agent 后执行一次）
echo-agent gateway stop       # 停止
echo-agent gateway uninstall  # 取消注册
```

网关运行后，在本机任意终端用 `echo-agent cli` 接入，即可与同一个常驻 agent 对话（会话独立、记忆共享）。网关仅监听本机 loopback（127.0.0.1），不支持远程地址；远程接入请走 ssh。

两点环境差异需要注意：Linux 的用户级服务随登录会话结束而停止，执行 `sudo loginctl enable-linger $USER` 可使其在退出登录后继续运行；未启用 systemd 的环境（WSL2 默认配置、容器）改用 tmux 维持前台进程，如 `tmux new -s echo-agent 'echo-agent gateway'`。系统级注册与服务文件更新见[后台常驻服务](https://fuyuxiang.github.io/echo-agent/operations/background-service/)。

> **本机访问边界**：零配置下的 loopback 网关只接受两类客户端——`echo-agent cli`，以及不携带浏览器 `Origin` 的原生客户端（脚本、SDK）。携带跨站 `Origin` 的浏览器请求一律拒绝，防止网页借用户浏览器驱动本机 agent（CSRF）。开放浏览器或 playground 访问的配置方式见[网关认证](https://fuyuxiang.github.io/echo-agent/integrations/gateway/authentication/)。

---

## 文档

完整文档在 **[fuyuxiang.github.io/echo-agent](https://fuyuxiang.github.io/echo-agent/)**，本 README 覆盖安装与上手部分。

| | |
|---|---|
| [开始使用](https://fuyuxiang.github.io/echo-agent/getting-started/) | 安装、快速上手、升级与卸载 |
| [使用指南](https://fuyuxiang.github.io/echo-agent/guides/) | 模型接入、工具与权限、记忆、知识库、任务、Dashboard、成本 |
| [核心概念](https://fuyuxiang.github.io/echo-agent/concepts/) | 架构、Agent 循环、记忆系统、事件投递、安全模型、技能进化 |
| [集成](https://fuyuxiang.github.io/echo-agent/integrations/) | 通道、网关、MCP、A2A、插件与技能 |
| [运维](https://fuyuxiang.github.io/echo-agent/operations/) | 部署、后台常驻、可观测性、备份恢复、安全加固、故障排查 |
| [参考手册](https://fuyuxiang.github.io/echo-agent/reference/) | CLI、配置项、环境变量、网关 API、工具清单、术语表 |

---

## 架构

<div align="center">
  <img src="docs/assets/architecture.png" alt="Echo Agent 架构图" width="820" />
</div>

各组件的职责边界与数据流见[架构总览](https://fuyuxiang.github.io/echo-agent/concepts/architecture/)，仓库目录结构见[代码地图](https://fuyuxiang.github.io/echo-agent/development/repository-map/)。

---

## 适用场景

- Agent 运行在本机或自有服务器，需要完整审计与可追溯
- 对话、偏好与任务经验需要跨会话长期沉淀
- 希望 Agent 的技能从真实使用中持续改进，而非出厂定型
- 多入口（CLI、Webhook、消息机器人）需共享同一份记忆与权限
- 高风险工具需要强制审批，避免误操作
- 需同时接入多家模型 provider，按任务类型分配

生产部署的形态选择、资源规划与加固清单见[运维文档](https://fuyuxiang.github.io/echo-agent/operations/)。

---

## 能力总览

| 模块 | 说明 | 文档 |
|------|------|------|
| **Agent 循环** | 接收事件 → 构建上下文 → 调用模型 → 执行工具，跨入口共享同一条执行路径 | [Agent 循环](https://fuyuxiang.github.io/echo-agent/concepts/agent-loop/) |
| **认知记忆** | Working / Episodic / Semantic / Archival 四层，配合衰减、矛盾检测与重要性重排 | [记忆系统](https://fuyuxiang.github.io/echo-agent/concepts/memory-system/) |
| **混合检索** | BM25 + FAISS 向量融合召回，按查询特征自适应权重，FAISS 缺失时自动降级 | [知识库](https://fuyuxiang.github.io/echo-agent/guides/knowledge-base/) |
| **自进化引擎** | 轨迹记录 → 候选生成 → 评测对照 → 晋升/驳回，支持冷却期与一键回滚 | [技能进化与评测](https://fuyuxiang.github.io/echo-agent/concepts/evolution-evaluation/) |
| **模型路由** | 主推理、上下文压缩、向量嵌入、风险审批可独立配置 provider 与模型 | [路由与 Fallback](https://fuyuxiang.github.io/echo-agent/guides/models/routing-fallback/) |
| **工具审批** | 三档策略 `manual` / `smart` / `off`，无人值守通道默认拒绝高风险调用 | [工具与权限](https://fuyuxiang.github.io/echo-agent/guides/tools-permissions/) |
| **多模型支持** | OpenAI、Anthropic、Gemini、Bedrock、OpenRouter，以及 DeepSeek、Qwen、Kimi、GLM、Ollama 等 OpenAI 兼容端点 | [Provider 总览](https://fuyuxiang.github.io/echo-agent/guides/models/providers/) |
| **跨进程互操作** | A2A JSON-RPC 入站任务端点 + MCP 客户端（含 OAuth 与动态工具注册）；当前 Agent 运行时不提供 A2A 出站委派入口 | [MCP](https://fuyuxiang.github.io/echo-agent/integrations/mcp/) · [A2A](https://fuyuxiang.github.io/echo-agent/integrations/a2a/) |
| **插件体系** | 通过 entry-point 注册外部插件 | [使用插件](https://fuyuxiang.github.io/echo-agent/integrations/plugins/using-plugins/) |
| **Dashboard** | 内置 Web 管理面板，查看对话、费用与运行状态 | [Dashboard](https://fuyuxiang.github.io/echo-agent/guides/dashboard/) |
| **定时任务** | 内置 Cron 调度器，按计划触发 Agent 执行 | [定时任务](https://fuyuxiang.github.io/echo-agent/guides/scheduled-jobs/) |
| **输出保全** | 超长工具输出落盘保全，模型只见首尾预览与取回路径，可用 `read_spill` 按字符区间或正则取回完整内容 | [上下文压缩与输出保全](https://fuyuxiang.github.io/echo-agent/concepts/context-compression-spill/) |
| **本地优先** | 会话、记忆、轨迹、凭证默认存放工作区，凭证加密落盘 | [安全模型](https://fuyuxiang.github.io/echo-agent/concepts/security-model/) |

> 超过 `spill.maxInlineChars`（默认 6000 字符）的工具输出不直接进入上下文，而是替换为头部、
> 尾部与落盘路径，模型通过 `read_spill` 按字符区间或正则取回完整内容。若技能或提示词依赖工具
> 输出完整可见，可设 `spill.enabled: false` 关闭该行为，或提高 `spill.maxInlineChars`。
> 产物的会话隔离、回收策略与边界成立条件见
> [上下文压缩与输出保全](https://fuyuxiang.github.io/echo-agent/concepts/context-compression-spill/)。

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

当前 `0.3.x`，处于 Beta。兼容性按语义化版本处理：

- **PATCH**（`0.3.x`）向后兼容，可直接升级。
- **MINOR**（`0.x.0`）保持配置兼容，涉及数据结构调整时随版本提供迁移：`echo-agent migrate status` 查看待执行项，`echo-agent migrate run` 执行（`--dry-run` 先预演），`echo-agent migrate rollback` 回退。
- 配置项与插件 / 技能接口的调整会在 [CHANGELOG](CHANGELOG.md) 中逐项标注。

升级前建议备份工作区目录。详细流程见[升级与迁移](https://fuyuxiang.github.io/echo-agent/operations/upgrade-migrations/)，各接口的稳定级别见[兼容性说明](https://fuyuxiang.github.io/echo-agent/reference/compatibility/)。

## 安全

漏洞请通过 GitHub [私密安全报告](https://github.com/fuyuxiang/echo-agent/security/advisories/new)提交，我们在 48 小时内确认接收。披露流程与支持版本见 [SECURITY.md](SECURITY.md)。

部署侧的安全边界与加固清单见[安全模型](https://fuyuxiang.github.io/echo-agent/concepts/security-model/)与[安全加固](https://fuyuxiang.github.io/echo-agent/operations/security-hardening/)。

---

## 协议

[MIT License](LICENSE)
