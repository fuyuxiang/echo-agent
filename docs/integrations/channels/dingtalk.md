# 钉钉（DingTalk）

## 概述

钉钉通道通过 **Stream Mode**（流模式）与钉钉开放平台通信。Stream Mode 采用回调注册 + WebSocket 长轮询机制，无需暴露公网端点，适合内网部署或无固定公网 IP 的场景。

通道支持单聊（1:1）和群聊两种消息类型，通过 `conversation_type` 元数据自动区分聊天类型，并分别调用不同的发送接口。

!!! tip
    Stream Mode 是钉钉推荐的机器人接入方式，免去了 Webhook 回调地址配置和 SSL 证书管理的复杂度。

---

## 配置示例

```yaml
channels:
  dingtalk:
    enabled: true
    app_key: "your-app-key"
    app_secret: "your-app-secret"
    robot_code: "your-robot-code"
    allow_from:
      - "user1"
      - "user2"
```

| 字段 | 必填 | 说明 |
|------|------|------|
| `app_key` | 是 | 企业内部应用的 AppKey |
| `app_secret` | 是 | 企业内部应用的 AppSecret |
| `robot_code` | 是 | 机器人唯一标识码 |
| `allow_from` | 否 | 允许发送消息的用户白名单，为空则不限制 |

---

## 凭证获取

1. 登录 [钉钉开放平台](https://open-dev.dingtalk.com/)
2. 进入「应用开发」→「企业内部应用」→ 点击「创建应用」
3. 在应用信息页获取 **AppKey** 和 **AppSecret**
4. 进入「机器人与消息推送」配置页面
5. 启用机器人功能，获取 **robot_code**
6. 在机器人设置中启用 **Stream Mode**（消息接收模式选择「Stream 模式」）

!!! warning
    AppSecret 仅在创建时显示一次，请妥善保存。如遗失需重新生成，届时旧密钥立即失效。

---

## 回调 / Webhook 设置

Stream Mode 下**无需配置公网回调地址**。连接流程如下：

1. 应用启动时使用 AppKey + AppSecret 向钉钉 API 注册回调
2. 钉钉返回 WebSocket 连接地址
3. 应用建立 WebSocket 长连接，持续接收消息推送
4. 连接断开时自动重连

```
┌─────────┐   注册回调    ┌──────────────┐
│  Agent  │ ────────────→ │  DingTalk API │
│         │ ←──────────── │              │
│         │  WS endpoint  │              │
│         │ ═══════════════│              │
│         │  WebSocket 长连接（消息推送）    │
└─────────┘               └──────────────┘
```

!!! tip
    由于使用 WebSocket 出站连接，部署在 NAT 或防火墙后的服务也可正常工作，仅需允许出站 HTTPS/WSS 流量。

---

## 能力矩阵

| 能力 | 支持情况 | 备注 |
|------|----------|------|
| 消息编辑 | ❌ | 钉钉 API 不支持修改已发送消息 |
| 表情回应 | ❌ | 不支持 |
| 文件发送 | ❌ | 当前未实现 |
| 实时消息 | ✅ | Stream Mode WebSocket 推送 |
| 群聊 | ✅ | 通过 `openConversationId` 路由 |
| 单聊 | ✅ | 通过 staffId 路由 |

**消息发送接口区分：**

- 单聊：`POST /v1.0/robot/oToMessages/batchSend`
- 群聊：`POST /v1.0/robot/groupMessages/send`（需要 `openConversationId`）

---

## 认证机制

钉钉采用 AppKey + AppSecret 换取 `access_token` 的认证方式：

```
POST https://api.dingtalk.com/v1.0/oauth2/accessToken
{
  "appKey": "your-app-key",
  "appSecret": "your-app-secret"
}
```

Token 有效期为 2 小时，通道层自动刷新，无需手动管理。

---

## 常见问题

!!! question "需维护者确认"
    群聊中 `openConversationId` 的获取方式是否需要额外的事件订阅权限？

**Q: 触发了速率限制怎么办？**

钉钉对机器人消息发送有频率限制。当触发限流时，通道会自动进行退避等待（backoff 300 秒），无需手动干预。频繁触发限流时建议：

- 合并短时间内的多条回复为单条消息
- 检查是否有异常的重复消息发送

**Q: Stream Mode 连接不上？**

1. 确认 AppKey / AppSecret / robot_code 三项配置均正确
2. 确认机器人设置中已启用 Stream Mode
3. 检查出站网络是否允许 WSS 连接（端口 443）
4. 查看日志中是否有 token 获取失败的错误

**Q: 消息在群聊中没有回复？**

- 确认 `allow_from` 未将发送者排除在外
- 确认机器人已被添加到目标群聊
- 检查 `conversation_type` 是否正确识别为群聊
- 群聊需要 @机器人 才能触发响应（取决于配置）

**Q: 如何区分单聊和群聊消息？**

通道通过消息中的 `conversationType` 字段自动区分：
- `1` = 单聊
- `2` = 群聊

不同类型的消息会自动路由到对应的发送接口。
