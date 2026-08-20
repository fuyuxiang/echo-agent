# 飞书（Feishu/Lark）

## 概述

飞书通道通过事件订阅（Event Subscription）接收消息，通过 REST API 发送消息。需要公网可达的 Webhook 端点接收事件推送，支持单聊和群聊（通过 @提及 策略控制）。

!!! warning "公网端点要求"
    飞书事件订阅需要公网可访问的回调地址。部署前请确保回调端口可被飞书服务器访问。

!!! tip "群聊支持"
    飞书通道支持群聊消息，通过 `group_policy` 配置控制响应策略：仅响应 @机器人 的消息，或响应群内所有消息。

## 配置示例

```yaml
channels:
  feishu:
    app_id: "cli_a1b2c3d4e5f6g7h8"
    app_secret: "your-app-secret-here"
    verification_token: "your-verification-token"
    encryption_key: "your-encryption-key"
    webhook_path: "/feishu"
    host: "0.0.0.0"
    port: 8083
    group_policy: "mention"     # mention | all
    bot_open_id: "ou_abcdef1234567890"
```

| 字段 | 必填 | 默认值 | 说明 |
|------|------|--------|------|
| `app_id` | 是 | — | 应用 App ID |
| `app_secret` | 是 | — | 应用 App Secret |
| `verification_token` | 是 | — | 事件订阅验证 Token |
| `encryption_key` | 否 | — | 事件加密密钥（推荐配置） |
| `webhook_path` | 否 | `/feishu` | 回调路径 |
| `host` | 否 | `0.0.0.0` | 监听地址 |
| `port` | 否 | `8083` | 监听端口 |
| `group_policy` | 否 | `mention` | 群聊策略：`mention` 仅 @机器人 时响应，`all` 响应全部 |
| `bot_open_id` | 否 | — | 机器人 open_id，用于 @提及 检测 |

## 凭证获取

### 步骤 1：创建飞书应用

1. 登录 [飞书开放平台](https://open.feishu.cn/app)
2. 点击 **创建企业自建应用**
3. 填写应用名称和描述
4. 创建完成后进入应用详情

### 步骤 2：获取凭证

1. 在应用详情 → **凭证与基础信息** 页面
2. 记录 **App ID** 和 **App Secret**

### 步骤 3：启用机器人能力

1. 进入 **应用能力** → **机器人**
2. 开启机器人功能

### 步骤 4：配置权限

在 **权限管理** 中添加以下权限并申请审批：

| 权限 | 权限标识 | 用途 |
|------|----------|------|
| 获取与发送单聊、群组消息 | `im:message` | 发送消息 |
| 接收群聊中@机器人消息事件 | `im:message.receive_v1` | 接收消息事件 |

!!! tip "权限审批"
    部分权限需要管理员审批。创建应用后尽早申请权限，避免部署时权限未通过。

### 步骤 5：配置事件订阅

1. 进入 **事件订阅** 页面
2. 配置 **请求地址**：`https://your-domain.com/feishu`
3. 记录 **Verification Token**
4. 设置 **Encrypt Key**（推荐）
5. 添加事件：`im.message.receive_v1`（接收消息）

## 回调/Webhook 设置

### Challenge-Response 验证

飞书在配置回调 URL 时会发送验证请求：

```json
{
  "challenge": "ajls384kdjx98XX",
  "token": "your-verification-token",
  "type": "url_verification"
}
```

正确响应：

```json
{
  "challenge": "ajls384kdjx98XX"
}
```

### 事件推送格式

消息事件推送示例（启用加密时需先解密）：

```json
{
  "schema": "2.0",
  "header": {
    "event_id": "unique-event-id",
    "event_type": "im.message.receive_v1",
    "create_time": "1234567890",
    "token": "your-verification-token",
    "app_id": "cli_a1b2c3d4e5f6g7h8"
  },
  "event": {
    "sender": {
      "sender_id": {
        "open_id": "ou_sender_open_id"
      }
    },
    "message": {
      "message_id": "om_abcdefg",
      "chat_id": "oc_chatid123",
      "message_type": "text",
      "content": "{\"text\":\"hello\"}"
    }
  }
}
```

### 事件加密

当配置了 `encryption_key` 时，事件内容会被加密传输：

```json
{
  "encrypt": "encrypted-event-content-base64"
}
```

解密流程：AES-256-CBC，密钥为 SHA256(encryption_key) 的前 32 字节。

### Tenant Access Token

发送消息需要 `tenant_access_token`：

```text
POST https://open.feishu.cn/open-apis/auth/v3/tenant_access_token/internal
{
  "app_id": "cli_a1b2c3d4e5f6g7h8",
  "app_secret": "your-app-secret"
}
```

!!! warning "Token 有效期"
    `tenant_access_token` 有效期为 7200 秒（2 小时），系统会自动刷新。

## 能力矩阵

| 能力 | 支持 | 备注 |
|------|------|------|
| 发送文本 | ✅ | — |
| 发送图片 | ❌ | 当前未实现 |
| 发送文件 | ❌ | 当前未实现 |
| 编辑消息 | ❌ | 当前未实现 |
| 表情回应 | ❌ | 当前未实现 |
| 群聊 | ✅ | 通过 @提及 策略控制 |
| 实时消息 | ✅ | 事件订阅推送 |

## 技术细节

### 消息去重

通过 `message_id` 集合进行去重。飞书可能因网络原因重复推送同一事件，系统维护已处理的 message_id 集合防止重复响应。

### 回复方式

使用 `reply` 接口通过 `message_id` 进行回复：

```text
POST https://open.feishu.cn/open-apis/im/v1/messages/{message_id}/reply
```

`receive_id_type` 默认使用 `chat_id`，支持在单聊和群聊场景下统一处理。

### 群聊 @提及 检测

群聊消息通过 `bot_open_id` 判断是否被 @：

1. 解析消息中的 mention 列表
2. 检查是否包含 `bot_open_id`
3. 若未配置 `bot_open_id`，回退使用 `app_id` 进行匹配

!!! tip "建议显式配置 bot_open_id"
    `bot_open_id` 可在机器人详情页或通过 API 获取。未配置时回退用 `app_id` 匹配 mention 列表，这在部分场景下并不准确——群聊按 `mention` 策略工作时，匹配失败会表现为机器人对 @ 无反应。

## 常见问题

!!! question "Q: Challenge 验证失败？"
    1. 确认服务已启动且回调端口可被公网访问
    2. 检查响应格式是否正确（需返回 JSON 格式的 challenge 值）
    3. 确认 Content-Type 为 `application/json`
    4. 检查是否配置了 `encryption_key` 但未实现解密逻辑

!!! question "Q: 收不到事件推送？"
    1. 确认已添加 `im.message.receive_v1` 事件订阅
    2. 检查应用权限是否已审批通过
    3. 确认机器人能力已启用
    4. 确认用户/群组在应用的可用范围内

!!! question "Q: 群聊中机器人不响应？"
    1. 检查 `group_policy` 配置：`mention` 模式下需要 @机器人
    2. 确认 `bot_open_id` 配置正确
    3. 确认机器人已被添加到目标群组

!!! question "Q: tenant_access_token 获取失败？"
    1. 检查 `app_id` 和 `app_secret` 是否正确
    2. 确认应用状态为已启用
    3. 检查网络是否可访问飞书开放平台 API

!!! question "Q: 为什么收到重复消息回复？"
    飞书可能因超时重试事件推送。确认消息去重机制正常工作，检查 `message_id` 去重集合是否被正确维护。
