# WhatsApp

通过 WhatsApp Business API 接入 Echo Agent。

---

## 概述

WhatsApp 通道使用 Meta 的 WhatsApp Business API (Cloud API)，通过 Webhook 接收消息。

## 配置

```yaml
channels:
  whatsapp:
    enabled: true
    phoneNumberId: "your-phone-number-id"
    accessToken: "your-access-token"
    verifyToken: "your-verify-token"
    webhookPath: "/whatsapp"
    host: "0.0.0.0"
    port: 8085
```

## 凭证获取

1. 在 [Meta for Developers](https://developers.facebook.com/) 创建应用
2. 添加 WhatsApp 产品
3. 获取 Phone Number ID 和 Access Token
4. 设置 Webhook 回调 URL

## Webhook 配置

1. 在 Meta 控制台配置 Webhook URL：`https://your-domain/whatsapp`
2. Verify Token 必须与配置中的 `verifyToken` 一致
3. 订阅 `messages` 事件

## 能力

| 能力 | 支持 |
|------|------|
| 编辑消息 | ❌ |
| 表情回应 | ❌ |
| 文件发送 | ✅ |
| 实时响应 | ✅ |
| 群聊 | ✅ |

## 常见问题

**收不到消息？**
- 确认 Webhook URL 可公网访问
- 检查 Verify Token 是否匹配
- 确认已订阅 messages 事件

**消息发送失败？**
- 检查 Access Token 是否过期
- 确认对方号码格式正确（含国际区号）
