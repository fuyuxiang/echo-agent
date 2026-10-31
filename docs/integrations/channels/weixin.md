# 微信个人号（iLink Bot）

**适用范围：独立 Python Runtime。** 桌面端操作见 [Desktop 使用指南](../../desktop/index.md)。

## 概述

微信个人号通道通过 iLink Bot API 实现与个人微信账号的对接。采用 HTTP 长轮询方式获取消息，无需公网端点，适合在 NAT 或防火墙后部署。

!!! warning "适用范围"
    本通道仅适用于 **个人微信账号**，不适用于微信公众号（Official Account）或微信小程序。公众号接入请参考其他方案。

!!! tip "无需公网 IP"
    iLink Bot 采用 HTTP 长轮询模式，客户端主动拉取消息，因此部署环境无需暴露公网端口或配置反向代理。

## 配置示例

```yaml
channels:
  weixin:
    account_id: "wxid_xxxxxxxxxx"
    token: "your-ilink-bot-token"
    base_url: "https://ilinkai.weixin.qq.com"
    cdn_base_url: "https://cdn.ilinkai.weixin.qq.com"
    allow_from:
      - "friend_wxid_1"
      - "friend_wxid_2"
    dm_policy: "allow"          # allow | deny | allowlist
    data_dir: "./data/weixin"
```

| 字段 | 必填 | 默认值 | 说明 |
|------|------|--------|------|
| `account_id` | 是 | — | 微信号 wxid |
| `token` | 是 | — | iLink Bot API Token |
| `base_url` | 否 | `https://ilinkai.weixin.qq.com` | API 基础地址 |
| `cdn_base_url` | 否 | — | 媒体 CDN 地址 |
| `allow_from` | 否 | `[]`（全部允许） | 白名单 wxid 列表 |
| `dm_policy` | 否 | `allow` | 私聊策略 |
| `data_dir` | 否 | `./data/weixin` | 本地数据存储路径 |

## 凭证获取

### 1. 获取 iLink Bot Token

1. 访问 iLink Bot 管理后台
2. 创建新的 Bot 实例，获取 API Token
3. 将 Token 填入配置文件的 `token` 字段

### 2. QR 码登录流程

```text
启动通道 → 请求登录二维码 → 终端/日志输出二维码 → 手机微信扫码确认 → 登录成功
```

!!! warning "会话会过期，需要重新扫码"
    扫码登录后的会话有效期由 iLink Bot 服务端决定，不由本项目控制，因此需要周期性重新扫码。日志中出现 `errcode: -14` 即表示会话已失效，须重新登录。

    这意味着该通道无法做到完全无人值守。用于长期运行时，建议对该错误码配置告警，以便及时补扫。

## 定时投递与恢复

接收连接在线不代表微信允许主动发送。回复会使用最近一条用户消息携带的
`context_token`；缺少上下文时，Runtime 会保留定时任务的结果，并提示先给机器人发送消息。
接口返回 `ret=-2` 时，Runtime 保留原始错误，并等待新的入站消息，不把它解释成固定时长的限流或过期。

收到该会话的新消息后，已明确拒收的定时任务结果会自动补发，重启后也可继续恢复。
补发复用原消息和已确认的分段位置，不重新调用模型或执行任务。发送超时、回执丢失等
“结果未知”的投递不会自动重发，需要在 Dashboard「消息投递恢复」中核对接收端后处理。

微信上下文文件采用私有权限、原子替换和完整备份保存。备份只能恢复本地状态，不能延长服务端会话的有效期。
如果登录失效（`-14`），需要重新扫码，并给机器人发送消息建立会话。
长期无人交互的个人微信投递仍受服务端策略约束，建议为必须送达的任务配置独立的通知渠道。

## 回调/Webhook 设置

本通道 **不需要** 配置 Webhook 回调。消息通过 HTTP 长轮询方式获取：

```text
客户端 ──(GET /messages)──→ iLink Bot API
客户端 ←──(JSON response)── iLink Bot API
```

轮询间隔由 API 服务端控制，客户端保持长连接等待新消息。

## 能力矩阵

| 能力 | 支持 | 备注 |
|------|------|------|
| 发送文本 | ✅ | 最大长度 4000 字符 |
| 发送图片 | ✅ | 通过 getuploadurl 上传 |
| 发送语音 | ✅ | SILK 编码格式 |
| 发送文件 | ✅ | 通过 getuploadurl 上传 |
| 编辑消息 | ❌ | 微信不支持 |
| 表情回应 | ❌ | 微信不支持 |
| 群聊 | ❌ | 当前未实现 |
| 实时消息 | ✅ | 长轮询 |
| 输入状态 | ❌ | 当前未实现 |

## 技术细节

### 媒体加密

媒体文件使用 AES-128-ECB 加密传输：

```text
原始文件 → AES-128-ECB 加密 → 上传至 CDN
CDN 下载 → AES-128-ECB 解密 → 原始文件
```

### 语音消息

语音消息使用 SILK 音频编码格式，这是微信原生的音频格式。发送语音时需将音频转换为 SILK 格式。

### 会话过期检测

当 API 返回 `errcode: -14` 时，表示当前会话已过期，需要重新执行 QR 码登录流程。

### 消息去重

使用消息 ID 去重，TTL 为 300 秒（5 分钟），防止长轮询重连时重复处理消息。

## 常见问题

!!! question "Q: 登录后多久需要重新扫码？"
    取决于 iLink Bot 服务端的 session 管理策略。当日志出现 `errcode: -14` 时需重新扫码。建议配置告警监控此错误码。

!!! question "Q: 消息最大长度是多少？"
    单条文本消息最大 4000 字符。超出部分需要分段发送。

!!! question "Q: 如何发送图片/文件？"
    通过 `getuploadurl` 接口获取上传地址，上传文件后获得媒体 ID，再通过发送接口引用该媒体 ID。

!!! question "Q: 为什么收到重复消息？"
    检查消息去重机制是否正常工作。默认 TTL 为 300 秒，如果服务重启导致去重缓存丢失，可能短暂出现重复。

!!! question "Q: 支持群聊吗？"
    当前版本不支持群聊消息的收发。仅处理单聊（私聊）消息。
