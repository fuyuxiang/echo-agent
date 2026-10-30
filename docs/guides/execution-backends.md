# 执行后端

**适用范围：独立 Python Runtime。** 桌面端操作见 [Desktop 使用指南](../desktop/index.md)。

命令与代码类工具（`exec`、`execute_code`、`process`）不直接在 Agent 进程里跑，而是交给一个**执行器**。执行器决定隔离强度与运行位置。本页字段取自 `echo_agent/config/schema.py` 的 `ExecutionConfig`，行为取自 `echo_agent/agent/executors/factory.py`。

## 四种执行器

`execution.default_executor` 在 `tools.exec.host: auto` 时选择执行器。schema 默认值为 `sandbox`，但包内默认配置将其覆盖为 `local`；执行工具当前另由默认的 `tools.exec.host: sandbox` 选用目录型执行器：

| 取值 | 隔离方式 | 运行位置 | 适用场景 |
|------|----------|----------|----------|
| `local` | 无额外隔离 | 本机，工作区内 | 完全信任的本地开发 |
| `sandbox` | 独立工作目录，不提供 OS 级隔离 | 本机 `sandbox_root` 下 | 需要临时工作副本的受信任命令 |
| `container` | 容器 | 本机容器运行时 | 需要强隔离或固定运行环境 |
| `remote` | SSH | 远程主机 | 算力或环境在别处 |

执行器实例在 `AgentLoop` 生命周期内长期复用，沙箱与容器的准备开销只付一次，不是每次工具调用都重建。

```yaml
execution:
  default_executor: sandbox
  network_policy: deny
```

!!! note "不存在 execution.backend"
    配置字段是 `default_executor`，且四种取值是平铺的枚举 —— 不存在 `execution.backend`，也不存在 `execution.shell`、`execution.container`、`execution.process` 这类按后端分组的嵌套小节。写成那种结构不会报错，但会被当作未知键静默忽略。

## 通用配置

`ExecutionConfig` 的全部字段如下：

| 字段 | 默认值 | 说明 |
|------|--------|------|
| `default_executor` | `local`（生效值；schema 为 `sandbox`） | `tools.exec.host: auto` 时的执行器类型 |
| `network_policy` | `allow`（生效值；schema 为 `deny`） | 出站网络策略：`allow` / `deny` / `restricted` |
| `sandbox_root` | `/tmp/echo-agent-sandbox` | `sandbox` 执行器的根目录 |
| `container_image` | `''` | `container` 执行器使用的镜像 |
| `remote_host` | `''` | `remote` 执行器的目标主机 |
| `remote_user` | `root` | SSH 用户 |
| `remote_key_path` | `''` | SSH 私钥路径 |
| `remote_strict_host_key` | `accept-new` | 主机密钥校验：`no` / `accept-new` / `yes` |
| `remote_connect_timeout` | `10` | SSH 连接超时（秒） |
| `max_background_tasks` | `64` | 后台任务并发上限 |

`network_policy` 会传递给执行器。包内默认配置将其设为 `allow`；显式设为 `deny` 时，`web_fetch`、`web_search` 等带出站网络能力的工具不会注册。命令执行器对网络命令的检查是策略检查，不是操作系统网络隔离，详见[安全档位矩阵](../reference/security-profile-matrix.md)。

## local

直接在工作区内执行，不做额外隔离。仅在完全信任且需要访问本机环境时使用。

```yaml
execution:
  default_executor: local
```

## sandbox

在 `sandbox_root` 下建立独立目录，并将工作区内容复制到其中执行。进程仍在宿主机上运行，可以通过绝对路径访问其权限允许的宿主文件；它不是不受信任代码的安全边界。

```yaml
execution:
  default_executor: sandbox
  sandbox_root: /tmp/echo-agent-sandbox
```

## container

在容器内执行，隔离性最强，同时能固定运行环境。需要本机已安装并运行容器运行时，且 `container_image` 必须显式指定 —— 它默认为空。

```yaml
execution:
  default_executor: container
  container_image: python:3.12-slim
  network_policy: deny
```

资源限额、卷挂载等细节由容器运行时侧决定，配置中没有对应字段。

## remote

通过 SSH 在远程主机上执行。

