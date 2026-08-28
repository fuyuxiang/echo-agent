# Gateway 认证详解

Gateway 提供三种认证模式，适应从开发调试到生产部署的不同场景。认证系统同时管理用户身份验证和 API 令牌鉴权两个层面。

## 认证模式

### open 模式

完全开放，不做任何用户身份限制。

```yaml
gateway:
  auth:
    mode: "open"
```

!!! warning "仅限开发环境"
    `open` 模式不对任何请求进行身份校验，任何人均可访问所有接口。绝不应在生产环境或公网可达的实例上使用此模式。

### allowlist 模式

仅允许预先配置的用户列表访问。用户标识支持两种格式：

- 纯用户 ID：`"user1"` — 匹配任意平台上该 ID 的用户
- 平台限定格式：`"telegram:123456"` — 仅匹配指定平台的指定用户

```yaml
gateway:
  auth:
    mode: "allowlist"
    allowed_users:
      - "alice"
      - "bob"
      - "telegram:123456"
      - "wechat:wx_abcdef"
```

!!! tip "平台限定格式的优先级"
    当同一个用户 ID 同时出现纯 ID 和平台限定格式时，平台限定格式优先匹配。推荐在多平台环境中使用平台限定格式以避免跨平台 ID 冲突。

### pairing 模式

用户通过临时配对码完成首次认证绑定，配对码具有 TTL（生存时间）限制。已通过配对的用户信息会被持久化存储。同时支持 allowlist 作为后备方案。

```yaml
gateway:
  auth:
    mode: "pairing"
    allowed_users:
      - "admin_user"  # allowlist 后备，这些用户无需配对
```

配对流程：

1. 管理员通过 API 或仪表盘生成配对码（有效期有限）
2. 新用户在客户端输入配对码
3. 系统验证配对码有效性并绑定用户身份
4. 后续请求使用已绑定的身份自动通过认证

!!! warning "配对码安全"
    配对码应通过安全渠道（如私信、加密邮件）传递给目标用户。配对码一旦使用即失效，不可重复使用。

## 令牌认证

独立于用户认证之外，Gateway 还提供基于令牌（Token）的 API 访问控制：

### api_tokens（普通权限）

具有读取和聊天级别的访问权限，适用于第三方集成和自动化脚本。

```yaml
gateway:
  auth:
    api_tokens:
      - "tk-proj-abc123def456"
      - "tk-integration-xyz789"
```

### admin_tokens（管理员权限）

具有管理员级别的完整访问权限，隐含包含普通令牌的所有权限。

```yaml
gateway:
  auth:
    admin_tokens:
      - "atk-master-key-do-not-share"
```

!!! tip "令牌命名建议"
    建议为令牌添加有意义的前缀（如 `tk-`、`atk-`）和用途标识，便于审计时追踪令牌来源。

### 令牌传递方式

令牌通过 HTTP 请求头传递，默认请求头名称为 `X-API-Token`：

```http
GET /api/sessions HTTP/1.1
Host: localhost:8090
X-API-Token: tk-proj-abc123def456
```

可通过配置自定义请求头名称：

```yaml
gateway:
  auth:
    token_header: "X-API-Token"  # 默认值
```

!!! warning "管理操作只认请求头"
    HTTP 管理端点（例如技能导入/安装/删除和知识库上传/删除）不接受 URL 的
    `?token=`，admin 令牌必须走请求头 —— 否则它会留在访问日志、代理日志与
    referrer 里，存活期远长于其本身。

    会话 WebSocket 的兼容入口仍允许从 `?token=` 读取 api 令牌，但生产环境也应
    优先使用请求头或 auth 帧。详见 [WebSocket 协议参考](../../reference/websocket-protocol.md)。

## 环回地址豁免

来自 `127.0.0.1` 或 `::1`（localhost）的请求可以绕过用户身份认证，简化本地开发和内部服务间调用。

!!! warning "环回豁免不影响令牌认证"
    环回豁免仅跳过用户身份验证。对于需要令牌的 API 端点（如管理接口），即使是本地请求仍然需要提供有效的 `api_token` 或 `admin_token`。

!!! warning "DNS 重绑定防护"
    环回豁免通过 Host 请求头验证进行 DNS 重绑定防护。如果 Host 头不在 `allowed_hosts` 列表中，即使来源 IP 是 127.0.0.1，请求也会被拒绝。

## 配对失败锁定

为防止暴力猜测配对码，系统实施以下锁定策略：

- **阈值**：5 次连续失败尝试
- **锁定时长**：300 秒（5 分钟）
- **锁定粒度**：按来源 IP 或用户标识

```
第 1-4 次失败 → 返回 401 Unauthorized
第 5 次失败   → 触发锁定，返回 429 Too Many Requests
锁定期间      → 所有配对请求直接返回 429，不做验证
300 秒后      → 自动解锁，计数器重置
```

