# Email 通道

## 概述

Email 通道通过 IMAP 轮询接收邮件、SMTP 发送回复，无需公网端点即可运行。适用于需要异步沟通的场景，如客服工单、邮件自动回复等。

由于 Email 天然是异步协议，本通道标记为 `is_realtime=False`——Agent 仅在最终消息生成完毕后一次性发送，不支持流式输出。

## 配置示例

```yaml
channels:
  email:
    enabled: true
    imap_host: imap.gmail.com
    imap_port: 993
    smtp_host: smtp.gmail.com
    smtp_port: 465
    username: bot@example.com
    password: ${EMAIL_APP_PASSWORD}
    use_ssl: true
    poll_interval_seconds: 30
    allow_from:
      - admin@example.com
      - support@example.com
```

| 字段 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `imap_host` | string | — | IMAP 服务器地址 |
| `imap_port` | int | 993 | IMAP 端口（SSL） |
| `smtp_host` | string | — | SMTP 服务器地址 |
| `smtp_port` | int | 465 | SMTP 端口（SSL） |
| `username` | string | — | 登录账号 |
| `password` | string | — | 密码或应用专用密码 |
| `use_ssl` | bool | true | 是否启用 SSL/TLS |
| `poll_interval_seconds` | int | 30 | IMAP 轮询间隔（秒） |
| `allow_from` | list | [] | 发件人白名单，为空则接受所有来源 |

## 凭证获取

### Gmail

1. 前往 [Google 账号安全设置](https://myaccount.google.com/security)
2. 启用两步验证（如未启用）
3. 在「应用专用密码」中生成一个 16 位密码
4. 将该密码填入配置的 `password` 字段

!!! warning "不要使用 Gmail 常规密码"
    Google 已禁止「低安全性应用」直接使用账户密码登录 IMAP/SMTP。必须使用 App Password，否则连接将被拒绝。

### Outlook / Microsoft 365

1. 使用 `outlook.office365.com`（IMAP）和 `smtp.office365.com`（SMTP，端口 587）
2. 如果组织启用了 OAuth2，需要管理员授权 IMAP 访问权限

### QQ 邮箱

1. 登录 QQ 邮箱 → 设置 → 账户 → POP3/IMAP/SMTP 服务
2. 开启 IMAP/SMTP 服务，获取授权码
3. 使用授权码（非 QQ 密码）作为 `password`

!!! tip "QQ 邮箱配置参考"
    - IMAP: `imap.qq.com:993`
    - SMTP: `smtp.qq.com:465`
    - 密码字段填写授权码

## 能力矩阵

| 能力 | 支持 | 说明 |
|------|------|------|
| 编辑已发消息 | ❌ | 邮件协议不支持撤回/编辑 |
| 表情回应 | ❌ | — |
| 文件附件 | ❌ | 当前版本不处理附件 |
| 实时流式输出 | ❌ | 异步投递，仅发送最终结果 |
| 群组/多人会话 | ❌ | 按发件人独立会话 |

## 内部机制

### UID 水位线持久化

通道使用 IMAP UID 作为水位线标记已处理邮件。水位线通过原子写入持久化到磁盘，确保：

- 进程重启后不会重复处理已读邮件
- 异常退出时不会丢失进度（原子写保证一致性）

### 邮件处理流程

1. IMAP 轮询获取 UID > 水位线的新邮件
2. HTML 正文自动转换为纯文本（保留可读结构）
3. 通过 Subject 追踪回复线程（`Re:` 前缀匹配）
4. Agent 生成回复后，通过 SMTP 发送，自动设置 `In-Reply-To` 头

## 常见问题

!!! question "轮询间隔设为多少合适？"
    建议 30-60 秒。过短（<10s）可能触发邮箱提供商的限流；过长会增加用户等待时间。Gmail 的 IMAP IDLE 不在当前支持范围内。

!!! question "allow_from 为空时会怎样？"
    通道将接受所有发件人的邮件。生产环境建议配置白名单以防滥用。

!!! question "HTML 邮件如何处理？"
    入站邮件的 HTML 正文会自动转换为纯文本再传递给 Agent。出站回复以纯文本发送。

!!! warning "附件不会被处理"
    正文提取只取第一个 `text/plain` 部分，没有纯文本时退回 `text/html` 并转换为纯文本。其余 MIME 部分——包括所有附件——一律跳过，Agent 既看不到附件内容，也不会被告知邮件带有附件。

    因此依赖附件的工作流不适用此通道。需要处理文档时，把文件放入知识库目录或工作区，再在邮件正文里说明路径。
