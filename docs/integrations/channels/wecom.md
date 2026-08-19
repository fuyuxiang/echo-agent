# 企业微信（WeCom）

## 概述

企业微信通道通过 Webhook 回调接收消息，通过 REST API 发送消息。需要公网可达的回调端点，适合部署在有公网 IP 或通过反向代理暴露的服务器上。

!!! warning "公网端点要求"
    企业微信要求配置可公网访问的回调 URL，用于接收消息推送。部署前请确保服务端口可被企业微信服务器访问。

!!! tip "安全验证"
    回调消息使用 AES 加密 + SHA1 签名双重保护，确保消息来源可信且内容不被篡改。

## 配置示例

```yaml
channels:
  wecom:
    corp_id: "ww1234567890abcdef"
    agent_id: "1000002"
    secret: "your-app-secret-here"
    token: "your-callback-token"
    encoding_aes_key: "43-char-base64-encoding-aes-key-from-wecom"
    webhook_path: "/wecom"
    host: "0.0.0.0"
    port: 8084
```

| 字段 | 必填 | 默认值 | 说明 |
|------|------|--------|------|
| `corp_id` | 是 | — | 企业 ID |
| `agent_id` | 是 | — | 自建应用 AgentId |
| `secret` | 是 | — | 应用 Secret |
| `token` | 是 | — | 回调 Token（用于签名验证） |
| `encoding_aes_key` | 是 | — | 回调 EncodingAESKey（43 位 Base64） |
| `webhook_path` | 否 | `/wecom` | 回调路径 |
| `host` | 否 | `0.0.0.0` | 监听地址 |
| `port` | 否 | `8084` | 监听端口 |

## 凭证获取

### 步骤 1：获取企业 ID（Corp ID）

1. 登录 [企业微信管理后台](https://work.weixin.qq.com/wework_admin/frame)
2. 进入 **我的企业** → **企业信息**
3. 找到 **企业ID** 字段，复制保存

### 步骤 2：创建自建应用

1. 进入 **应用管理** → **自建**
2. 点击 **创建应用**
3. 填写应用名称、Logo、可见范围
4. 创建完成后记录 **AgentId** 和 **Secret**

!!! tip "Secret 只显示一次"
    应用 Secret 创建后仅显示一次，请立即保存。如遗失需重新生成。

### 步骤 3：配置回调

1. 在应用详情页，找到 **接收消息** → **设置API接收**
2. 填写回调 URL：`https://your-domain.com/wecom`
3. 生成或自定义 Token 和 EncodingAESKey
4. 点击保存（此时企业微信会发送验证请求）

## 回调/Webhook 设置

### URL 验证流程

企业微信在保存回调配置时会发送 GET 请求进行验证：

```text
GET /wecom?msg_signature=xxx&timestamp=xxx&nonce=xxx&echostr=xxx
```

验证步骤：

1. 取出 `msg_signature`、`timestamp`、`nonce`、`echostr` 参数
2. 用 `token`、`timestamp`、`nonce`、解密后的 `echostr` 计算 SHA1 签名
3. 比对签名是否与 `msg_signature` 一致
4. 验证通过后，返回解密后的 `echostr` 明文

```text
SHA1(sort(token, timestamp, nonce, echostr_decrypt)) == msg_signature
```

### 消息接收流程

正常消息通过 POST 请求推送：

```text
POST /wecom?msg_signature=xxx&timestamp=xxx&nonce=xxx

<xml>
  <ToUserName><![CDATA[corp_id]]></ToUserName>
  <Encrypt><![CDATA[encrypted_content]]></Encrypt>
  <AgentID>1000002</AgentID>
</xml>
```

解密流程：

1. 验证 `msg_signature`（SHA1(sort(token, timestamp, nonce, encrypt))）
2. 使用 `encoding_aes_key` 进行 AES 解密
3. 解析 XML 获取消息内容

### Access Token

发送消息需要 `access_token`，通过 Corp ID + Secret 获取：

```text
GET https://qyapi.weixin.qq.com/cgi-bin/gettoken?corpid=ID&corpsecret=SECRET
```

!!! warning "Token 有效期"
    `access_token` 有效期为 7200 秒（2 小时），系统会自动刷新。请勿频繁请求以避免触发频率限制。

## 能力矩阵

| 能力 | 支持 | 备注 |
|------|------|------|
| 发送文本 | ✅ | — |
| 发送图片 | ❌ | 当前未实现 |
| 发送语音 | ❌ | 当前未实现 |
| 发送文件 | ❌ | 当前未实现 |
| 编辑消息 | ❌ | 企业微信不支持 |
| 表情回应 | ❌ | 企业微信不支持 |
| 群聊 | ❌ | 当前未实现 |
| 实时消息 | ✅ | Webhook 推送 |

## 技术细节

### 消息加解密

使用 `wecom_crypto.py` 模块处理：

- 加密算法：AES-256-CBC
- 密钥：Base64Decode(EncodingAESKey + "=")，取前 32 字节
- IV：密钥的前 16 字节
- 填充方式：PKCS#7

### 签名验证

```text
signature = SHA1(sort([token, timestamp, nonce, encrypt_msg]))
```

所有回调请求都必须通过签名验证，防止伪造请求。

## 常见问题

!!! question "Q: 回调 URL 验证失败怎么办？"
    1. 确认服务已启动且端口可被公网访问
    2. 检查 Token 和 EncodingAESKey 是否与管理后台一致
    3. 确认回调路径正确（默认 `/wecom`）
    4. 查看日志中的签名计算结果，对比请求参数

!!! question "Q: 收不到消息推送？"
    1. 确认应用的可见范围包含了目标用户
    2. 检查回调 URL 是否验证通过（管理后台显示绿色状态）
    3. 确认防火墙未拦截企业微信服务器 IP

!!! question "Q: access_token 获取失败？"
    1. 检查 Corp ID 和 Secret 是否正确
    2. 确认应用未被停用
    3. 注意 IP 白名单设置（如已配置）

!!! question "Q: 如何确认消息来自企业微信？"
    通过 `msg_signature` 签名验证。签名基于 Token + Timestamp + Nonce + EncryptedContent 的 SHA1 值，无法伪造。
