<div align="center">

# Echo Agent

**记得住过去，学得会未来的开源 AI Agent**

<a href="https://github.com/fuyuxiang/echo-agent">
  <img src="docs/assets/echo-agent.png" alt="Echo Agent" width="720" />
</a>

<br/>

[![PyPI](https://img.shields.io/pypi/v/echo-agent)](https://pypi.org/project/echo-agent/)
[![Downloads](https://static.pepy.tech/badge/echo-agent)](https://pepy.tech/project/echo-agent)
[![GitHub stars](https://img.shields.io/github/stars/fuyuxiang/echo-agent?style=social)](https://github.com/fuyuxiang/echo-agent)

[中文](README.md) · [English](README.en.md)

</div>

---

## 什么是 Echo Agent

Echo Agent 是一个可自托管的长期运行 AI Agent。与一次性问答不同，它能：

- **跨会话记忆** — 四层认知记忆结构，自动衰减与矛盾检测，解决长期运行下的记忆膨胀问题，对话不再从零开始。
- **自进化技能** — 从真实执行轨迹中生成候选改进，经评测验证后才生效，支持回滚。
- **多入口归一** — CLI、Gateway、Webhook、Cron 及 Telegram / Discord / Slack / 微信 / 飞书 / 钉钉等 12 个通道共享同一份状态。
- **安全可控** — 高风险工具调用经统一审批，凭证加密存储，执行日志可审计。

一句话：**让 Agent 带着记忆和不断进化的技能，长期为你工作。**

---

## 快速开始

环境要求：Python 3.11+，至少一个模型 API Key。

```bash
# 安装
pip install "echo-agent[all]"

# 交互式配置向导（数据默认存放在 ~/.echo-agent）
echo-agent setup

# 启动
echo-agent run
```

> 查看配置项：`echo-agent config explain <配置项>` 查看单项说明（含类型、默认值与可选值）、`echo-agent config dump` 查看当前生效配置（密钥自动脱敏）、`echo-agent config validate` 校验配置文件。

<details>
<summary>国内镜像 / Windows / 一键脚本</summary>

```bash
# 阿里云镜像加速
pip install "echo-agent[all]" -i https://mirrors.aliyun.com/pypi/simple/
```

```powershell
# Windows（PowerShell）
pip install "echo-agent[all]"
$env:OPENAI_API_KEY = "sk-..."
echo-agent setup
echo-agent run
```

```bash
# 一键安装脚本（仅支持 Linux / macOS / WSL2，会从源码安装到 ~/.echo-agent
# 并可注册 systemd 服务；建议先审查脚本内容再执行）
curl -fsSL -o install.sh https://raw.githubusercontent.com/fuyuxiang/echo-agent/master/scripts/install.sh
less install.sh && bash install.sh
```

</details>

> 常驻网关：用 `echo-agent gateway` 在前台启动常驻网关（或由 systemd/launchd 托管，`echo-agent service install` 可注册 systemd 服务）。网关运行后，可在本机用 `echo-agent cli` 作为瘦客户端接入，开一条独立会话与同一个常驻 agent 对话。仅限本机 loopback（127.0.0.1），不支持远程地址；远程接入请走 ssh。

> 本机安全边界：零配置（`allowlist` 模式 + 空白名单）的 loopback 网关只服务两类客户端——`echo-agent cli`（自带 `cli:` 身份），以及不带浏览器 `Origin` 的原生客户端（脚本/SDK）。带跨站 `Origin` 的浏览器请求（含自带 playground 页面）会被拒，以阻断恶意网页借浏览器驱动本机 agent（CSRF）。若要让浏览器/playground 访问，请在配置中设 `gateway.auth.mode=open`、把用户加进 `gateway.auth.allowed_users`，或（webview 桌面端等场景）把其 Origin 加进 `gateway.auth.allowed_origins`。

---

## 架构总览

<div align="center">
  <img src="docs/assets/architecture.png" alt="Echo Agent 架构图" width="820" />
</div>

---

## 核心能力

| 模块 | 说明 |
|------|------|
| **Agent Loop** | 接收事件 → 构建上下文 → 调用模型 → 执行工具，跨入口共享同一条执行路径 |
| **认知记忆** | Working / Episodic / Semantic / Archival 四层，配合衰减、矛盾检测与重要性重排 |
| **混合检索** | BM25 + FAISS 向量融合召回，按查询特征自适应权重，FAISS 缺失时自动降级 |
| **自进化引擎** | 轨迹记录 → 候选生成 → 评测对照 → 晋升/驳回，支持冷却期与一键回滚 |
| **模型路由** | 主推理、上下文压缩、向量嵌入、风险审批可独立配置 provider 与模型 |
| **工具审批** | 三档策略 `manual` / `smart` / `off`，无人值守通道默认拒绝高风险调用 |
| **跨进程互操作** | A2A JSON-RPC + MCP 客户端（含 OAuth），支持动态工具注册 |
| **本地优先** | 会话、记忆、轨迹、凭证默认存放工作区，凭证加密落盘 |

---

## 适用场景

- Agent 跑在本机或自有服务器，需要完整审计与可追溯
- 对话、偏好与任务经验需要跨会话长期沉淀
- 希望 Agent 技能从真实使用中持续改进，而非出厂定型
- 多入口（CLI、Webhook、消息机器人）需共享同一份记忆与权限
- 高风险工具需要强制审批，避免误操作
- 需同时接入多家模型 provider，按任务类型分配

---

## 开发与贡献

从源码安装（开发模式）：

```bash
git clone https://github.com/fuyuxiang/echo-agent.git   # 国内可用 https://gitee.com/fuyuxiang/echo-agent.git
cd echo-agent
uv venv venv --python 3.11 && source venv/bin/activate
uv pip install -e ".[all,dev]"

# 提交前检查
ruff check .
pytest
```

PR 前请确保 lint 和测试通过（CI 会在 PR 上自动运行同样的检查），并同步更新中英文 README。

**参与方向：** 通道适配器 · 内置工具 · MCP 集成 · 技能示例 · 评测数据集 · 文档完善 · 部署模板

**社区：**
- QQ群：[47572014](https://qm.qq.com/q/JWOPDBNssw)

---

## 协议

[MIT License](LICENSE)

---


## Star History

[![Star History Chart](https://api.star-history.com/svg?repos=fuyuxiang/echo-agent&type=Date)](https://star-history.com/#fuyuxiang/echo-agent&Date)
