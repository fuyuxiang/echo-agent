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
    token_header: "X-Echo-Agent-Token"
```

- `api_tokens`：普通用户 Token，可执行对话和任务操作
- `admin_tokens`：管理员 Token，可访问配置修改、服务管理等管理接口
- `token_header`：自定义请求头名称，默认 `X-Echo-Agent-Token`

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
  port: 58123
```

如必须接受远程连接（如容器环境），使用防火墙限制来源：

```bash
# iptables 示例：仅允许内网访问
iptables -A INPUT -p tcp --dport 58123 -s 10.0.0.0/8 -j ACCEPT
iptables -A INPUT -p tcp --dport 58123 -j DROP
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

### security.profile：运行形态

`security.profile` 描述的是**部署形态**，而非"宽松到严格"的强度等级。合法取值只有三个：

| 取值 | 含义 | 追加限制 |
|------|------|----------|
| `personal_cli` | 本机单人使用（默认） | 无 |
| `daemon` | 长期后台运行 | 拒绝 4 个工具 + 4 类能力 |
| `public_gateway` | 网关对外暴露 | 拒绝 11 个工具 + 8 类能力 |

```yaml
security:
  profile: public_gateway    # 对外暴露时使用
```

!!! warning "不存在 relaxed / standard / strict"
    这三个名字不是合法取值，填入会在启动时被配置校验拒绝。请按部署形态选择上表中的取值。

### tools.profile：工具白名单

控制 Agent 可调用的工具范围，四档累加，默认 `full`：

| 档位 | 工具数 | 可用范围 |
|------|--------|----------|
| `minimal` | 14 | 只读问答 |
| `messaging` | 18 | + 记忆与媒体生成 |
| `coding` | 24 | + 文件写入与编排 |
| `full` | 全部 | 白名单为 `*`，放通所有工具（默认） |

```yaml
tools:
  profile: coding    # 生产环境建议不超过 coding
```

!!! danger "默认档位是 full"
    `full` 放通全部工具，包括 `exec`、`execute_code`、`process`、`skill_install`、`skill_manage`、`cronjob` 这 6 个 `HIGH_RISK_TOOLS`。生产环境应显式降到 `coding` 或更低，并配合 `security.profile` 收紧。

两个字段是独立生效的：`tools.profile` 决定白名单，`security.profile` 在白名单之上追加拒绝。完整判定顺序见[安全档位矩阵](../reference/security-profile-matrix.md)。

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
  deny:                    # 显式禁用，优先于档位
    - exec
    - execute_code
  also_allow:              # 在档位基础上额外放行
    - web_search
```

配置中没有 `tools.disabled`。三个列表的关系是：`deny` 显式禁用，`also_allow` 在档位之上追加，`allow` 则完全覆盖档位、只放行列出的工具。

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
  profile: daemon          # 或 public_gateway（对外暴露时）

tools:
  profile: coding          # 不使用默认的 full
  deny:
    - skill_install        # deny 是第一层判定，无法被绕过

gateway:
  host: 127.0.0.1
  port: 58123
  auth:
    mode: allowlist
    allowed_origins:
      - "http://localhost:3000"
    allowed_hosts:
      - "localhost"
```

禁用工具用 `tools.deny`，配置中没有 `tools.disabled` 字段。

!!! warning "令牌不要写在配置文件里"
    配置文件**不支持** `${ENV_VAR}` 形式的环境变量替换 —— 写成 `"${ECHO_AGENT_API_TOKEN}"` 会被当作字面字符串存为令牌值，而不是读取环境变量。

    `api_tokens` 与 `admin_tokens` 是列表类型，也**无法**用环境变量注入 —— 环境变量的值一律是字符串，`ECHO_AGENT_GATEWAY__AUTH__API_TOKENS='["x"]'` 会因类型不符被校验拒绝。

    可行的做法是把令牌写在配置文件里，并用文件权限保护：

    ```bash
    chmod 600 ~/.echo-agent/echo-agent.yaml
    ```

    确保该文件不进版本库。环境变量的覆盖规则与适用范围见[环境变量参考](../reference/environment-variables.md)。