## 管理员用户

除了 `admin_tokens` 之外，还可以通过 `admin_users` 列表直接赋予特定用户管理员权限：

```yaml
gateway:
  auth:
    admin_users:
      - "super_admin"
      - "telegram:999888"
```

管理员用户可以访问所有 API 端点，包括用户管理、配置修改、系统控制等敏感操作。

## CORS 来源白名单

当 Gateway 被浏览器端应用（如 Web 仪表盘）访问时，需要配置 CORS 允许的来源：

```yaml
gateway:
  auth:
    allowed_origins:
      - "https://my-dashboard.example.com"
      - "https://admin.example.com"
```

!!! tip "开发环境 CORS"
    开发时可添加 `http://localhost:3000` 等本地地址。生产环境应严格限制为实际使用的域名。

## Host 头白名单

用于防止 DNS 重绑定攻击和虚拟主机混淆：

```yaml
gateway:
  auth:
    allowed_hosts:
      - "gateway.example.com"
```

比对前，请求的 Host 头与配置条目会用同一个函数规范化（`gateway/host_rules.py`）：转小写、剥掉端口、IPv6 统一成方括号形式。因此 `Echo.Example.com`、`echo.example.com:58123`、裸写的 `::1` 都能正常匹配——直接从浏览器地址栏粘贴过来也可以。

通配地址（`0.0.0.0`、`::`、空字符串）不是有效条目，会被直接丢弃。浏览器发送的是地址栏里的名字，永远不会是服务端绑定用的通配符，所以 `allowed_hosts: [0.0.0.0]` 匹配不到任何请求，却看着像已经配好了——这种写法等同于「未配置」，启动时仍会告警。

### `allowed_hosts` 为空时的默认行为

判定按三段顺序进行，空配置**不会**从绑定地址派生 Host 值：

| 情形 | 允许的 Host |
|------|-------------|
| `allowed_hosts` 有可用条目 | 仅列表中的条目 |
| 为空且绑定在回环地址 | 固定的回环名集合：`localhost`、`127.0.0.1`、`[::1]` |
| 为空且绑定在非回环地址 | 无默认值，所有 Host 一律拒绝 |

第三种情形是有意为之：把网关绑到 `0.0.0.0` 的部署必须显式列出自己的域名，回环豁免不会顺延到攻击者可控的名字上。空 Host 头同样按不可信处理。

「绑定在回环地址」按地址判定，不是按字面量匹配：`127.0.0.0/8` 全段（含 `127.0.0.2`）、`::1`、`[::1]` 与 `localhost` 都算本机。而 `host` 留空**不算**本机——空字符串与 `::` 一样是通配绑定，等同 `0.0.0.0`，落在第三种情形里。

### 实际影响范围

第三种情形并不会让整个 Dashboard 打不开。Host 校验位于 `_check_csrf`，只有 `_require_admin_token` 会调用它，所以：

- 登录、概览等只读页面（走 `_require_api_token`）仍然可用；
- 本机客户端（`echo-agent cli`、curl，不带 Origin）不受影响；
- 会话、配置、记忆写入、任务、定时、知识库等管理端点全部返回 403 `cross-site request forbidden`。

这个差异值得留意：只看到首页能打开就断定「告警是误报」，会让管理功能一直不可用。

## 审计日志

所有认证相关事件（成功、失败、锁定、令牌使用）均记录到审计日志：

- 日志路径：`gateway_auth/audit.jsonl`
- 格式：每行一条 JSON 记录
- 包含字段：时间戳、事件类型、来源 IP、用户标识、结果

```json
{"ts": "2024-01-15T10:30:00Z", "event": "auth_success", "ip": "192.168.1.100", "user": "telegram:123456", "mode": "allowlist"}
{"ts": "2024-01-15T10:30:05Z", "event": "auth_failure", "ip": "10.0.0.50", "user": "unknown", "mode": "pairing", "reason": "invalid_code"}
{"ts": "2024-01-15T10:32:00Z", "event": "lockout_triggered", "ip": "10.0.0.50", "attempts": 5, "lockout_seconds": 300}
```

## 完整配置参考

```yaml
gateway:
  auth:
    mode: "allowlist"              # open | allowlist | pairing
    allowed_users:                 # allowlist / pairing 后备用户
      - "alice"
      - "telegram:123456"
    admin_users:                   # 管理员用户列表
      - "super_admin"
    api_tokens:                    # 普通 API 令牌
      - "tk-proj-abc123"
    admin_tokens:                  # 管理员令牌
      - "atk-master-key"
    token_header: "X-API-Token"   # 令牌请求头名称
    allowed_origins:               # CORS 来源白名单
      - "https://dashboard.example.com"
    allowed_hosts:                 # Host 头白名单
      - "gateway.example.com"
```

## 相关文档

- [Gateway 概览](index.md)
- [反向代理配置](reverse-proxy.md)
