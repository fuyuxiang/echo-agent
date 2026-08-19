# 安全加固

生产环境部署前，请逐项完成以下安全清单。

---

## 安全清单总览

| 类别 | 项目 | 优先级 |
|------|------|--------|
| 认证 | 启用 Token 认证 | P0 |
| 认证 | 配置 admin_tokens | P0 |
| 网络 | 确认绑定 localhost | P0 |
| 网络 | 配置 allowed_origins | P1 |
| 网络 | 配置 allowed_hosts | P1 |
| 工具 | 设置 security.profile | P0 |
| 工具 | 设置 tools.profile | P1 |
| 文件 | 限制数据目录权限 | P1 |
| 文件 | 保护配置文件 | P1 |
| 运行时 | 限制工具风险等级 | P1 |

---

## 认证模式

Gateway 提供三种认证模式，生产环境应选择 `allowlist` 或 `pairing`：

### open 模式（仅限开发）

```yaml
gateway:
  auth:
    mode: open
```

!!! danger "禁止在生产环境使用 open 模式"
    `open` 模式不验证任何请求身份，任何能访问 Gateway 端口的客户端均可执行操作。

### allowlist 模式（推荐）

预定义 Token 白名单，客户端请求必须携带有效 Token：

```yaml
gateway:
  auth:
    mode: allowlist
    api_tokens:
      - "ea-user-a1b2c3d4e5f6"
      - "ea-user-x7y8z9w0v1u2"
    admin_tokens:
      - "ea-admin-secret-token"
    token_header: "X-Echo-Token"
```

- `api_tokens`：普通用户 Token，可执行对话和任务操作
- `admin_tokens`：管理员 Token，可访问配置修改、服务管理等管理接口
- `token_header`：自定义请求头名称，默认 `X-Echo-Token`

### pairing 模式

适合动态设备接入场景。新客户端需通过配对码完成首次认证：

```yaml
gateway:
  auth:
    mode: pairing
    pairing_ttl_seconds: 300   # 配对码有效期
    allowed_users:
      - "alice"
      - "bob"
    admin_users:
      - "alice"
```

!!! tip "Token 生成建议"
    使用足够长度的随机字符串作为 Token（建议 32 字符以上），避免使用可预测的值。可用 `python -c "import secrets; print(secrets.token_urlsafe(32))"` 生成。

---

## 网络绑定与保护

### 绑定地址

```yaml
gateway:
  host: 127.0.0.1    # 仅本地访问（默认）
  port: 8420
```

如必须接受远程连接（如容器环境），使用防火墙限制来源：

```bash
# iptables 示例：仅允许内网访问
iptables -A INPUT -p tcp --dport 8420 -s 10.0.0.0/8 -j ACCEPT
iptables -A INPUT -p tcp --dport 8420 -j DROP
```

### Origin 保护

防止跨站请求伪造（CSRF）：

```yaml
gateway:
  auth:
    allowed_origins:
      - "http://localhost:3000"
      - "https://echo-agent.internal.com"
    allowed_hosts:
      - "localhost"
      - "echo-agent.internal.com"
```

Gateway 会拒绝 `Origin` 或 `Host` 头不在白名单中的请求。

!!! warning "空 Origin 请求"
    某些非浏览器客户端（如 curl）不发送 Origin 头。Gateway 对无 Origin 的请求采用 Host 头验证。

---

## 安全配置文件

Echo Agent 提供分级安全配置，通过 profile 快速应用预定义策略：

### security.profile（3 级）

| 级别 | 名称 | 说明 |
|------|------|------|
| 1 | `relaxed` | 最少限制，适合本地开发 |
| 2 | `standard` | 平衡安全与功能，默认值 |
| 3 | `strict` | 最大限制，生产推荐 |

```yaml
security:
  profile: strict
```

### tools.profile（4 级）

控制 Agent 可调用的工具范围：

| 级别 | 名称 | 可用工具 |
|------|------|---------|
| 1 | `minimal` | MINIMAL_TOOLS — 仅基础对话 |
| 2 | `messaging` | + MESSAGING_TOOLS — 消息通道 |
| 3 | `coding` | + CODING_TOOLS — 代码读写执行 |
| 4 | `full` | + HIGH_RISK_TOOLS — 系统管理 |

```yaml
tools:
  profile: coding    # 生产环境建议不超过 coding
```

!!! danger "HIGH_RISK_TOOLS 警告"
    `full` 级别包含文件删除、系统命令等高风险工具。除非明确需要，否则不要在生产环境启用。

---

## 工具风险等级

Echo Agent 的内置工具按风险分为四组：

| 工具组 | 示例工具 | 风险说明 |
|--------|---------|---------|
| `MINIMAL_TOOLS` | 搜索、问答、摘要 | 只读操作，无副作用 |
| `MESSAGING_TOOLS` | 发消息、邮件通知 | 可对外发送内容 |
| `CODING_TOOLS` | 文件读写、代码执行 | 可修改本地文件 |
| `HIGH_RISK_TOOLS` | shell 执行、网络请求 | 可执行任意命令 |

可在配置中精细控制单个工具的启用/禁用：

```yaml
tools:
  profile: coding
  disabled:
    - "shell_execute"        # 禁用特定高风险工具
    - "file_delete"
```

---

## 文件权限

### 数据目录

```bash
# 限制数据目录仅当前用户可访问
chmod 700 ~/.echo-agent
chmod 600 ~/.echo-agent/config.yaml
chmod 700 ~/.echo-agent/data
```

### 配置文件中的敏感信息

配置文件可能包含 API Key 等敏感信息，确保权限正确：

```bash
# 检查权限
ls -la ~/.echo-agent/config.yaml
# -rw------- 1 user user ... config.yaml

# 修复权限
chmod 600 ~/.echo-agent/config.yaml
```

!!! tip "使用环境变量存储密钥"
    将 API Key 通过环境变量 `ECHO_AGENT_*` 传入，而非写入配置文件，可降低密钥泄露风险。配合 systemd 的 `EnvironmentFile` 或 secret manager 使用效果更佳。

---

## 生产环境配置模板

综合以上加固项的推荐配置：

```yaml
# ~/.echo-agent/config.yaml — 生产环境
security:
  profile: strict

tools:
  profile: coding
  disabled:
    - "shell_execute"

gateway:
  host: 127.0.0.1
  port: 8420
  auth:
    mode: allowlist
    api_tokens:
      - "${ECHO_AGENT_API_TOKEN}"      # 通过环境变量注入
    admin_tokens:
      - "${ECHO_AGENT_ADMIN_TOKEN}"
    allowed_origins:
      - "http://localhost:3000"
    allowed_hosts:
      - "localhost"
```

!!! question "需维护者确认"
    配置文件中是否支持 `${ENV_VAR}` 语法进行环境变量替换？还是需要通过 `ECHO_AGENT_` 前缀的环境变量覆盖？
