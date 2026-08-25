# MCP (Model Context Protocol)

通过 MCP 协议连接外部工具服务器，扩展 Agent 的工具能力。

---

## 概述

MCP 是一种标准化的 AI 工具通信协议。Echo Agent 作为 MCP 客户端，可以连接 MCP Server 获得额外工具能力。

**实现范围（请先读这一节）**

- 协议版本：请求 `2025-06-18`，可向下协商到 `2025-03-26` / `2024-11-05`；服务端返回其他版本时连接会被明确拒绝，而不是带着不确定的分帧继续跑。
- 传输方式：stdio 与 Streamable HTTP。两者都对官方 MCP Python SDK 做过互操作验证。
- 已实现：`tools/list`（含 `nextCursor` 分页）、`tools/call`、`resources/*` 与 `prompts/*`（经 `mcp_resources` / `mcp_prompts` 工具接入 Agent）、`tools/list_changed` 通知、运行期重连、会话终止。
- **未实现**：sampling（客户端不声明该能力，服务端发来的请求会收到 `-32601`）、elicitation、roots、progress 通知。
- MCP 2026-07-28 的无握手 stateless 协议尚未支持。

## 配置

```yaml
tools:
  mcp:
    enabled: true          # 总开关；false 时下面配置的服务一个都不连
  mcp_servers:
    filesystem:
      command: "npx"
      args: ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/dir"]
      enabled: true

    web-search:
      url: "http://localhost:8080/mcp"
      headers:
        Authorization: "Bearer ${MCP_TOKEN}"
      enabled: true
```

`url` 与 `command` 必须二选一，同时配置会在加载期报错（不再静默优先某一个）。

服务名（`mcp_servers` 的键）会参与工具名和凭据文件名，只允许字母、数字与 `.` `-` `_`。

## 传输方式

### Stdio（子进程）

```yaml
tools:
  mcp_servers:
    my-server:
      command: "python"
      args: ["-m", "my_mcp_server"]
      env:
        API_KEY: "${MY_API_KEY}"
```

`env` 与 `headers` 支持 `${VAR}` 和 `$VAR` 展开。**变量未设置时会直接报错**，而不是把字面量 `${VAR}` 当作值发出去 —— 后者会让认证失败表现为服务端一个难以追查的 401。

### Streamable HTTP（远程）

```yaml
tools:
  mcp_servers:
    remote:
      url: "https://mcp.example.com/mcp"
      headers:
        Authorization: "Bearer token"
      auth: "oauth"        # 可选：OAuth 2.1 PKCE 浏览器授权
```

每个 POST 都带规范要求的 `Accept: application/json, text/event-stream`，握手后带 `MCP-Protocol-Version`；响应为 JSON 或 SSE 均可处理（SSE 分帧兼容 `\r\n\r\n`、`\n\n`、`\r\r`）；4xx/5xx 会作为错误抛出而不是静默入队；会话被服务端判为过期（404）时重建，关闭时发 `DELETE` 显式终止。

注意 `execution.networkPolicy` 默认为 `deny`，此时 HTTP 类型的 MCP 服务会被跳过；要连远程服务需显式放开。

### OAuth

`auth: "oauth"` 时的流程：发现 Protected Resource Metadata → 读取授权服务器 metadata → authorization code + PKCE（携带 RFC 8707 `resource` 参数）→ 需要时动态注册客户端并持久化。

凭据落盘为 `0600`（目录 `0700`）、原子写入。授权端点强制 HTTPS（loopback 例外），且 metadata 里声明的 endpoint 必须与 issuer 同源 —— 否则拒绝，不会把授权码和 PKCE verifier 发往另一个源。

## 工具过滤

```yaml
tools:
  mcp_servers:
    my-server:
      command: "..."
      tools_include: ["read_file", "write_file"]  # 白名单
      tools_exclude: ["dangerous_tool"]           # 黑名单
```

## 资源与提示词

除了工具，MCP 服务还可以发布**资源**（resources，作为上下文的文件/数据）和**提示词模板**（prompts）。这两类能力通过两个内置工具暴露给模型，采用渐进披露：先 `list` 拿紧凑元数据，再取单个条目 —— 一个发布上千资源的服务因此只花几百 token 就能被发现，而不是一次调用撑爆上下文。

```
mcp_resources  action=list                    # 发现资源
               action=read   uri=config://app # 读取单个资源
               action=templates               # 列出带参数的 URI 模板
mcp_prompts    action=list                    # 发现模板(含各自的参数声明)
               action=get    name=review  arguments={...}
```

