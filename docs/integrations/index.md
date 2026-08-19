# 集成与扩展

Echo Agent 通过多层扩展体系与外部系统集成，覆盖消息通道、API 网关、技能系统、插件机制及多智能体协议。

## 架构概览

```
┌─────────────────────────────────────────────────┐
│                  Echo Agent Core                  │
├──────────┬──────────┬──────────┬────────────────┤
│ Channels │ Gateway  │  Skills  │    Plugins     │
│ (14 个)  │ HTTP/WS  │ (35 个)  │  plugin.yaml   │
├──────────┴──────────┴──────────┴────────────────┤
│         MCP (工具扩展)  │  A2A (多智能体协作)    │
└─────────────────────────────────────────────────┘
```

## 模块导览

| 模块 | 说明 | 文档 |
|------|------|------|
| [消息通道](channels/index.md) | 14 个平台适配器，覆盖 IM、邮件、Webhook、CLI | 通道配置与能力矩阵 |
| [Gateway](gateway/index.md) | HTTP/WebSocket API 网关，提供认证、限流、会话管理 | 认证模式与反向代理 |
| [技能系统](skills/using-skills.md) | 35 个内置技能，按类别组织 | 使用与目录 |
| [插件系统](plugins/using-plugins.md) | 基于 plugin.yaml 的扩展机制 | 使用与开发 |
| [MCP](mcp.md) | Model Context Protocol 客户端，连接外部工具服务器 | 配置与使用 |
| [A2A](a2a.md) | Agent-to-Agent 协议，实现多智能体任务委派 | 协议与集成 |

## 快速开始

### 启用一个通道

在 `config.yaml` 中开启对应通道：

```yaml
channels:
  telegram:
    enabled: true
    token: "YOUR_BOT_TOKEN"
```

### 连接 MCP 工具服务器

```yaml
mcp:
  servers:
    filesystem:
      enabled: true
      command: "npx"
      args: ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/dir"]
```

### 启用 A2A 协议

```yaml
a2a:
  enabled: true
  agent_card:
    name: "my-agent"
    description: "My Echo Agent instance"
```

## 设计原则

- **统一消息总线** — 所有通道通过 `MessageBus` 收发消息，通道间解耦
- **能力自描述** — 每个通道声明自身能力（编辑、表情、文件），上层按能力路由
- **渐进式启用** — 所有集成默认关闭，按需开启，零配置即可运行 CLI 模式
- **安全优先** — 每层独立鉴权：通道白名单、Gateway token、插件权限沙箱
