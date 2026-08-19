# Telegram

## 概述

Telegram 通道通过 Bot API 的 Long-polling（`getUpdates`）方式连接，无需公网端点或 Webhook 服务器。适合部署在 NAT 后方、无固定 IP 的环境。

支持私聊与群组两种场景，群组内可配置仅在被 @mention 时响应，避免噪声干扰。

!!! tip
    Long-polling 模式下，Bot 主动拉取消息，延迟通常 < 1 秒。对于绝大多数场景，体验与 Webhook 无明显差异。

---

## 配置示例

```yaml
channels:
  - type: telegram
    token: "123456789:ABCdefGHIjklMNOpqrsTUVwxyz"
    allow_from:
      - 100000001        # 用户 ID（数字）
      - 100000002
    proxy: "socks5://127.0.0.1:1080"   # 可选，支持 socks5/http
    group_policy: mention               # open | mention
    reactions_enabled: true
    data_dir: "./data/telegram"         # offset 持久化目录
```

| 字段 | 必填 | 说明 |
|------|------|------|
| `token` | 是 | Bot Token，从 @BotFather 获取 |
| `allow_from` | 否 | 白名单用户/群组 ID，为空则不限制 |
| `proxy` | 否 | SOCKS5 或 HTTP 代理地址 |
| `group_policy` | 否 | 群组响应策略，默认 `mention` |
| `reactions_enabled` | 否 | 是否启用 reaction 回复，默认 `true` |
| `data_dir` | 否 | offset 文件存放目录，默认 `./data/telegram` |

---

## 凭证获取

1. 在 Telegram 中搜索 **@BotFather** 并发起对话。
2. 发送 `/newbot`，按提示输入 Bot 名称和用户名。
3. 创建成功后，BotFather 会返回形如 `123456789:ABCdefGHI...` 的 Token。
4. 将 Token 填入配置的 `token` 字段。

!!! warning
    Token 等同于 Bot 的完整访问权限，请勿提交到公开仓库。建议通过环境变量注入：
    ```yaml
    token: "${TELEGRAM_BOT_TOKEN}"
    ```

如需获取用户 ID 用于 `allow_from`：

- 让用户给 Bot 发一条消息，查看日志中的 `from.id` 字段。
- 或使用 @userinfobot 等第三方工具查询。

---

## 回调/Webhook 设置

本通道使用 **Long-polling** 模式，无需配置 Webhook。

Bot 启动时会自动调用 `deleteWebhook` 清除可能存在的旧 Webhook 设置，确保 `getUpdates` 正常工作。

!!! question "需维护者确认"
    如果之前通过其他框架设置过 Webhook，首次启动时可能需要等待最多 1 分钟才能切换到 polling 模式。

---

## 能力矩阵

| 能力 | 支持 | 说明 |
|------|------|------|
| 编辑消息 | ✅ | 通过 `editMessageText` 实现 |
| Reactions | ✅ | 使用 `setMessageReaction` API |
| 文件发送 | ❌ | 当前未实现 |
| 实时通信 | ✅ | Long-polling，延迟 < 1s |
| 群组支持 | ✅ | 支持群组和超级群组 |
| 消息分块 | ✅ | 超过 4096 字符自动拆分 |

---

## 常见问题

### 代理设置不生效？

确保 `proxy` 字段格式正确。支持的协议：

- `socks5://host:port`
- `socks5://user:pass@host:port`
- `http://host:port`

代理通过 `aiohttp_socks` 实现，需确保该依赖已安装。

### 群组中 Bot 不响应消息？

1. 检查 `group_policy` 设置：
   - `mention`：仅在消息中包含 `@bot_username` 时响应。
   - `open`：响应群组内所有消息。
2. 确认 Bot 的 **Group Privacy** 设置：向 @BotFather 发送 `/setprivacy`，选择 **Disable**，否则 Bot 只能收到 `/command` 和 @mention。

### offset 文件损坏导致重复消息？

offset 使用原子写入（先写临时文件再 rename）到 `data_dir`，正常情况下不会损坏。如果确实出现问题：

1. 停止 Bot。
2. 删除 `data_dir` 下的 offset 文件。
3. 重启 Bot，会从最新消息开始处理（跳过历史）。

### HTML 特殊字符导致发送失败？

Bot 使用 HTML 解析模式发送消息。内容中的 `<`、`>`、`&` 会自动转义。如果你在模板中手动拼接 HTML，需确保用户输入部分已正确转义。

!!! tip
    消息超过 4096 字符时会自动按段落边界分块发送，无需手动处理。