只连了一个服务时 `server` 参数可省略；连了多个则必须指定 —— 隐式挑一个会让同一次调用在不同时刻含义不同。

两个工具都是 `read_only`：读取不会改变服务端状态，按 `exec` 审批属于过度拦截，而每次查上下文都弹审批的能力最终不会有人用。它们携带 `mcp.call` capability，因此和工具调用一样受 `public_gateway` / `daemon` 档位的默认拒绝约束。

**内容仍然是不可信数据。** 资源正文与模板消息同样是外部文本、同样进模型上下文。命中注入特征时不会丢弃内容（用户要读的东西应该能读到），而是加一条显式横幅标注"这是数据不是指令"；`trust_level: trusted` 的服务不加横幅，避免一份讲提示词工程的资源被永久标成恶意。渲染模板时保留 role 分界，否则服务端自撰的 `assistant` 轮次会读起来像本 Agent 已经同意了什么。

## 安全

### 信任级别与审批（重要）

MCP 规范明确 `ToolAnnotations` 只是**提示**，来自不可信服务端时不得作为安全判据。因此本项目按服务端而非按 payload 判定信任：

```yaml
tools:
  mcp_servers:
    my-server:
      command: "..."
      trust_level: "untrusted"   # 默认；可选 "trusted"
```

- `untrusted`（默认）：该服务的工具**至少按 `exec` 审批**（即首次调用需人工批准）。服务端声明的 `readOnlyHint` **不能**降低审批等级；`destructiveHint` 会升级到 `dangerous`。
- `trusted`：采信 annotations，`readOnlyHint` 可降到 `read_only`。**只对你自己掌控的服务使用。**

换句话说，annotations 只能升高风险等级，不能降低。

### 其他约束

- 工具名、描述以及 `inputSchema` 内的描述/标题都会做注入扫描；`tools.mcpSecurityPolicy` 为 `block`（默认）时拒绝可疑工具，`warn` 时仅告警。
- 与内置工具同名、或同一批内清洗后互相碰撞的 MCP 工具会被拒绝（碰撞时双方都拒绝，避免一个工具名指向另一个工具）。
- `inputSchema` 结构非法（如 `array` 缺 `items`）的工具在注册前就被拒绝并记录原因。
- MCP 工具携带 `mcp.call` capability，`public_gateway` 与 `daemon` 档位默认拒绝，需要在 `tools.allow` 中显式放行。
- 受 `tools.profile` 和 `permissions.approval.mode` 约束。

## 重连与生命周期

每个服务由一个 supervisor 看护：

- 首次连接失败按 1, 2, 4, 8, 16 秒退避重试，最多 5 次；每次重试都新建 transport，失败的连接（含 stdio 子进程）会被关闭，不留孤儿进程。
- 运行期断线后重建连接：先注销该服务已注册的工具，重连成功后重新发现并注册。
- 收到 `notifications/tools/list_changed` 时重新发现工具。
- 连接断开时，等待中的调用立即失败，不会挂满超时（不再出现"一个畸形帧导致此后每次调用都等满 120 秒"的情况）。
- 断开的服务其工具会报告为 not ready。注意：主循环发给模型的工具列表用的是全量定义，因此工具在被 supervisor 注销前仍可能出现在列表中，此时调用会立即返回连接错误；`task` / `delegate` 派生的子 agent 用的是 ready 过滤后的列表，不会看到它们。
- `stop_all()` 会注销所有 MCP 工具，关闭后模型不会再看到它们。

## 超时

- `connect_timeout`（默认 60s）：TCP 连接建立与 `initialize` 握手。
- `timeout`（默认 120s）：单次工具调用。

两者独立生效。调用超时后会向服务端发 `notifications/cancelled`，避免服务端继续为无人等待的请求消耗资源。

## 协议支持明细

| 能力 | 状态 |
|---|---|
| stdio `tools/list` / `tools/call` | 可用 |
| Streamable HTTP（JSON 与 SSE） | 可用 |
| 游标分页（`nextCursor`） | 可用 |
| `tools/list_changed` | 可用 |
| 运行期重连 / 工具注销 | 可用 |
| 会话终止（DELETE）与 404 重建 | 可用 |
| `structuredContent` / audio / resource_link | 可用（渲染进工具输出） |
| OAuth 2.1 PKCE + 动态注册 | 可用 |
| `resources/*`、`prompts/*` | 可用（`mcp_resources` / `mcp_prompts` 工具） |
| sampling / elicitation / roots | 未实现（不声明该能力） |
| progress 通知 | 未消费 |
| 旧 HTTP+SSE 传输 | 不支持（已移除） |
| MCP 2026-07-28 stateless | 不支持 |
