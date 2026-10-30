# WhatsApp

**适用范围：独立 Python Runtime。** 桌面端操作见 [Desktop 使用指南](../../desktop/index.md)。

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
    appSecret: "your-meta-app-secret"
    verifyToken: "your-verify-token"
    webhookPath: "/whatsapp"
    host: "0.0.0.0"
    port: 8085
```

## 凭证获取

1. 在 [Meta for Developers](https://developers.facebook.com/) 创建应用
2. 添加 WhatsApp 产品
3. 获取 Phone Number ID 和 Access Token
4. 获取应用 App Secret，用于验证每个 POST 回调的 `X-Hub-Signature-256`
5. 设置 Webhook 回调 URL

## Webhook 配置

1. 在 Meta 控制台配置 Webhook URL：`https://your-domain/whatsapp`
2. Verify Token 必须与配置中的 `verifyToken` 一致
3. 订阅 `messages` 事件

`verifyToken` 用于接入校验，`appSecret` 用于消息签名，两者均须配置。缺失时通道拒绝启动；未认证的 POST 不进入 Runtime。合法消息因 Runtime 暂停或队列繁忙未被接收时返回 503，平台重投递保持同一个服务端事件身份。

本地文件先上传到媒体 API，再按返回的 media ID 发出；远程 HTTP(S) 媒体可使用 link。文本和附件共同使用持久化分段回执。HTTP 5xx、连接异常或缺少消息 ID 表示结果未知，不自动重发该段；请在 Dashboard 投递恢复中核实平台结果后处理。

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
