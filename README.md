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

环境要求：Python 3.11+，至少一个模型 API Key。.

```bash
# 安装
pip install "echo-agent[all]"

# 交互式配置向导（引导录入模型 API Key，数据默认存放在 ~/.echo-agent）
echo-agent setup

# 启动交互式对话
echo-agent run
```

<details>
<summary>国内镜像 / Windows / 一键脚本</summary>

```bash
# 阿里云镜像加速
pip install "echo-agent[all]" -i https://mirrors.aliyun.com/pypi/simple/
```

```powershell
# Windows（PowerShell）
pip install "echo-agent[all]"
echo-agent setup
echo-agent run
```

```bash
# 一键安装脚本（仅支持 Linux / macOS / WSL2，会从源码安装到 ~/.echo-agent
# 并可注册后台常驻服务；建议先审查脚本内容再执行）
# 国内从 Gitee 拉脚本：
curl -fsSL -o install.sh https://gitee.com/fuyuxiang/echo-agent/raw/master/scripts/install.sh
# 国外从 GitHub 拉脚本：
curl -fsSL -o install.sh https://raw.githubusercontent.com/fuyuxiang/echo-agent/master/scripts/install.sh

less install.sh && bash install.sh

# 脚本会实测 Gitee 与 GitHub 的响应速度后自动选择克隆源；也可以手动指定：
bash install.sh --repo gitee     # 强制走 Gitee
bash install.sh --repo github    # 强制走 GitHub

# --repo 只影响 git clone/fetch；嵌入与精排模型包始终按 Gitee 优先、GitHub 兜底
# 的固定顺序下载（分卷托管所致，与 --repo 无关）。
# --no-mirror-probe 会同时关掉 PyPI 源、代码托管、Node.js 镜像三处测速，
# 各自退回到第一个默认源。
# 完整选项与环境变量（含 ECHO_SKIP_RERANK_PREFETCH 等模型预取开关）：
bash install.sh --help
```

</details>

### 常用命令

```bash
echo-agent run              # 交互式对话（终端行输入）
echo-agent setup            # 配置向导（模型、通道、权限等，可反复运行）
echo-agent status           # 查看当前配置状态
echo-agent gateway          # 前台启动常驻网关
echo-agent gateway install  # 把网关注册为后台服务（推荐的常驻方式，见下）
echo-agent cli              # 以瘦客户端接入本机常驻网关（终端 TUI）
echo-agent cost             # 查看成本归因报告
```

> 查看配置项：`echo-agent config explain <配置项>` 查看单项说明（含类型、默认值与可选值）、`echo-agent config dump` 查看当前生效配置（密钥自动脱敏）、`echo-agent config validate` 校验配置文件。

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

<details>
<summary>Linux 注意事项 / 无 systemd 环境 / 旧命令</summary>

- **退出登录后继续运行**（Linux 用户级服务默认随登录会话结束）：`sudo loginctl enable-linger $USER`
- **服务器多用户场景**：`echo-agent gateway install --system` 注册系统级 systemd 服务（需 sudo）
- **无 systemd 的环境**（WSL 未开 systemd、容器等）：用 tmux 或 nohup 保持前台进程，如 `tmux new -s echo-agent 'echo-agent gateway'`
- **升级后服务文件过期**：`echo-agent gateway status` 会提示 stale，执行 `echo-agent gateway install --force` 重写
- 旧的 `echo-agent service` 命令仍可用，已标记废弃，请改用 `echo-agent gateway <action>`

</details>

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