```yaml
execution:
  default_executor: remote
  remote_host: 10.0.0.20
  remote_user: echo
  remote_key_path: ~/.ssh/echo_agent_ed25519
  remote_strict_host_key: "yes"   # 必须加引号，否则 YAML 会解析为布尔值
  remote_connect_timeout: 10
```

`remote_strict_host_key` 的默认值 `accept-new` 会在首次连接时自动接受主机密钥。生产环境建议改为 `"yes"`，并预先把主机密钥写入 `known_hosts`，以免首次连接被中间人劫持。

!!! warning "yes 与 no 必须加引号"
    该字段是字符串枚举（`no` / `accept-new` / `yes`）。YAML 会把不加引号的 `yes` 和 `no` 解析为布尔值，导致配置校验失败。写作 `"yes"` 或 `"no"`。

## 按工具覆盖执行器

`tools.exec` 有独立的 `host` 字段，默认 `sandbox`。注册时创建的同一执行器由 `exec`、`execute_code` 和 `process` 共用；设为 `auto` 才使用 `execution.default_executor`：

```yaml
execution:
  default_executor: sandbox

tools:
  exec:
    host: container        # 三种执行工具共用容器执行器
```

`tools.exec` 的其余字段用于约束命令本身：

| 字段 | 默认值 | 说明 |
|------|--------|------|
| `enabled` | `true` | 是否启用 `exec` 工具 |
| `security` | `allowlist` | 命令准入策略 |
| `allowed_commands` | `[]` | 显式允许的命令 |
| `blocked_commands` | `[]` | 显式拒绝的命令 |
| `safe_bins` | 见下 | 视为安全的可执行文件清单 |
| `ask` | `on_miss` | 何时请求审批 |
| `max_output_chars` | `2000000` | 输出截断阈值 |
| `host` | `sandbox` | 该工具使用的执行器 |
| `env_allowlist` | `[]` | 显式传入 `exec`、`execute_code`、`process` 的本机环境变量名 |

`safe_bins` 默认包含 `awk`、`cat`、`date`、`echo`、`find`、`grep`、`head`、`ls`、`pwd` 等只读类命令。

`execute_code` 的约束在 `tools.code_exec`，含 `enabled`、`allowed_languages`、`timeout_seconds` 三项。

执行工具默认只继承 `PATH`、`HOME`、语言、TLS 证书路径等基础变量，以及不带账号密码的代理地址。需要项目变量时，在配置中按名称授权；变量值仍从服务进程环境读取，不写进配置文件：

```yaml
tools:
  exec:
    env_allowlist: [DATABASE_URL, PYTHONPATH]
```

名单同时作用于 `exec`、`execute_code` 和后台 `process`。这些工具运行的命令可读取获授权变量，因而只应列出确实需要的名称。执行请求中显式注入的凭据仍按原有授权机制传入。

## 执行器对比

| 维度 | local | sandbox | container | remote |
|------|:-----:|:-------:|:---------:|:------:|
| 隔离强度 | 无 | 仅工作目录分离 | 取决于容器配置 | 取决于远端 |
| 额外依赖 | 无 | 无 | 容器运行时 | SSH 可达 + 密钥 |
| 启动开销 | 最低 | 低 | 中 | 中 |
| 可访问本机工作区 | 是 | 有工作区副本，也可能通过绝对路径访问原目录 | 取决于挂载 | 取决于远端 |

## 安全建议

- 需要拒绝出站工具时，显式设置 `network_policy: deny`；不要把命令模式检查当成网络命名空间隔离。
- 不要为了省事切到 `local`：它没有隔离，模型生成的命令直接作用于工作区。
- `tools.exec.security` 保持 `allowlist`，用 `allowed_commands` 精确列出所需命令，而非放开全部再用 `blocked_commands` 排除 —— 黑名单容易被绕过。
- 使用 `remote` 时把 `remote_strict_host_key` 设为 `yes`。
- `exec`、`execute_code`、`process` 都属 `HIGH_RISK_TOOLS`，在 `daemon` 与 `public_gateway` 运行形态下默认被拒绝。要在这些形态下使用，需显式加入 `tools.also_allow`，并配合 `permissions.approval` 保留人工确认。

## 相关页面

- [安全档位矩阵](../reference/security-profile-matrix.md) — 工具准入与审批判定
- [内置工具参考](../reference/tools.md) — `exec` / `execute_code` / `process` 的参数
- [配置参考](../reference/configuration.md) — 由 schema 自动生成的逐项说明
