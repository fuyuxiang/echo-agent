"""Echo Agent configuration schema."""

from __future__ import annotations

from typing import Any, Literal

from pydantic import BaseModel, ConfigDict, Field
from pydantic.alias_generators import to_camel


class _Base(BaseModel):
    model_config = ConfigDict(alias_generator=to_camel, populate_by_name=True)


# ── Channel configs ──────────────────────────────────────────────────────────

class TelegramChannelConfig(_Base):
    enabled: bool = Field(
        default=False,
        json_schema_extra={
            "status": "effective", "ref": "channels/manager.py:362",
            "desc_zh": "是否启用 Telegram 通道",
            "desc_en": "Enable the Telegram channel",
        },
    )
    token: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "channels/telegram.py:30",
            "desc_zh": "Telegram Bot API token",
            "desc_en": "Telegram bot API token",
        },
    )
    allow_from: list[str] = Field(
        default_factory=list,
        json_schema_extra={
            "status": "effective", "ref": "channels/base.py:107",
            "desc_zh": "允许与机器人交互的用户白名单(空为不限制)",
            "desc_en": "Allowlist of user IDs permitted to interact (empty = all)",
        },
    )
    proxy: str | None = Field(
        default=None,
        json_schema_extra={
            "status": "effective", "ref": "channels/telegram.py:41",
            "desc_zh": "访问 Telegram API 的代理地址",
            "desc_en": "Proxy URL used to reach the Telegram API",
        },
    )
    group_policy: Literal["open", "mention"] = Field(
        default="mention",
        json_schema_extra={
            "status": "effective", "ref": "channels/telegram.py:35",
            "desc_zh": "群聊响应策略:open 全部响应,mention 仅被@时响应",
            "desc_en": "Group reply policy: open = all messages, mention = only when @-mentioned",
        },
    )
    reactions_enabled: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "channels/telegram.py:109",
            "desc_zh": "是否对消息添加表情回应",
            "desc_en": "Whether to add emoji reactions to messages",
        },
    )


class DiscordChannelConfig(_Base):
    enabled: bool = Field(
        default=False,
        json_schema_extra={
            "status": "effective", "ref": "channels/manager.py:362",
            "desc_zh": "是否启用 Discord 通道",
            "desc_en": "Enable the Discord channel",
        },
    )
    token: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "channels/discord.py:33",
            "desc_zh": "Discord Bot token",
            "desc_en": "Discord bot token",
        },
    )
    allow_from: list[str] = Field(
        default_factory=list,
        json_schema_extra={
            "status": "effective", "ref": "channels/base.py:107",
            "desc_zh": "允许与机器人交互的用户白名单(空为不限制)",
            "desc_en": "Allowlist of user IDs permitted to interact (empty = all)",
        },
    )
    group_policy: Literal["open", "mention"] = Field(
        default="mention",
        json_schema_extra={
            "status": "effective", "ref": "channels/discord.py:34",
            "desc_zh": "群聊响应策略:open 全部响应,mention 仅被@时响应",
            "desc_en": "Group reply policy: open = all messages, mention = only when @-mentioned",
        },
    )
    reactions_enabled: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "channels/discord.py:149",
            "desc_zh": "是否对消息添加表情回应",
            "desc_en": "Whether to add emoji reactions to messages",
        },
    )


class WebhookChannelConfig(_Base):
    enabled: bool = Field(
        default=False,
        json_schema_extra={
            "status": "effective", "ref": "channels/manager.py:362",
            "desc_zh": "是否启用 Webhook 通道",
            "desc_en": "Enable the webhook channel",
        },
    )
    host: str = Field(
        default="0.0.0.0",
        json_schema_extra={
            "status": "effective", "ref": "channels/webhook.py:34",
            "desc_zh": "Webhook 服务监听地址",
            "desc_en": "Webhook server bind address",
        },
    )
    port: int = Field(
        default=8080,
        json_schema_extra={
            "status": "effective", "ref": "channels/webhook.py:34",
            "desc_zh": "Webhook 服务监听端口",
            "desc_en": "Webhook server listen port",
        },
    )
    secret: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "channels/webhook.py:55",
            "desc_zh": "校验入站请求签名的密钥",
            "desc_en": "Secret used to verify inbound request signatures",
        },
    )
    path: str = Field(
        default="/webhook",
        json_schema_extra={
            "status": "effective", "ref": "channels/webhook.py:30",
            "desc_zh": "Webhook 接收路径",
            "desc_en": "HTTP path on which webhooks are received",
        },
    )
    max_pending: int = Field(
        default=1000,
        json_schema_extra={
            "status": "effective", "ref": "channels/webhook.py:93",
            "desc_zh": "待处理 webhook 请求队列上限",
            "desc_en": "Maximum number of pending webhook requests queued",
        },
    )


class CLIChannelConfig(_Base):
    enabled: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "channels/manager.py:362",
            "desc_zh": "是否启用命令行通道",
            "desc_en": "Enable the CLI channel",
        },
    )


class CronChannelConfig(_Base):
    enabled: bool = Field(
        default=False,
        json_schema_extra={
            "status": "effective", "ref": "channels/manager.py:362",
            "desc_zh": "是否启用定时任务通道",
            "desc_en": "Enable the cron channel",
        },
    )


class SlackChannelConfig(_Base):
    enabled: bool = Field(
        default=False,
        json_schema_extra={
            "status": "effective", "ref": "channels/manager.py:362",
            "desc_zh": "是否启用 Slack 通道",
            "desc_en": "Enable the Slack channel",
        },
    )
    bot_token: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "channels/slack.py:29",
            "desc_zh": "Slack bot token(xoxb-)",
            "desc_en": "Slack bot token (xoxb-)",
        },
    )
    app_token: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "channels/slack.py:30",
            "desc_zh": "Slack app-level token(xapp-),用于 Socket 模式",
            "desc_en": "Slack app-level token (xapp-) for Socket Mode",
        },
    )
    allow_from: list[str] = Field(
        default_factory=list,
        json_schema_extra={
            "status": "effective", "ref": "channels/base.py:107",
            "desc_zh": "允许与机器人交互的用户白名单(空为不限制)",
            "desc_en": "Allowlist of user IDs permitted to interact (empty = all)",
        },
    )
    reactions_enabled: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "channels/slack.py:118",
            "desc_zh": "是否对消息添加表情回应",
            "desc_en": "Whether to add emoji reactions to messages",
        },
    )


class WhatsAppChannelConfig(_Base):
    enabled: bool = Field(
        default=False,
        json_schema_extra={
            "status": "effective", "ref": "channels/manager.py:362",
            "desc_zh": "是否启用 WhatsApp 通道",
            "desc_en": "Enable the WhatsApp channel",
        },
    )
    verify_token: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "channels/whatsapp.py:24",
            "desc_zh": "WhatsApp webhook 验证 token",
            "desc_en": "WhatsApp webhook verification token",
        },
    )
    access_token: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "channels/whatsapp.py:25",
            "desc_zh": "WhatsApp Cloud API 访问令牌",
            "desc_en": "WhatsApp Cloud API access token",
        },
    )
    phone_number_id: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "channels/whatsapp.py:26",
            "desc_zh": "WhatsApp 发送号码 ID",
            "desc_en": "WhatsApp phone number ID used for sending",
        },
    )
    webhook_path: str = Field(
        default="/whatsapp",
        json_schema_extra={
            "status": "effective", "ref": "channels/whatsapp.py:35",
            "desc_zh": "WhatsApp webhook 接收路径",
            "desc_en": "HTTP path on which WhatsApp webhooks are received",
        },
    )
    host: str = Field(
        default="0.0.0.0",
        json_schema_extra={
            "status": "effective", "ref": "channels/whatsapp.py:40",
            "desc_zh": "WhatsApp 服务监听地址",
            "desc_en": "WhatsApp server bind address",
        },
    )
    port: int = Field(
        default=8081,
        json_schema_extra={
            "status": "effective", "ref": "channels/whatsapp.py:40",
            "desc_zh": "WhatsApp 服务监听端口",
            "desc_en": "WhatsApp server listen port",
        },
    )


class WeixinChannelConfig(_Base):
    enabled: bool = Field(
        default=False,
        json_schema_extra={
            "status": "effective", "ref": "channels/manager.py:362",
            "desc_zh": "是否启用微信(个人客服)通道",
            "desc_en": "Enable the Weixin channel",
        },
    )
    account_id: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "channels/weixin.py:306",
            "desc_zh": "微信客服账号 ID",
            "desc_en": "Weixin customer-service account ID",
        },
    )
    token: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "channels/weixin.py:307",
            "desc_zh": "微信接入鉴权 token",
            "desc_en": "Weixin access authentication token",
        },
    )
    base_url: str = Field(
        default="https://ilinkai.weixin.qq.com",
        json_schema_extra={
            "status": "effective", "ref": "channels/weixin.py:308",
            "desc_zh": "微信 API 基础地址",
            "desc_en": "Weixin API base URL",
        },
    )
    cdn_base_url: str = Field(
        default="https://novac2c.cdn.weixin.qq.com/c2c",
        json_schema_extra={
            "status": "effective", "ref": "channels/weixin.py:309",
            "desc_zh": "微信媒体 CDN 基础地址",
            "desc_en": "Weixin media CDN base URL",
        },
    )
    allow_from: list[str] = Field(
        default_factory=list,
        json_schema_extra={
            "status": "effective", "ref": "channels/base.py:107",
            "desc_zh": "允许与机器人交互的用户白名单(空为不限制)",
            "desc_en": "Allowlist of user IDs permitted to interact (empty = all)",
        },
    )
    dm_policy: str = Field(
        default="open",
        json_schema_extra={
            "status": "effective", "ref": "channels/weixin.py:310",
            "desc_zh": "私聊响应策略",
            "desc_en": "Direct-message reply policy",
        },
    )
    data_dir: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "channels/weixin.py:311",
            "desc_zh": "微信通道本地数据目录",
            "desc_en": "Local data directory for the Weixin channel",
        },
    )
    typing_indicator: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "channels/weixin.py",
            "desc_zh": "处理消息期间是否向对方下发“对方正在输入”状态",
            "desc_en": "Send a typing indicator to the user while a message is being processed",
        },
    )


class QQBotChannelConfig(_Base):
    enabled: bool = Field(
        default=False,
        json_schema_extra={
            "status": "effective", "ref": "channels/manager.py:362",
            "desc_zh": "是否启用 QQ 机器人通道",
            "desc_en": "Enable the QQ bot channel",
        },
    )
    app_id: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "channels/qqbot.py:85",
            "desc_zh": "QQ 机器人 AppID",
            "desc_en": "QQ bot AppID",
        },
    )
    app_secret: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "channels/qqbot.py:86",
            "desc_zh": "QQ 机器人 AppSecret",
            "desc_en": "QQ bot AppSecret",
        },
    )
    allow_from: list[str] = Field(
        default_factory=list,
        json_schema_extra={
            "status": "effective", "ref": "channels/base.py:107",
            "desc_zh": "允许与机器人交互的用户白名单(空为不限制)",
            "desc_en": "Allowlist of user IDs permitted to interact (empty = all)",
        },
    )
    sandbox: bool = Field(
        default=False,
        json_schema_extra={
            "status": "effective", "ref": "channels/qqbot.py:87",
            "desc_zh": "是否使用 QQ 沙箱环境",
            "desc_en": "Use the QQ sandbox environment",
        },
    )
    markdown_support: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "channels/qqbot.py:88",
            "desc_zh": "是否以 QQ 原生 Markdown(msg_type=2)发送并保留加粗/代码等行内标记。"
                       "默认开启;若机器人未开通原生 Markdown 权限,首条消息会被拒并自动降级为"
                       "纯文本重发,该会话后续对该目标直接走纯文本(24 小时后重新探测)。"
                       "无论开关如何,表格/标题/分隔线都会降级为可读纯文本。关闭则始终发纯文本",
            "desc_en": "Send as QQ native Markdown (msg_type=2) and keep inline markers "
                       "like bold/code. On by default; if the bot lacks native Markdown "
                       "permission, the first message is rejected and auto-retried as plain "
                       "text, and later messages to that target skip markdown (re-probed "
                       "after 24h). Tables/headings/HR are downgraded to readable plain text "
                       "regardless. When off, always sends plain text",
        },
    )
    media_enabled: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "channels/qqbot.py:107",
            "desc_zh": "是否启用媒体(图片/文件)收发",
            "desc_en": "Enable media (image/file) sending and receiving",
        },
    )
    media_max_file_size_mb: int = Field(
        default=20,
        json_schema_extra={
            "status": "effective", "ref": "channels/qqbot.py:109",
            "desc_zh": "媒体上传单文件大小上限(MB)",
            "desc_en": "Maximum size per uploaded media file (MB)",
        },
    )
    media_upload_cache_size: int = Field(
        default=500,
        json_schema_extra={
            "status": "effective", "ref": "channels/qqbot.py:111",
            "desc_zh": "媒体上传结果缓存条目上限",
            "desc_en": "Maximum number of cached media upload results",
        },
    )
    media_parse_tags: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "channels/qqbot.py:108",
            "desc_zh": "是否解析消息中的媒体标签",
            "desc_en": "Parse media tags embedded in messages",
        },
    )


class FeishuChannelConfig(_Base):
    enabled: bool = Field(
        default=False,
        json_schema_extra={
            "status": "effective", "ref": "channels/manager.py:362",
            "desc_zh": "是否启用飞书通道",
            "desc_en": "Enable the Feishu channel",
        },
    )
    app_id: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "channels/feishu.py:27",
            "desc_zh": "飞书应用 App ID",
            "desc_en": "Feishu app ID",
        },
    )
    app_secret: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "channels/feishu.py:28",
            "desc_zh": "飞书应用 App Secret",
            "desc_en": "Feishu app secret",
        },
    )
    verification_token: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "channels/feishu.py:29",
            "desc_zh": "飞书事件回调验证 token",
            "desc_en": "Feishu event callback verification token",
        },
    )
    encryption_key: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "channels/feishu.py:30",
            "desc_zh": "飞书事件加密密钥",
            "desc_en": "Feishu event encryption key",
        },
    )
    webhook_path: str = Field(
        default="/feishu",
        json_schema_extra={
            "status": "effective", "ref": "channels/feishu.py:41",
            "desc_zh": "飞书事件接收路径",
            "desc_en": "HTTP path on which Feishu events are received",
        },
    )
    host: str = Field(
        default="0.0.0.0",
        json_schema_extra={
            "status": "effective", "ref": "channels/feishu.py:45",
            "desc_zh": "飞书服务监听地址",
            "desc_en": "Feishu server bind address",
        },
    )
    port: int = Field(
        default=8083,
        json_schema_extra={
            "status": "effective", "ref": "channels/feishu.py:45",
            "desc_zh": "飞书服务监听端口",
            "desc_en": "Feishu server listen port",
        },
    )


class DingTalkChannelConfig(_Base):
    enabled: bool = Field(
        default=False,
        json_schema_extra={
            "status": "effective", "ref": "channels/manager.py:362",
            "desc_zh": "是否启用钉钉通道",
            "desc_en": "Enable the DingTalk channel",
        },
    )
    app_key: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "channels/dingtalk.py:30",
            "desc_zh": "钉钉应用 AppKey",
            "desc_en": "DingTalk app key",
        },
    )
    app_secret: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "channels/dingtalk.py:31",
            "desc_zh": "钉钉应用 AppSecret",
            "desc_en": "DingTalk app secret",
        },
    )
    robot_code: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "channels/dingtalk.py:32",
            "desc_zh": "钉钉机器人编码",
            "desc_en": "DingTalk robot code",
        },
    )
    allow_from: list[str] = Field(
        default_factory=list,
        json_schema_extra={
            "status": "effective", "ref": "channels/base.py:107",
            "desc_zh": "允许与机器人交互的用户白名单(空为不限制)",
            "desc_en": "Allowlist of user IDs permitted to interact (empty = all)",
        },
    )


class EmailChannelConfig(_Base):
    enabled: bool = Field(
        default=False,
        json_schema_extra={
            "status": "effective", "ref": "channels/manager.py:362",
            "desc_zh": "是否启用邮件通道",
            "desc_en": "Enable the email channel",
        },
    )
    imap_host: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "channels/email.py:100",
            "desc_zh": "收信 IMAP 服务器地址",
            "desc_en": "IMAP server host for receiving mail",
        },
    )
    imap_port: int = Field(
        default=993,
        json_schema_extra={
            "status": "effective", "ref": "channels/email.py:100",
            "desc_zh": "收信 IMAP 服务器端口",
            "desc_en": "IMAP server port for receiving mail",
        },
    )
    smtp_host: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "channels/email.py:68",
            "desc_zh": "发信 SMTP 服务器地址",
            "desc_en": "SMTP server host for sending mail",
        },
    )
    smtp_port: int = Field(
        default=465,
        json_schema_extra={
            "status": "effective", "ref": "channels/email.py:68",
            "desc_zh": "发信 SMTP 服务器端口",
            "desc_en": "SMTP server port for sending mail",
        },
    )
    username: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "channels/email.py:64",
            "desc_zh": "邮箱登录用户名",
            "desc_en": "Mailbox login username",
        },
    )
    password: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "channels/email.py:69",
            "desc_zh": "邮箱登录密码或授权码",
            "desc_en": "Mailbox login password or app token",
        },
    )
    use_ssl: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "channels/email.py:67",
            "desc_zh": "是否使用 SSL 连接邮件服务器",
            "desc_en": "Use SSL when connecting to mail servers",
        },
    )
    poll_interval_seconds: int = Field(
        default=30,
        json_schema_extra={
            "status": "effective", "ref": "channels/email.py:94",
            "desc_zh": "轮询新邮件的间隔(秒)",
            "desc_en": "Interval between new-mail polls (seconds)",
        },
    )
    allow_from: list[str] = Field(
        default_factory=list,
        json_schema_extra={
            "status": "effective", "ref": "channels/email.py:118",
            "desc_zh": "允许交互的发件人邮箱白名单(空为不限制)",
            "desc_en": "Allowlist of sender addresses permitted to interact (empty = all)",
        },
    )


class WeComChannelConfig(_Base):
    enabled: bool = Field(
        default=False,
        json_schema_extra={
            "status": "effective", "ref": "channels/manager.py:362",
            "desc_zh": "是否启用企业微信通道",
            "desc_en": "Enable the WeCom channel",
        },
    )
    corp_id: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "channels/wecom.py:26",
            "desc_zh": "企业微信企业 ID",
            "desc_en": "WeCom corporation ID",
        },
    )
    agent_id: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "channels/wecom.py:27",
            "desc_zh": "企业微信应用 AgentId",
            "desc_en": "WeCom application AgentId",
        },
    )
    secret: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "channels/wecom.py:28",
            "desc_zh": "企业微信应用 Secret",
            "desc_en": "WeCom application secret",
        },
    )
    token: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "channels/wecom.py:29",
            "desc_zh": "企业微信回调校验 token",
            "desc_en": "WeCom callback verification token",
        },
    )
    encoding_aes_key: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "channels/wecom.py:87",
            "desc_zh": "企业微信加密回调的 EncodingAESKey,留空则为明文模式",
            "desc_en": "EncodingAESKey for WeCom encrypted callbacks; empty means plaintext mode",
        },
    )
    webhook_path: str = Field(
        default="/wecom",
        json_schema_extra={
            "status": "effective", "ref": "channels/wecom.py:39",
            "desc_zh": "企业微信事件接收路径",
            "desc_en": "HTTP path on which WeCom events are received",
        },
    )
    host: str = Field(
        default="0.0.0.0",
        json_schema_extra={
            "status": "effective", "ref": "channels/wecom.py:44",
            "desc_zh": "企业微信服务监听地址",
            "desc_en": "WeCom server bind address",
        },
    )
    port: int = Field(
        default=8084,
        json_schema_extra={
            "status": "effective", "ref": "channels/wecom.py:44",
            "desc_zh": "企业微信服务监听端口",
            "desc_en": "WeCom server listen port",
        },
    )


class MatrixChannelConfig(_Base):
    enabled: bool = Field(
        default=False,
        json_schema_extra={
            "status": "effective", "ref": "channels/manager.py:362",
            "desc_zh": "是否启用 Matrix 通道",
            "desc_en": "Enable the Matrix channel",
        },
    )
    homeserver: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "channels/matrix.py:27",
            "desc_zh": "Matrix homeserver 地址",
            "desc_en": "Matrix homeserver URL",
        },
    )
    user_id: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "channels/matrix.py:28",
            "desc_zh": "Matrix 机器人用户 ID",
            "desc_en": "Matrix bot user ID",
        },
    )
    access_token: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "channels/matrix.py:38",
            "desc_zh": "Matrix 访问令牌",
            "desc_en": "Matrix access token",
        },
    )
    allow_rooms: list[str] = Field(
        default_factory=list,
        json_schema_extra={
            "status": "effective", "ref": "channels/matrix.py:30",
            "desc_zh": "允许响应的房间白名单(空为不限制)",
            "desc_en": "Allowlist of room IDs the bot responds in (empty = all)",
        },
    )
    reactions_enabled: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "channels/matrix.py:100",
            "desc_zh": "是否对消息添加表情回应",
            "desc_en": "Whether to add emoji reactions to messages",
        },
    )


class ChannelsConfig(_Base):
    telegram: TelegramChannelConfig = Field(default_factory=TelegramChannelConfig)
    discord: DiscordChannelConfig = Field(default_factory=DiscordChannelConfig)
    webhook: WebhookChannelConfig = Field(default_factory=WebhookChannelConfig)
    cli: CLIChannelConfig = Field(default_factory=CLIChannelConfig)
    cron: CronChannelConfig = Field(default_factory=CronChannelConfig)
    slack: SlackChannelConfig = Field(default_factory=SlackChannelConfig)
    whatsapp: WhatsAppChannelConfig = Field(default_factory=WhatsAppChannelConfig)
    weixin: WeixinChannelConfig = Field(default_factory=WeixinChannelConfig)
    qqbot: QQBotChannelConfig = Field(default_factory=QQBotChannelConfig)
    feishu: FeishuChannelConfig = Field(default_factory=FeishuChannelConfig)
    dingtalk: DingTalkChannelConfig = Field(default_factory=DingTalkChannelConfig)
    email: EmailChannelConfig = Field(default_factory=EmailChannelConfig)
    wecom: WeComChannelConfig = Field(default_factory=WeComChannelConfig)
    matrix: MatrixChannelConfig = Field(default_factory=MatrixChannelConfig)
    send_progress: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "channels/manager.py:88",
            "desc_zh": "是否向用户推送处理进度提示",
            "desc_en": "Send progress updates to the user",
        },
    )
    send_tool_hints: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "channels/manager.py:89",
            "desc_zh": "是否推送工具调用提示",
            "desc_en": "Send tool-invocation hints to the user",
        },
    )
    stream_channels: list[str] = Field(
        default_factory=lambda: ["cli", "telegram", "discord", "slack", "gateway:*"],
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:815",
            "desc_zh": "启用流式增量回复的通道列表",
            "desc_en": "Channels for which streaming incremental replies are enabled",
        },
    )
    stream_flush_chars: int = Field(
        default=180,
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:683",
            "desc_zh": "流式回复累计多少字符后推送一段",
            "desc_en": "Character count that triggers a streaming flush",
        },
    )
    stream_flush_interval_ms: int = Field(
        default=1500,
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:684",
            "desc_zh": "流式回复推送的最大时间间隔(毫秒)",
            "desc_en": "Maximum interval between streaming flushes (ms)",
        },
    )
    stream_paragraph_mode: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:685",
            "desc_zh": "是否按段落边界切分流式推送",
            "desc_en": "Flush streaming output on paragraph boundaries",
        },
    )
    transcription_api_key: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "channels/manager.py:369",
            "desc_zh": "语音转写服务的 API key",
            "desc_en": "API key for the voice transcription service",
        },
    )


# ── Provider configs ─────────────────────────────────────────────────────────

class ProviderConfig(_Base):
    name: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "app.py:91",
            "desc_zh": "提供商名称(路由引用此名)",
            "desc_en": "Provider name referenced by routes",
        },
    )
    api_key: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "models/providers/__init__.py:123",
            "desc_zh": "提供商 API 密钥",
            "desc_en": "Provider API key",
        },
    )
    api_base: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "models/providers/__init__.py:125",
            "desc_zh": "提供商 API 基础地址",
            "desc_en": "Provider API base URL",
        },
    )
    models: list[str] = Field(
        default_factory=list,
        json_schema_extra={
            "status": "effective", "ref": "models/router.py:352",
            "desc_zh": "该提供商支持的模型列表",
            "desc_en": "Models served by this provider",
        },
    )
    extra_headers: dict[str, str] = Field(
        default_factory=dict,
        json_schema_extra={
            "status": "effective", "ref": "models/providers/__init__.py:112",
            "desc_zh": "附加到请求的自定义 HTTP 头",
            "desc_en": "Extra HTTP headers attached to requests",
        },
    )
    max_retries: int = Field(
        default=3,
        json_schema_extra={
            "status": "effective", "ref": "models/providers/__init__.py:125",
            "desc_zh": "瞬时错误时的最大重试次数(指数退避)",
            "desc_en": "Max retries on transient errors (exponential backoff)",
        },
    )
    timeout_seconds: int = Field(
        default=120,
        json_schema_extra={
            "status": "effective", "ref": "models/providers/__init__.py:126",
            "desc_zh": "单次请求超时(秒)",
            "desc_en": "Per-request timeout (seconds)",
        },
    )
    rate_limit_rpm: int = Field(
        default=0,
        json_schema_extra={
            "status": "effective", "ref": "models/providers/__init__.py:131",
            "desc_zh": "该提供商每分钟请求上限(0 为不限)",
            "desc_en": "Provider request-per-minute cap (0 = unlimited)",
        },
    )
    credential_pool: list[str] = Field(
        default_factory=list,
        json_schema_extra={
            "status": "effective", "ref": "models/providers/__init__.py:119",
            "desc_zh": "轮换使用的多个 API 密钥池",
            "desc_en": "Pool of API keys rotated for this provider",
        },
    )


class ModelRouteConfig(_Base):
    model: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "models/router.py:122",
            "desc_zh": "该路由使用的模型名",
            "desc_en": "Model name used by this route",
        },
    )
    provider: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "models/router.py:296",
            "desc_zh": "该路由绑定的提供商名",
            "desc_en": "Provider name bound to this route",
        },
    )
    task_types: list[str] = Field(
        default_factory=list,
        json_schema_extra={
            "status": "effective", "ref": "models/router.py:294",
            "desc_zh": "命中此路由的任务类型列表",
            "desc_en": "Task types that match this route",
        },
    )
    fallback_models: list[str] = Field(
        default_factory=list,
        json_schema_extra={
            "status": "effective", "ref": "models/router.py:284",
            "desc_zh": "主模型失败时的回退模型列表",
            "desc_en": "Fallback models when the primary fails",
        },
    )
    max_tokens: int = Field(
        default=4096,
        json_schema_extra={
            "status": "effective", "ref": "agent/pipeline/inference_stage.py:615",
            "desc_zh": "该路由生成的最大 token 数",
            "desc_en": "Maximum tokens generated for this route",
        },
    )
    temperature: float = Field(
        default=0.7,
        json_schema_extra={
            "status": "effective", "ref": "agent/pipeline/inference_stage.py:616",
            "desc_zh": "该路由的采样温度",
            "desc_en": "Sampling temperature for this route",
        },
    )


class ModelsConfig(_Base):
    default_model: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "models/router.py:130",
            "desc_zh": "无匹配路由时使用的默认模型",
            "desc_en": "Default model used when no route matches",
        },
    )
    providers: list[ProviderConfig] = Field(
        default_factory=list,
        json_schema_extra={
            "status": "effective", "ref": "app.py:88",
            "desc_zh": "模型提供商配置列表",
            "desc_en": "List of model provider configurations",
        },
    )
    routes: list[ModelRouteConfig] = Field(
        default_factory=list,
        json_schema_extra={
            "status": "effective", "ref": "models/router.py:121",
            "desc_zh": "任务到模型的路由规则列表",
            "desc_en": "List of task-to-model routing rules",
        },
    )
    fallback_model: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "models/router.py:147",
            "desc_zh": "全局兜底模型",
            "desc_en": "Global fallback model",
        },
    )


# ── Tool configs ─────────────────────────────────────────────────────────────

class ExecToolConfig(_Base):
    enabled: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "agent/tools/__init__.py:39",
            "desc_zh": "是否启用 shell/进程执行工具",
            "desc_en": "Enable the shell/process execution tool",
        },
    )
    max_output_chars: int = Field(
        default=16000,
        json_schema_extra={
            "status": "effective", "ref": "agent/tools/__init__.py:47",
            "desc_zh": "命令输出截断的最大字符数",
            "desc_en": "Maximum characters of command output before truncation",
        },
    )
    host: Literal["auto", "local", "sandbox", "container", "remote"] = Field(
        default="sandbox",
        json_schema_extra={
            "status": "effective", "ref": "agent/tools/__init__.py:42",
            "desc_zh": "命令执行所在的宿主环境",
            "desc_en": "Host environment in which commands execute",
        },
    )
    security: Literal["deny", "allowlist", "full"] = Field(
        default="allowlist",
        json_schema_extra={
            "status": "effective", "ref": "security/guards.py:237",
            "desc_zh": "命令执行安全模式",
            "desc_en": "Command execution security mode",
        },
    )
    ask: Literal["off", "on_miss", "always"] = Field(
        default="on_miss",
        json_schema_extra={
            "status": "effective", "ref": "security/guards.py:238",
            "desc_zh": "命令执行前的审批询问策略",
            "desc_en": "When to ask for approval before running a command",
        },
    )
    safe_bins: list[str] = Field(
        default_factory=lambda: [
            "awk",
            "cat",
            "date",
            "echo",
            "find",
            "grep",
            "head",
            "ls",
            "pwd",
            "rg",
            "sed",
            "sort",
            "tail",
            "tr",
            "uniq",
            "wc",
        ],
        json_schema_extra={
            "status": "effective", "ref": "security/guards.py:265",
            "desc_zh": "allowlist 模式下免审批直接放行的安全命令",
            "desc_en": "Commands allowed without approval under allowlist mode",
        },
    )
    allowed_commands: list[str] = Field(
        default_factory=list,
        json_schema_extra={
            "status": "effective", "ref": "agent/tools/__init__.py:45",
            "desc_zh": "额外允许执行的命令白名单",
            "desc_en": "Additional allowlist of commands permitted to run",
        },
    )
    blocked_commands: list[str] = Field(
        default_factory=list,
        json_schema_extra={
            "status": "effective", "ref": "agent/tools/__init__.py:46",
            "desc_zh": "禁止执行的命令黑名单",
            "desc_en": "Blocklist of commands forbidden from running",
        },
    )


class WebToolConfig(_Base):
    enabled: bool = Field(
        default=False,
        json_schema_extra={
            "status": "effective", "ref": "agent/tools/__init__.py:56",
            "desc_zh": "是否启用网络访问工具",
            "desc_en": "Enable the web access tool",
        },
    )
    proxy: str | None = Field(
        default=None,
        json_schema_extra={
            "status": "effective", "ref": "agent/tools/__init__.py:57",
            "desc_zh": "网络访问使用的代理地址",
            "desc_en": "Proxy URL used for web access",
        },
    )
    timeout_seconds: int = Field(
        default=30,
        json_schema_extra={
            "status": "effective", "ref": "agent/tools/__init__.py:64",
            "desc_zh": "网络请求超时(秒)",
            "desc_en": "Web request timeout (seconds)",
        },
    )
    search_api_key: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "agent/tools/__init__.py:58",
            "desc_zh": "搜索服务 API key",
            "desc_en": "Search service API key",
        },
    )
    search_provider: Literal["brave", "tavily", "serpapi", "searxng"] = Field(
        default="brave",
        json_schema_extra={
            "status": "effective", "ref": "agent/tools/__init__.py:58",
            "desc_zh": "网络搜索服务提供商",
            "desc_en": "Web search service provider",
        },
    )
    search_api_base: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "agent/tools/__init__.py:62",
            "desc_zh": "搜索服务 API 基础地址",
            "desc_en": "Search service API base URL",
        },
    )
    # SSRF guard: block web_fetch requests to loopback/private/link-local
    # addresses (cloud metadata endpoints, internal services). Opt out only
    # when the agent legitimately needs to reach internal hosts.
    allow_private_addresses: bool = Field(
        default=False,
        json_schema_extra={
            "status": "effective", "ref": "agent/tools/__init__.py:57",
            "desc_zh": "是否允许 web_fetch 访问私有/回环地址(SSRF 风险)",
            "desc_en": "Allow web_fetch to reach private/loopback addresses (SSRF risk)",
        },
    )


class BrowserToolConfig(_Base):
    enabled: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "agent/tools/__init__.py",
            "desc_zh": "是否开启浏览器自动化工具（默认开，未装 playwright/chromium 时 is_ready 探测自动降级不装配）",
            "desc_en": "Enable browser automation tool (default on; auto-degrades if playwright/chromium missing)",
        },
    )
    max_sessions: int = Field(
        default=3,
        json_schema_extra={
            "status": "effective", "ref": "agent/browser/session.py",
            "desc_zh": "并发浏览器会话上限",
            "desc_en": "Max concurrent browser sessions",
        },
    )
    session_idle_timeout_sec: int = Field(
        default=300,
        json_schema_extra={
            "status": "effective", "ref": "agent/browser/session.py",
            "desc_zh": "浏览器会话空闲多久(秒)后自动回收",
            "desc_en": "Idle seconds before a browser session is reaped",
        },
    )
    max_snapshot_chars: int = Field(
        default=8000,
        json_schema_extra={
            "status": "effective", "ref": "agent/browser/snapshot.py",
            "desc_zh": "可访问性快照文本截断上限(字符)",
            "desc_en": "Accessibility snapshot text truncation limit (chars)",
        },
    )
    headless: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "agent/browser/session.py",
            "desc_zh": "无头模式（服务器环境必需）",
            "desc_en": "Headless mode (required on servers)",
        },
    )
    nav_timeout_sec: int = Field(
        default=30,
        json_schema_extra={
            "status": "effective", "ref": "agent/browser/actions.py",
            "desc_zh": "单次页面导航超时(秒)",
            "desc_en": "Per-navigation timeout (seconds)",
        },
    )
    allow_private_addresses: bool = Field(
        default=False,
        json_schema_extra={
            "status": "effective", "ref": "agent/browser/actions.py",
            "desc_zh": "是否允许导航到内网地址（默认拦截，复用 SSRF 口径）",
            "desc_en": "Allow navigating to private addresses (default blocked, reuses SSRF policy)",
        },
    )


class ImageGenConfig(_Base):
    backend: str = Field(
        default="openai",
        json_schema_extra={
            "status": "effective", "ref": "agent/tools/__init__.py:192",
            "desc_zh": "图像生成后端",
            "desc_en": "Image generation backend",
        },
    )
    # OpenAI-compatible backend
    api_key: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "agent/tools/__init__.py:213",
            "desc_zh": "OpenAI 兼容后端 API key",
            "desc_en": "OpenAI-compatible backend API key",
        },
    )
    api_base: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "agent/tools/__init__.py:214",
            "desc_zh": "OpenAI 兼容后端 API 基础地址",
            "desc_en": "OpenAI-compatible backend API base URL",
        },
    )
    model: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "agent/tools/__init__.py:215",
            "desc_zh": "图像生成模型名",
            "desc_en": "Image generation model name",
        },
    )
    # FAL.ai backend
    fal_key: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "agent/tools/__init__.py:195",
            "desc_zh": "FAL.ai 后端访问密钥",
            "desc_en": "FAL.ai backend access key",
        },
    )
    fal_model: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "agent/tools/__init__.py:196",
            "desc_zh": "FAL.ai 图像生成模型名",
            "desc_en": "FAL.ai image generation model name",
        },
    )


class TTSConfig(_Base):
    openai_api_key: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "agent/tools/__init__.py:236",
            "desc_zh": "OpenAI TTS API key",
            "desc_en": "OpenAI TTS API key",
        },
    )
    openai_api_base: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "agent/tools/__init__.py:237",
            "desc_zh": "OpenAI TTS API 基础地址",
            "desc_en": "OpenAI TTS API base URL",
        },
    )
    model: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "agent/tools/__init__.py:238",
            "desc_zh": "TTS 模型名",
            "desc_en": "TTS model name",
        },
    )
    default_backend: str = Field(
        default="edge",
        json_schema_extra={
            "status": "effective", "ref": "agent/tools/__init__.py:239",
            "desc_zh": "默认语音合成后端",
            "desc_en": "Default text-to-speech backend",
        },
    )
    default_voice: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "agent/tools/__init__.py:240",
            "desc_zh": "默认语音音色",
            "desc_en": "Default synthesis voice",
        },
    )


class CodeExecConfig(_Base):
    enabled: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "agent/tools/__init__.py:91",
            "desc_zh": "是否启用代码执行工具",
            "desc_en": "Enable the code execution tool",
        },
    )
    timeout_seconds: int = Field(
        default=30,
        json_schema_extra={
            "status": "effective", "ref": "agent/tools/__init__.py:98",
            "desc_zh": "代码执行超时(秒)",
            "desc_en": "Code execution timeout (seconds)",
        },
    )
    allowed_languages: list[str] = Field(
        default_factory=lambda: ["python", "javascript", "bash"],
        json_schema_extra={
            "status": "effective", "ref": "agent/tools/__init__.py:96",
            "desc_zh": "允许执行的代码语言列表",
            "desc_en": "Languages permitted for code execution",
        },
    )


class MCPServerConfig(_Base):
    command: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "mcp/manager.py:117",
            "desc_zh": "stdio 传输方式下启动 MCP 服务的命令",
            "desc_en": "Command launching the MCP server over stdio",
        },
    )
    args: list[str] = Field(
        default_factory=list,
        json_schema_extra={
            "status": "effective", "ref": "mcp/manager.py:119",
            "desc_zh": "启动 MCP 服务命令的参数",
            "desc_en": "Arguments for the MCP server launch command",
        },
    )
    env: dict[str, str] = Field(
        default_factory=dict,
        json_schema_extra={
            "status": "effective", "ref": "mcp/manager.py:118",
            "desc_zh": "MCP 服务进程的环境变量",
            "desc_en": "Environment variables for the MCP server process",
        },
    )
    url: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "mcp/manager.py:108",
            "desc_zh": "HTTP 传输方式下 MCP 服务地址",
            "desc_en": "MCP server URL for HTTP transport",
        },
    )
    headers: dict[str, str] = Field(
        default_factory=dict,
        json_schema_extra={
            "status": "effective", "ref": "mcp/manager.py:109",
            "desc_zh": "HTTP 连接 MCP 服务的自定义头",
            "desc_en": "Custom headers for the MCP HTTP connection",
        },
    )
    auth: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "mcp/manager.py:110",
            "desc_zh": "MCP 服务的认证凭据",
            "desc_en": "Authentication credential for the MCP server",
        },
    )
    enabled: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "mcp/manager.py:37",
            "desc_zh": "是否启用该 MCP 服务",
            "desc_en": "Enable this MCP server",
        },
    )
    timeout: int = Field(
        default=120,
        json_schema_extra={
            "status": "effective", "ref": "mcp/manager.py:144",
            "desc_zh": "MCP 调用超时(秒)",
            "desc_en": "MCP call timeout (seconds)",
        },
    )
    connect_timeout: int = Field(
        default=60,
        json_schema_extra={
            "status": "effective", "ref": "mcp/manager.py:95",
            "desc_zh": "连接 MCP 服务的超时(秒)",
            "desc_en": "MCP server connection timeout (seconds)",
        },
    )
    tools_include: list[str] = Field(
        default_factory=list,
        json_schema_extra={
            "status": "effective", "ref": "mcp/manager.py:128",
            "desc_zh": "仅暴露的 MCP 工具白名单(空为全部)",
            "desc_en": "Allowlist of MCP tools to expose (empty = all)",
        },
    )
    tools_exclude: list[str] = Field(
        default_factory=list,
        json_schema_extra={
            "status": "effective", "ref": "mcp/manager.py:129",
            "desc_zh": "排除的 MCP 工具黑名单",
            "desc_en": "Blocklist of MCP tools to exclude",
        },
    )


class ToolsConfig(_Base):
    # Keep in sync with default.yaml (tools.profile). "full" is the packaged
    # default; both must agree so the effective profile is unambiguous.
    profile: Literal["minimal", "messaging", "coding", "full"] = Field(
        default="full",
        json_schema_extra={
            "status": "effective", "ref": "security/tool_policy.py:114",
            "desc_zh": "工具集预设档位",
            "desc_en": "Preset tool profile",
        },
    )
    allow: list[str] = Field(
        default_factory=list,
        json_schema_extra={
            "status": "effective", "ref": "security/tool_policy.py:119",
            "desc_zh": "覆盖档位、显式允许的工具列表",
            "desc_en": "Explicit allowlist of tools overriding the profile",
        },
    )
    also_allow: list[str] = Field(
        default_factory=list,
        json_schema_extra={
            "status": "effective", "ref": "security/tool_policy.py:122",
            "desc_zh": "在档位基础上额外允许的工具",
            "desc_en": "Tools additionally allowed on top of the profile",
        },
    )
    deny: list[str] = Field(
        default_factory=list,
        json_schema_extra={
            "status": "effective", "ref": "security/tool_policy.py:111",
            "desc_zh": "显式禁用的工具列表",
            "desc_en": "Explicit blocklist of tools",
        },
    )
    exec: ExecToolConfig = Field(default_factory=ExecToolConfig)
    web: WebToolConfig = Field(default_factory=WebToolConfig)
    browser: BrowserToolConfig = Field(default_factory=BrowserToolConfig)
    restrict_to_workspace: bool = Field(
        default=False,
        json_schema_extra={
            "status": "effective", "ref": "agent/tools/__init__.py:34",
            "desc_zh": "是否将文件操作限制在工作区内",
            "desc_en": "Restrict file operations to the workspace",
        },
    )
    safe_write_root: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "agent/tools/__init__.py:35",
            "desc_zh": "允许写入的根目录",
            "desc_en": "Root directory under which writes are permitted",
        },
    )
    inbound_document_enabled: bool = Field(
        True,
        json_schema_extra={
            "status": "effective",
            "ref": "agent/context.py:resolve_inbound_media",
            "desc_zh": "是否自动下载、解密并解析入站文档附件(docx/xlsx/pptx/pdf)",
            "desc_en": "Auto download, decrypt and parse inbound document attachments",
        },
    )
    inbound_document_max_chars: int = Field(
        8000,
        ge=0,
        json_schema_extra={
            "status": "effective",
            "ref": "agent/context.py:build_messages",
            "desc_zh": "入站文档自动注入正文的字符上限,超出则注入摘要并提示用 read_document 读全文",
            "desc_en": "Char cap for auto-injecting inbound document text; beyond it, inject a summary and hint read_document",
        },
    )
    mcp_servers: dict[str, MCPServerConfig] = Field(
        default_factory=dict,
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:552",
            "desc_zh": "MCP 服务配置(键为服务名)",
            "desc_en": "MCP server configurations keyed by name",
        },
    )
    mcp_security_policy: Literal["warn", "block"] = Field(
        default="block",
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:558",
            "desc_zh": "MCP 工具安全策略:warn 仅告警,block 拦截",
            "desc_en": "MCP tool security policy: warn or block",
        },
    )
    image_gen: ImageGenConfig = Field(default_factory=ImageGenConfig)
    tts: TTSConfig = Field(default_factory=TTSConfig)
    code_exec: CodeExecConfig = Field(default_factory=CodeExecConfig)


# ── Execution environment configs ────────────────────────────────────────────

class ExecutionConfig(_Base):
    default_executor: Literal["local", "sandbox", "container", "remote"] = Field(
        default="sandbox",
        json_schema_extra={
            "status": "effective", "ref": "agent/executors/factory.py:18",
            "desc_zh": "默认命令执行器类型",
            "desc_en": "Default command executor type",
        },
    )
    sandbox_root: str = Field(
        default="/tmp/echo-agent-sandbox",
        json_schema_extra={
            "status": "effective", "ref": "agent/executors/factory.py:22",
            "desc_zh": "sandbox 执行器的根目录",
            "desc_en": "Root directory for the sandbox executor",
        },
    )
    container_image: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "agent/executors/factory.py:24",
            "desc_zh": "container 执行器使用的镜像",
            "desc_en": "Image used by the container executor",
        },
    )
    remote_host: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "agent/executors/factory.py:27",
            "desc_zh": "remote 执行器的目标主机",
            "desc_en": "Target host for the remote executor",
        },
    )
    remote_user: str = Field(
        default="root",
        json_schema_extra={
            "status": "effective", "ref": "agent/executors/factory.py:28",
            "desc_zh": "remote 执行器登录用户名",
            "desc_en": "Login user for the remote executor",
        },
    )
    remote_key_path: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "agent/executors/factory.py:29",
            "desc_zh": "remote 执行器 SSH 私钥路径",
            "desc_en": "SSH private key path for the remote executor",
        },
    )
    remote_strict_host_key: Literal["no", "accept-new", "yes"] = Field(
        default="accept-new",
        json_schema_extra={
            "status": "effective", "ref": "agent/executors/factory.py:30",
            "desc_zh": "SSH 主机密钥严格校验策略",
            "desc_en": "SSH strict host key checking policy",
        },
    )
    remote_connect_timeout: int = Field(
        default=10,
        json_schema_extra={
            "status": "effective", "ref": "agent/executors/factory.py:31",
            "desc_zh": "remote 执行器连接超时(秒)",
            "desc_en": "Remote executor connection timeout (seconds)",
        },
    )
    network_policy: Literal["allow", "deny", "restricted"] = Field(
        default="deny",
        json_schema_extra={
            "status": "effective", "ref": "security/tool_policy.py:141",
            "desc_zh": "执行环境的网络访问策略",
            "desc_en": "Network access policy for the execution environment",
        },
    )
    max_background_tasks: int = Field(
        default=64,
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:259",
            "desc_zh": "后台任务并发上限,超限时可丢弃任务被丢、不可丢任务排队",
            "desc_en": "Max concurrent background tasks; over limit discardable dropped, durable queued",
        },
    )


# ── Permission configs ───────────────────────────────────────────────────────

class ApprovalConfig(_Base):
    require_approval: list[str] = Field(
        default_factory=lambda: [
            "cronjob",
            "dep_install",
            "exec",
            "execute_code",
            "process",
            "skill_install",
            "skill_manage",
        ],
        json_schema_extra={
            "status": "effective", "ref": "agent/approval_gate.py:291",
            "desc_zh": "执行前必须审批的工具/动作列表",
            "desc_en": "Tools/actions that require approval before running",
        },
    )
    auto_approve: list[str] = Field(
        default_factory=list,
        json_schema_extra={
            "status": "effective", "ref": "agent/approval_gate.py:116",
            "desc_zh": "自动批准的工具/动作列表",
            "desc_en": "Tools/actions auto-approved without prompting",
        },
    )
    auto_deny: list[str] = Field(
        default_factory=list,
        json_schema_extra={
            "status": "effective", "ref": "agent/approval_gate.py:285",
            "desc_zh": "自动拒绝的工具/动作列表",
            "desc_en": "Tools/actions auto-denied",
        },
    )
    default_policy: Literal["approve", "deny", "ask"] = Field(
        default="approve",
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:143",
            "desc_zh": "未命中规则时的默认审批策略",
            "desc_en": "Default approval policy when no rule matches",
        },
    )
    wait_timeout_seconds: int = Field(
        default=300,
        json_schema_extra={
            "status": "effective", "ref": "agent/approval_gate.py:226",
            "desc_zh": "等待人工审批的超时(秒)",
            "desc_en": "Timeout while waiting for human approval (seconds)",
        },
    )
    cli_auto_approve: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "agent/approval_gate.py:301",
            "desc_zh": "CLI 通道是否自动批准",
            "desc_en": "Auto-approve actions on the CLI channel",
        },
    )
    trusted_channels: list[str] = Field(
        default_factory=list,
        json_schema_extra={
            "status": "effective", "ref": "agent/approval_gate.py:306",
            "desc_zh": "免审批的可信通道列表",
            "desc_en": "Trusted channels exempt from approval",
        },
    )
    mode: Literal["manual", "smart", "off"] = Field(
        default="smart",
        json_schema_extra={
            "status": "effective", "ref": "agent/approval_gate.py:128",
            "desc_zh": "审批模式:manual 全人工,smart 智能判定,off 关闭",
            "desc_en": "Approval mode: manual, smart, or off",
        },
    )
    smart_model: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "agent/approval_gate.py:184",
            "desc_zh": "smart 模式判定审批所用模型",
            "desc_en": "Model used to judge approvals in smart mode",
        },
    )
    unattended_policy: Literal["deny", "allow_safe"] = Field(
        default="deny",
        json_schema_extra={
            "status": "effective", "ref": "agent/approval_gate.py:316",
            "desc_zh": "无人值守时的审批策略",
            "desc_en": "Approval policy when running unattended",
        },
    )


class ElevatedConfig(_Base):
    enabled: bool = Field(
        default=False,
        json_schema_extra={
            "status": "effective", "ref": "agent/approval_gate.py:357",
            "desc_zh": "是否启用提权操作机制",
            "desc_en": "Enable the elevated-permission mechanism",
        },
    )
    allow_from: dict[str, list[str]] = Field(
        default_factory=dict,
        json_schema_extra={
            "status": "effective", "ref": "agent/approval_gate.py:359",
            "desc_zh": "各通道允许提权的用户映射",
            "desc_en": "Per-channel mapping of users allowed to elevate",
        },
    )


class PermissionsConfig(_Base):
    admin_users: list[str] = Field(
        default_factory=list,
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:783",
            "desc_zh": "全局管理员用户列表",
            "desc_en": "Global administrator users",
        },
    )
    approval: ApprovalConfig = Field(default_factory=ApprovalConfig)
    elevated: ElevatedConfig = Field(default_factory=ElevatedConfig)


class SecurityConfig(_Base):
    profile: Literal["personal_cli", "daemon", "public_gateway"] = Field(
        default="personal_cli",
        json_schema_extra={
            "status": "effective", "ref": "security/tool_policy.py:127",
            "desc_zh": "整体安全档位预设",
            "desc_en": "Overall security profile preset",
        },
    )


class CredentialSecurityConfig(_Base):
    encryption_key_env: str = Field(
        default="ECHO_AGENT_CREDENTIAL_KEY",
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:167",
            "desc_zh": "存放凭据加密密钥的环境变量名",
            "desc_en": "Environment variable holding the credential encryption key",
        },
    )
    require_encryption: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:168",
            "desc_zh": "是否强制要求凭据加密",
            "desc_en": "Require credential encryption",
        },
    )


# ── Session configs ──────────────────────────────────────────────────────────

class SessionConfig(_Base):
    max_history_messages: int = Field(
        default=500,
        json_schema_extra={
            "status": "effective", "ref": "agent/pipeline/context_stage.py:133",
            "desc_zh": "单会话保留的最大历史消息数",
            "desc_en": "Maximum history messages retained per session",
        },
    )
    expiry_hours: int = Field(
        default=72,
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:99",
            "desc_zh": "会话过期时间(小时)",
            "desc_en": "Session expiry time (hours)",
        },
    )
    context_window_tokens: int = Field(
        default=65536,
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:122",
            "desc_zh": "上下文窗口 token 上限",
            "desc_en": "Context window token budget",
        },
    )
    introduction_enabled: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:790",
            "desc_zh": "是否在新会话发送自我介绍",
            "desc_en": "Send a self-introduction on new sessions",
        },
    )
    introduction_template: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:795",
            "desc_zh": "自我介绍模板",
            "desc_en": "Self-introduction template",
        },
    )
    history_image_ttl_minutes: int = Field(
        default=30,
        json_schema_extra={
            "status": "effective", "ref": "agent/pipeline/context_stage.py:245",
            "desc_zh": "历史图片保留时长(分钟)",
            "desc_en": "Time-to-live for images in history (minutes)",
        },
    )
    history_image_limit: int = Field(
        default=4,
        json_schema_extra={
            "status": "effective", "ref": "agent/pipeline/context_stage.py:246",
            "desc_zh": "历史中保留的最大图片数",
            "desc_en": "Maximum images retained in history",
        },
    )
    history_image_skip_if_current: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "agent/pipeline/context_stage.py:247",
            "desc_zh": "当前轮已带图时是否跳过历史图片",
            "desc_en": "Skip history images when the current turn already has one",
        },
    )
    group_session_scope: Literal["per_user", "shared"] = Field(
        default="per_user",
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:597",
            "desc_zh": "群聊会话隔离策略:per_user 每人独立会话(默认,防群内串话),shared 整群共享一个会话",
            "desc_en": "Group session scope: per_user = isolate per sender (default), shared = whole group shares one session",
        },
    )


# ── Memory configs ───────────────────────────────────────────────────────────

class MemoryConfig(_Base):
    enabled: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:184",
            "desc_zh": "是否启用认知记忆",
            "desc_en": "Enable cognitive memory",
        },
    )
    scope_policy: Literal["legacy", "session"] = Field(
        default="session",
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:108",
            "desc_zh": "记忆作用域策略",
            "desc_en": "Memory scope policy",
        },
    )
    retrieval_on_miss: Literal["degrade", "sync"] = Field(
        default="degrade",
        json_schema_extra={
            "status": "effective", "ref": "agent/pipeline/context_stage.py:197",
            "desc_zh": "检索缓存未命中时的行为:degrade=本轮跳过检索,sync=同步补检索",
            "desc_en": "Behavior on retrieval cache miss: degrade=skip this turn, sync=fetch synchronously",
        },
    )
    cache_ttl_seconds: float = Field(
        default=60.0,
        json_schema_extra={
            "status": "effective", "ref": "agent/pipeline/context_stage.py:192",
            "desc_zh": "检索预取缓存新鲜度 TTL(秒),超时即视为未命中",
            "desc_en": "Retrieval prefetch cache freshness TTL in seconds",
        },
    )
    cache_jaccard_min: float = Field(
        default=0.3,
        json_schema_extra={
            "status": "effective", "ref": "agent/pipeline/context_stage.py:192",
            "desc_zh": "当前查询与缓存查询的最小 Jaccard 相似度,低于则视为话题突变未命中",
            "desc_en": "Min Jaccard similarity between current and cached query; below is a miss",
        },
    )
    consolidation_threshold: int = Field(
        default=20,
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:176",
            "desc_zh": "触发记忆整合的条目阈值",
            "desc_en": "Entry threshold that triggers memory consolidation",
        },
    )
    vector_enabled: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:364",
            "desc_zh": "是否启用向量检索记忆",
            "desc_en": "Enable vector-based memory retrieval",
        },
    )
    vector_dimensions: int = Field(
        default=0,
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:366",
            "desc_zh": "记忆向量维度,0=自动跟随当前嵌入模型的实际维度",
            "desc_en": "Memory embedding vector dimensions; 0 = follow the active embedding model",
        },
    )
    max_user_memories: int = Field(
        default=1000,
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:104",
            "desc_zh": "单用户记忆条目上限",
            "desc_en": "Maximum stored memories per user",
        },
    )
    max_env_memories: int = Field(
        default=500,
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:105",
            "desc_zh": "环境记忆条目上限",
            "desc_en": "Maximum stored environment memories",
        },
    )
    memory_nudge_interval: int = Field(
        default=10,
        json_schema_extra={
            "status": "effective", "ref": "agent/pipeline/inference_stage.py:189",
            "desc_zh": "提示模型记录记忆的轮次间隔",
            "desc_en": "Turn interval for nudging the model to store memories",
        },
    )
    importance_decay_days: float = Field(
        default=30.0,
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:106",
            "desc_zh": "记忆重要性衰减周期(天)",
            "desc_en": "Memory importance decay period (days)",
        },
    )
    snapshot_enabled: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "agent/pipeline/context_stage.py:107",
            "desc_zh": "是否启用记忆快照注入上下文",
            "desc_en": "Inject memory snapshots into context",
        },
    )
    contradiction_detection: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:406",
            "desc_zh": "是否启用记忆矛盾检测",
            "desc_en": "Enable memory contradiction detection",
        },
    )
    sleep_consolidation: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "agent/consolidation.py:109",
            "desc_zh": "是否启用空闲期记忆整合",
            "desc_en": "Enable idle-time (sleep) memory consolidation",
        },
    )
    archival_threshold: float = Field(
        default=0.05,
        json_schema_extra={
            "status": "effective", "ref": "memory/store.py:172",
            "desc_zh": "记忆归档分数阈值,低于此值进入归档层",
            "desc_en": "Archival score threshold; entries below it move to the archival tier",
        },
    )
    forget_threshold: float = Field(
        default=0.01,
        json_schema_extra={
            "status": "effective", "ref": "memory/store.py:172",
            "desc_zh": "记忆遗忘分数阈值,低于此值被遗忘",
            "desc_en": "Forget score threshold; entries below it are forgotten",
        },
    )
    max_working_memory: int = Field(
        default=20,
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:655",
            "desc_zh": "工作记忆条目上限",
            "desc_en": "Maximum working-memory entries",
        },
    )
    embedding_backend: Literal["auto", "local", "provider"] = Field(
        default="auto",
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:_resolve_embed_and_index",
            "desc_zh": "嵌入后端: auto=启动探测provider,失败静默回退fastembed; "
                       "local=直接用本地fastembed免探测; provider=强制provider,探测失败报错不回退",
            "desc_en": "Embedding backend: auto=probe provider at startup, fall back to fastembed "
                       "on failure; local=use local fastembed directly; provider=force provider, "
                       "error out if probe fails",
        },
    )
    embedding_model: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:370",
            "desc_zh": "记忆向量化使用的嵌入模型",
            "desc_en": "Embedding model used for memory vectorization",
        },
    )
    local_embedding_model: str = Field(
        default="BAAI/bge-small-zh-v1.5",
        json_schema_extra={
            "status": "effective", "ref": "memory/local_embed.py",
            "desc_zh": "无embed能力provider时的本地嵌入兜底模型(fastembed),空串禁用兜底",
            "desc_en": "Local fastembed fallback model when no embed-capable provider exists; empty string disables the fallback",
        },
    )
    # Latency budget for the per-message query-embedding round-trip in hybrid
    # retrieval. On timeout retrieval degrades to keyword-only for that turn.
    # Raise this if your embedding endpoint is on a high-latency network and
    # you prefer recall quality over response latency.
    embed_timeout_seconds: float = Field(
        default=1.5,
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:419",
            "desc_zh": "查询向量化的单次超时(秒),超时降级为关键词检索",
            "desc_en": "Query-embedding timeout (seconds); falls back to keyword search on timeout",
        },
    )
    embed_load_timeout_seconds: float = Field(
        default=60.0,
        json_schema_extra={
            "status": "effective", "ref": "memory/local_embed.py",
            "desc_zh": "本地嵌入模型首次加载/下载的超时(秒),超时即标记失败并降级为关键词检索,避免下载挂起拖垮进程",
            "desc_en": "Local embedding model first-load/download timeout (seconds); on timeout the embedder is marked failed and degrades to keyword search, preventing a hung download from starving the process",
        },
    )
    contradiction_scan_on_store: bool = Field(
        default=False,
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:109",
            "desc_zh": "是否在写入记忆时即时扫描矛盾",
            "desc_en": "Scan for contradictions at memory store time",
        },
    )
    auto_resolve_contradictions: bool = Field(
        default=False,
        json_schema_extra={
            "status": "effective", "ref": "memory/consolidator.py:auto_resolve",
            "desc_zh": "睡眠整合时自动消解同 key 矛盾(newest-wins),默认关闭只检测不消解",
            "desc_en": "Auto-resolve same-key contradictions (newest-wins) during sleep consolidation; off by default",
        },
    )
    reflection_enabled: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "memory/reflection.py",
            "desc_zh": "是否启用睡眠反思(归纳提炼+LLM矛盾裁决),随睡眠整合运行",
            "desc_en": "Enable sleep-time reflection (distillation + LLM conflict adjudication), piggybacking on sleep consolidation",
        },
    )


class KnowledgeConfig(_Base):
    enabled: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:215",
            "desc_zh": "是否启用知识库检索",
            "desc_en": "Enable knowledge-base retrieval",
        },
    )
    docs_dir: str = Field(
        default="data/knowledge",
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:219",
            "desc_zh": "知识库文档目录",
            "desc_en": "Knowledge base documents directory",
        },
    )
    index_path: str = Field(
        default="data/knowledge_index.json",
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:220",
            "desc_zh": "知识库索引文件路径",
            "desc_en": "Knowledge base index file path",
        },
    )
    auto_index: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:225",
            "desc_zh": "是否自动索引文档目录",
            "desc_en": "Automatically index the documents directory",
        },
    )
    chunk_size: int = Field(
        default=1200,
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:221",
            "desc_zh": "文档切块大小(字符)",
            "desc_en": "Document chunk size (characters)",
        },
    )
    chunk_overlap: int = Field(
        default=120,
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:222",
            "desc_zh": "相邻切块重叠大小(字符)",
            "desc_en": "Overlap between adjacent chunks (characters)",
        },
    )
    max_results: int = Field(
        default=5,
        json_schema_extra={
            "status": "effective", "ref": "agent/pipeline/context_stage.py:213",
            "desc_zh": "知识检索返回的最大结果数",
            "desc_en": "Maximum knowledge retrieval results returned",
        },
    )
    allowed_extensions: list[str] = Field(
        default_factory=lambda: [
            ".md", ".txt", ".rst", ".json", ".yaml", ".yml", ".py",
            ".pdf", ".docx", ".xlsx", ".pptx",
        ],
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:223",
            "desc_zh": "允许索引的文档扩展名",
            "desc_en": "Document extensions eligible for indexing",
        },
    )


# ── Multi-agent delegation configs ─────────────────────────────────────────────

class WorkerProfileConfig(_Base):
    id: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "agent/multi_agent/registry.py:21",
            "desc_zh": "子代理画像 ID",
            "desc_en": "Worker profile ID",
        },
    )
    name: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "agent/multi_agent/registry.py:22",
            "desc_zh": "子代理名称",
            "desc_en": "Worker profile name",
        },
    )
    description: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "agent/multi_agent/registry.py:23",
            "desc_zh": "子代理用途描述",
            "desc_en": "Worker profile description",
        },
    )
    instructions: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "agent/multi_agent/registry.py:24",
            "desc_zh": "子代理系统指令",
            "desc_en": "Worker profile system instructions",
        },
    )
    default_tools: list[str] = Field(
        default_factory=list,
        json_schema_extra={
            "status": "effective", "ref": "agent/multi_agent/registry.py:25",
            "desc_zh": "子代理默认可用工具",
            "desc_en": "Default tools available to the worker",
        },
    )
    model: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "agent/multi_agent/registry.py:26",
            "desc_zh": "子代理使用的模型",
            "desc_en": "Model used by the worker",
        },
    )
    max_iterations: int = Field(
        default=12,
        json_schema_extra={
            "status": "effective", "ref": "agent/multi_agent/registry.py:28",
            "desc_zh": "子代理单任务最大迭代数",
            "desc_en": "Maximum iterations per worker task",
        },
    )
    max_tokens: int = Field(
        default=4096,
        json_schema_extra={
            "status": "effective", "ref": "agent/multi_agent/registry.py:29",
            "desc_zh": "子代理生成最大 token 数",
            "desc_en": "Maximum tokens generated by the worker",
        },
    )
    temperature: float = Field(
        default=0.4,
        json_schema_extra={
            "status": "effective", "ref": "agent/multi_agent/registry.py:30",
            "desc_zh": "子代理采样温度",
            "desc_en": "Worker sampling temperature",
        },
    )


class MultiAgentConfig(_Base):
    enabled: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:424",
            "desc_zh": "是否启用多代理委派",
            "desc_en": "Enable multi-agent delegation",
        },
    )
    max_depth: int = Field(
        default=3,
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:442",
            "desc_zh": "委派嵌套最大深度",
            "desc_en": "Maximum delegation nesting depth",
        },
    )
    max_parallel_workers: int = Field(
        default=4,
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:443",
            "desc_zh": "并行子代理数上限",
            "desc_en": "Maximum parallel workers",
        },
    )
    max_iterations: int = Field(
        default=12,
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:444",
            "desc_zh": "子代理默认最大迭代数",
            "desc_en": "Default maximum iterations per worker",
        },
    )
    audit_path: str = Field(
        default="data/delegation_audit.jsonl",
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:430",
            "desc_zh": "委派审计日志路径",
            "desc_en": "Delegation audit log path",
        },
    )
    worker_profiles: list[WorkerProfileConfig] = Field(
        default_factory=list,
        json_schema_extra={
            "status": "effective", "ref": "agent/multi_agent/registry.py:19",
            "desc_zh": "子代理画像配置列表",
            "desc_en": "List of worker profile configurations",
        },
    )


# ── Scheduler configs ───────────────────────────────────────────────────────

class SchedulerConfig(_Base):
    enabled: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "app.py:118",
            "desc_zh": "是否启用任务调度器",
            "desc_en": "Enable the task scheduler",
        },
    )
    max_concurrent_jobs: int = Field(
        default=10,
        json_schema_extra={
            "status": "effective", "ref": "app.py:122",
            "desc_zh": "并发任务数上限",
            "desc_en": "Maximum concurrent scheduled jobs",
        },
    )


# ── Checkpoint configs ───────────────────────────────────────────────────────

class CheckpointConfig(_Base):
    enabled: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "checkpoint/manager.py",
            "desc_zh": "是否开启编辑前影子git快照安全网（探测不到git时自动降级）",
            "desc_en": "Enable pre-edit shadow-git checkpoint safety net (auto-degrades if git missing)",
        },
    )
    store_path: str = Field(
        default="~/.echo-agent/checkpoints/store",
        json_schema_extra={
            "status": "effective", "ref": "checkpoint/store.py",
            "desc_zh": "影子git仓库存放路径",
            "desc_en": "Path to the shadow git store",
        },
    )
    max_snapshots_per_workspace: int = Field(
        default=20,
        json_schema_extra={
            "status": "effective", "ref": "checkpoint/manager.py",
            "desc_zh": "每个工作区保留的最大快照数量",
            "desc_en": "Max snapshots retained per workspace",
        },
    )
    max_total_size_mb: int = Field(
        default=500,
        json_schema_extra={
            "status": "effective", "ref": "checkpoint/manager.py",
            "desc_zh": "整个store的总大小上限（MB），超出触发gc",
            "desc_en": "Total store size cap in MB; exceeding triggers gc",
        },
    )
    max_file_size_mb: int = Field(
        default=10,
        json_schema_extra={
            "status": "effective", "ref": "checkpoint/store.py",
            "desc_zh": "单文件超过此大小（MB）不纳入快照",
            "desc_en": "Files larger than this (MB) are excluded from snapshots",
        },
    )


# ── Validation configs ───────────────────────────────────────────────────────

class ValidationConfig(_Base):
    enabled: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "validation/__init__.py",
            "desc_zh": "是否开启写后增量校验反馈（检查器探测不到时自动降级）",
            "desc_en": "Enable post-write incremental validation feedback (auto-degrades if checkers missing)",
        },
    )
    timeout_sec: float = Field(
        default=5.0,
        json_schema_extra={
            "status": "effective", "ref": "validation/validator.py",
            "desc_zh": "单个文件校验的超时上限（秒），超时静默跳过",
            "desc_en": "Per-file validation timeout in seconds; times out silently",
        },
    )
    max_diagnostics: int = Field(
        default=10,
        json_schema_extra={
            "status": "effective", "ref": "validation/validator.py",
            "desc_zh": "追加到工具结果的诊断条数上限",
            "desc_en": "Max diagnostics appended to the tool result",
        },
    )
    max_file_size_kb: int = Field(
        default=512,
        json_schema_extra={
            "status": "effective", "ref": "validation/validator.py",
            "desc_zh": "超过此大小（KB）的文件跳过校验",
            "desc_en": "Files larger than this (KB) skip validation",
        },
    )


# ── Media understanding configs ──────────────────────────────────────────────

class MediaUnderstandingConfig(_Base):
    audio_enabled: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "agent/media/understanding/registry.py",
            "desc_zh": "是否开启入站音频/语音转写（provider 探测不到时自动降级）",
            "desc_en": "Enable inbound audio/voice transcription (auto-degrades if no provider)",
        },
    )
    audio_provider: str = Field(
        default="auto",
        json_schema_extra={
            "status": "effective", "ref": "agent/media/understanding/registry.py",
            "desc_zh": "转写后端：auto(探测) / cloud(云) / local(本地 faster-whisper)",
            "desc_en": "Transcribe backend: auto (probe) / cloud / local (faster-whisper)",
        },
    )
    min_audio_size_kb: float = Field(
        default=1.0,
        json_schema_extra={
            "status": "effective", "ref": "agent/media/understanding/audio.py",
            "desc_zh": "小于此大小(KB)的音频跳过转写（噪音/误触）",
            "desc_en": "Audio smaller than this (KB) skips transcription",
        },
    )
    max_audio_size_kb: int = Field(
        default=25000,
        json_schema_extra={
            "status": "effective", "ref": "agent/media/understanding/audio.py",
            "desc_zh": "大于此大小(KB)的音频跳过转写（控成本）",
            "desc_en": "Audio larger than this (KB) skips transcription",
        },
    )
    local_model_size: str = Field(
        default="base",
        json_schema_extra={
            "status": "effective", "ref": "agent/media/understanding/audio.py",
            "desc_zh": "本地 faster-whisper 模型规格（tiny/base/small/...）",
            "desc_en": "Local faster-whisper model size (tiny/base/small/...)",
        },
    )
    video_enabled: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "agent/media/understanding/registry.py",
            "desc_zh": "是否开启入站视频理解（抽帧+音轨；provider/ffmpeg 探测不到自动降级）",
            "desc_en": "Enable inbound video understanding (frames + audio; auto-degrades)",
        },
    )
    video_frame_count: int = Field(
        default=4,
        json_schema_extra={
            "status": "effective", "ref": "agent/media/understanding/video.py",
            "desc_zh": "视频均匀抽帧数（喂 vision 模型）",
            "desc_en": "Number of frames uniformly sampled from a video",
        },
    )
    video_vision_model: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "agent/media/understanding/video.py",
            "desc_zh": "视频画面描述的 vision 模型覆盖（空=用 provider 默认模型）",
            "desc_en": "Vision model override for video captioning (empty = provider default)",
        },
    )
    video_vision_prompt: str = Field(
        default="简要描述这段视频的画面内容。",
        json_schema_extra={
            "status": "effective", "ref": "agent/media/understanding/video.py",
            "desc_zh": "视频抽帧描述的提示词",
            "desc_en": "Prompt for video frame captioning",
        },
    )
    min_video_size_kb: float = Field(
        default=1.0,
        json_schema_extra={
            "status": "effective", "ref": "agent/media/understanding/video.py",
            "desc_zh": "小于此大小(KB)的视频跳过理解",
            "desc_en": "Video smaller than this (KB) skips understanding",
        },
    )
    max_video_size_kb: int = Field(
        default=204800,
        json_schema_extra={
            "status": "effective", "ref": "agent/media/understanding/video.py",
            "desc_zh": "大于此大小(KB)的视频跳过理解（≈200MB，成本护栏）",
            "desc_en": "Video larger than this (KB) skips understanding (~200MB cost guard)",
        },
    )
    video_ffmpeg_concurrency: int = Field(
        default=2,
        json_schema_extra={
            "status": "effective", "ref": "agent/media/understanding/video.py",
            "desc_zh": "同时运行的 ffmpeg 抽帧/抽音轨进程数上限（防多视频打爆 CPU）",
            "desc_en": "Max concurrent ffmpeg processes for video frame/audio extraction",
        },
    )
    transcription_base_url: str = Field(
        default="https://api.groq.com/openai/v1",
        json_schema_extra={
            "status": "effective", "ref": "agent/media/understanding/registry.py",
            "desc_zh": "云转写端点 base_url（OpenAI 兼容 /audio/transcriptions）",
            "desc_en": "Cloud transcription endpoint base_url (OpenAI-compatible)",
        },
    )
    transcription_model: str = Field(
        default="whisper-large-v3",
        json_schema_extra={
            "status": "effective", "ref": "agent/media/understanding/registry.py",
            "desc_zh": "云转写模型名",
            "desc_en": "Cloud transcription model name",
        },
    )


# ── Storage configs ──────────────────────────────────────────────────────────

class StorageConfig(_Base):
    database_path: str = Field(
        default="data/echo_agent.db",
        json_schema_extra={
            "status": "effective", "ref": "app.py:71",
            "desc_zh": "SQLite 数据库文件路径",
            "desc_en": "SQLite database file path",
        },
    )
    sessions_dir: str = Field(
        default="data/sessions",
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:98",
            "desc_zh": "会话数据存储目录",
            "desc_en": "Directory storing session data",
        },
    )
    memory_dir: str = Field(
        default="data/memory",
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:103",
            "desc_zh": "记忆数据存储目录",
            "desc_en": "Directory storing memory data",
        },
    )
    logs_dir: str = Field(
        default="data/logs",
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:112",
            "desc_zh": "日志文件存储目录",
            "desc_en": "Directory storing log files",
        },
    )


# ── Observability configs ────────────────────────────────────────────────────

class ObservabilityConfig(_Base):
    log_level: str = Field(
        default="INFO",
        json_schema_extra={
            "status": "effective", "ref": "app.py:62",
            "desc_zh": "日志级别",
            "desc_en": "Logging level",
        },
    )
    trace_enabled: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "observability/monitor.py:55",
            "desc_zh": "是否记录执行轨迹(关闭则不写 trace 文件)",
            "desc_en": "Whether to record execution traces (off disables trace files)",
        },
    )
    max_trace_files: int = Field(
        default=500,
        json_schema_extra={
            "status": "effective", "ref": "observability/monitor.py:95",
            "desc_zh": "trace 文件保留数量上限,超出按最旧优先轮转删除;<=0 表示不限制(禁用轮转)",
            "desc_en": "Max retained trace files; oldest are rotated out when exceeded; <=0 disables rotation",
        },
    )
    health_check_interval_seconds: int = Field(
        default=60,
        json_schema_extra={
            "status": "effective", "ref": "app.py:198",
            "desc_zh": "健康检查间隔(秒)",
            "desc_en": "Health check interval (seconds)",
        },
    )
    otel_enabled: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:202",
            "desc_zh": "是否启用 OpenTelemetry 指标导出",
            "desc_en": "Enable OpenTelemetry metrics export",
        },
    )
    otel_endpoint: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:206",
            "desc_zh": "OpenTelemetry 导出端点",
            "desc_en": "OpenTelemetry export endpoint",
        },
    )
    otel_service_name: str = Field(
        default="echo-agent",
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:205",
            "desc_zh": "OpenTelemetry 服务名",
            "desc_en": "OpenTelemetry service name",
        },
    )
    otel_export_interval_ms: int = Field(
        default=5000,
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:207",
            "desc_zh": "OpenTelemetry 指标导出间隔(毫秒)",
            "desc_en": "OpenTelemetry metrics export interval (ms)",
        },
    )
    loop_watchdog_enabled: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "observability/loop_watchdog.py:44",
            "desc_zh": "是否启用事件循环看门狗(检测 loop 冻结并自杀重启)",
            "desc_en": "Enable the event-loop watchdog (detects a frozen loop and self-exits for respawn)",
        },
    )
    loop_watchdog_warn_seconds: float = Field(
        default=5.0,
        json_schema_extra={
            "status": "effective", "ref": "observability/loop_watchdog.py:52",
            "desc_zh": "事件循环停滞多少秒后告警并转储线程栈",
            "desc_en": "Seconds of loop stall before warning and dumping thread stacks",
        },
    )
    loop_watchdog_kill_seconds: float = Field(
        default=30.0,
        json_schema_extra={
            "status": "effective", "ref": "observability/loop_watchdog.py:53",
            "desc_zh": "事件循环冻结多少秒后自杀退出以便 supervisor 重启",
            "desc_en": "Seconds of loop freeze before self-exiting for supervisor respawn",
        },
    )
    loop_watchdog_check_interval_seconds: float = Field(
        default=5.0,
        json_schema_extra={
            "status": "effective", "ref": "observability/loop_watchdog.py:54",
            "desc_zh": "看门狗线程检查心跳的间隔(秒)",
            "desc_en": "Interval (s) at which the watchdog thread checks the heartbeat",
        },
    )
    loop_watchdog_max_restarts_per_hour: int = Field(
        default=5,
        json_schema_extra={
            "status": "effective", "ref": "observability/restart_guard.py:26",
            "desc_zh": "一小时内看门狗自杀重启次数上限,超过则熔断不再自杀",
            "desc_en": "Max watchdog self-exits per hour before the circuit breaker suspends restarts",
        },
    )


# ── Compression configs ──────────────────────────────────────────────────────

class CompressionConfig(_Base):
    enabled: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "agent/compression/compressor.py:80",
            "desc_zh": "是否启用上下文压缩",
            "desc_en": "Enable context compression",
        },
    )
    trigger_ratio: float = Field(
        default=0.7,
        json_schema_extra={
            "status": "effective", "ref": "agent/compression/compressor.py:46",
            "desc_zh": "上下文占用达到该比例时触发压缩",
            "desc_en": "Context usage ratio that triggers compression",
        },
    )
    tail_budget_ratio: float = Field(
        default=0.4,
        json_schema_extra={
            "status": "effective", "ref": "agent/compression/compressor.py:60",
            "desc_zh": "压缩后保留尾部消息的预算比例",
            "desc_en": "Budget ratio reserved for tail messages after compression",
        },
    )
    head_protect_count: int = Field(
        default=3,
        json_schema_extra={
            "status": "effective", "ref": "agent/compression/compressor.py:59",
            "desc_zh": "压缩时保护不动的头部消息数",
            "desc_en": "Number of head messages protected from compression",
        },
    )
    summary_target_ratio: float = Field(
        default=0.20,
        json_schema_extra={
            "status": "effective", "ref": "agent/compression/compressor.py:69",
            "desc_zh": "摘要相对原文的目标长度比例",
            "desc_en": "Target summary length relative to source",
        },
    )
    summary_min_tokens: int = Field(
        default=2000,
        json_schema_extra={
            "status": "effective", "ref": "agent/compression/compressor.py:70",
            "desc_zh": "摘要最小 token 数",
            "desc_en": "Minimum summary tokens",
        },
    )
    summary_max_tokens: int = Field(
        default=12000,
        json_schema_extra={
            "status": "effective", "ref": "agent/compression/compressor.py:71",
            "desc_zh": "摘要最大 token 数",
            "desc_en": "Maximum summary tokens",
        },
    )
    summary_model: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "agent/compression/compressor.py:67",
            "desc_zh": "生成摘要使用的模型",
            "desc_en": "Model used to generate summaries",
        },
    )
    summary_cooldown_seconds: int = Field(
        default=600,
        json_schema_extra={
            "status": "effective", "ref": "agent/compression/compressor.py:72",
            "desc_zh": "两次压缩之间的冷却时间(秒)",
            "desc_en": "Cooldown between compressions (seconds)",
        },
    )
    tool_pruning_enabled: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "agent/compression/compressor.py:56",
            "desc_zh": "是否启用工具结果剪枝",
            "desc_en": "Enable pruning of tool results",
        },
    )
    tool_pruning_tail_budget_ratio: float = Field(
        default=0.3,
        json_schema_extra={
            "status": "effective", "ref": "agent/compression/compressor.py:53",
            "desc_zh": "工具结果剪枝保留尾部的预算比例",
            "desc_en": "Tail budget ratio retained when pruning tool results",
        },
    )
    max_compression_count: int = Field(
        default=10,
        json_schema_extra={
            "status": "effective", "ref": "agent/compression/compressor.py:169",
            "desc_zh": "单会话最大压缩次数",
            "desc_en": "Maximum compressions per session",
        },
    )


# ── Gateway configs ─────────────────────────────────────────────────────────

class GatewaySessionPolicyConfig(_Base):
    mode: Literal["daily", "idle", "both", "none"] = Field(
        default="idle",
        json_schema_extra={
            "status": "effective", "ref": "gateway/session_policy.py:14",
            "desc_zh": "网关会话重置策略",
            "desc_en": "Gateway session reset policy",
        },
    )
    daily_reset_hour: int = Field(
        default=4,
        json_schema_extra={
            "status": "effective", "ref": "gateway/session_policy.py:15",
            "desc_zh": "每日重置会话的小时(0-23)",
            "desc_en": "Hour of day to reset sessions (0-23)",
        },
    )
    idle_timeout_minutes: int = Field(
        default=1440,
        json_schema_extra={
            "status": "effective", "ref": "gateway/session_policy.py:16",
            "desc_zh": "会话空闲超时(分钟)",
            "desc_en": "Session idle timeout (minutes)",
        },
    )


class GatewayPlatformConfig(_Base):
    rate_limit_rpm: int = Field(
        default=30,
        json_schema_extra={
            "status": "effective", "ref": "gateway/server.py:91",
            "desc_zh": "该平台每分钟请求上限",
            "desc_en": "Per-minute request cap for this platform",
        },
    )


class GatewayAuthConfig(_Base):
    mode: Literal["open", "allowlist", "pairing"] = Field(
        default="allowlist",
        json_schema_extra={
            "status": "effective", "ref": "gateway/auth.py:20",
            "desc_zh": "网关鉴权模式",
            "desc_en": "Gateway authentication mode",
        },
    )
    allowed_users: list[str] = Field(
        default_factory=list,
        json_schema_extra={
            "status": "effective", "ref": "gateway/auth.py:21",
            "desc_zh": "允许访问网关的用户白名单",
            "desc_en": "Allowlist of users permitted to access the gateway",
        },
    )
    admin_users: list[str] = Field(
        default_factory=list,
        json_schema_extra={
            "status": "effective", "ref": "gateway/auth.py:22",
            "desc_zh": "网关管理员用户列表",
            "desc_en": "Gateway administrator users",
        },
    )
    api_tokens: list[str] = Field(
        default_factory=list,
        json_schema_extra={
            "status": "effective", "ref": "gateway/auth.py:23",
            "desc_zh": "网关 API 访问令牌列表",
            "desc_en": "Gateway API access tokens",
        },
    )
    admin_tokens: list[str] = Field(
        default_factory=list,
        json_schema_extra={
            "status": "effective", "ref": "gateway/auth.py:23",
            "desc_zh": "高危管理接口(关停/技能导入安装删除/知识库上传删除)专用令牌；为空时回退到 api_tokens",
            "desc_en": "Tokens required for high-risk admin endpoints (shutdown, skills import/install/delete, knowledge upload/delete); falls back to api_tokens when empty",
        },
    )
    allowed_origins: list[str] = Field(
        default_factory=list,
        json_schema_extra={
            "status": "effective", "ref": "gateway/server.py:_check_csrf",
            "desc_zh": "浏览器 Origin 白名单(CSRF 防护)；留空则不启用 CSRF 检查(默认),配置后仅放行白名单内的跨站浏览器请求,非浏览器客户端始终不受影响",
            "desc_en": "Allowlisted browser Origins (CSRF protection); empty disables the CSRF check (default). When set, only listed cross-site browser requests pass; non-browser clients are always unaffected",
        },
    )
    token_header: str = Field(
        default="X-Echo-Agent-Token",
        json_schema_extra={
            "status": "effective", "ref": "gateway/auth.py:24",
            "desc_zh": "携带 API 令牌的请求头名",
            "desc_en": "Request header carrying the API token",
        },
    )
    pairing_ttl_seconds: int = Field(
        default=300,
        json_schema_extra={
            "status": "effective", "ref": "gateway/auth.py:25",
            "desc_zh": "配对模式令牌有效期(秒)",
            "desc_en": "Pairing-mode token time-to-live (seconds)",
        },
    )


class GatewayConfig(_Base):
    enabled: bool = Field(
        default=False,
        json_schema_extra={
            "status": "effective", "ref": "app.py:267",
            "desc_zh": "是否启用网关服务",
            "desc_en": "Enable the gateway service",
        },
    )
    host: str = Field(
        default="0.0.0.0",
        json_schema_extra={
            "status": "effective", "ref": "gateway/server.py:128",
            "desc_zh": "网关监听地址",
            "desc_en": "Gateway bind address",
        },
    )
    port: int = Field(
        default=58123,
        json_schema_extra={
            "status": "effective", "ref": "gateway/server.py:129",
            "desc_zh": "网关监听端口",
            "desc_en": "Gateway listen port",
        },
    )
    api_prefix: str = Field(
        default="/api/v1",
        json_schema_extra={
            "status": "effective", "ref": "gateway/server.py:195",
            "desc_zh": "网关 API 路径前缀",
            "desc_en": "Gateway API path prefix",
        },
    )
    ws_path: str = Field(
        default="/ws",
        json_schema_extra={
            "status": "effective", "ref": "gateway/server.py:207",
            "desc_zh": "网关 WebSocket 路径",
            "desc_en": "Gateway WebSocket path",
        },
    )
    session_policy: GatewaySessionPolicyConfig = Field(default_factory=GatewaySessionPolicyConfig)
    auth: GatewayAuthConfig = Field(default_factory=GatewayAuthConfig)
    platforms: dict[str, GatewayPlatformConfig] = Field(
        default_factory=dict,
        json_schema_extra={
            "status": "effective", "ref": "gateway/server.py:90",
            "desc_zh": "各接入平台的网关配置(键为平台名)",
            "desc_en": "Per-platform gateway configurations keyed by platform",
        },
    )
    media_cache_dir: str = Field(
        default="data/media_cache",
        json_schema_extra={
            "status": "effective", "ref": "gateway/server.py:79",
            "desc_zh": "网关媒体缓存目录",
            "desc_en": "Gateway media cache directory",
        },
    )
    media_cache_max_mb: int = Field(
        default=500,
        json_schema_extra={
            "status": "effective", "ref": "gateway/server.py:80",
            "desc_zh": "媒体缓存大小上限(MB)",
            "desc_en": "Media cache size limit (MB)",
        },
    )
    emit_progress_events: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "agent/pipeline/context_stage.py:82",
            "desc_zh": "是否向网关客户端推送进度事件",
            "desc_en": "Emit progress events to gateway clients",
        },
    )
    progress_debug: bool = Field(
        default=False,
        json_schema_extra={
            "status": "effective", "ref": "agent/pipeline/context_stage.py:177",
            "desc_zh": "是否输出进度事件调试信息",
            "desc_en": "Emit progress-event debug information",
        },
    )
    hooks_dir: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "gateway/server.py:94",
            "desc_zh": "网关钩子脚本目录",
            "desc_en": "Gateway hook scripts directory",
        },
    )


# ── Skills configs ───────────────────────────────────────────────────────────

class SkillsConfig(_Base):
    skills_dir: str = Field(
        default="skills",
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:227",
            "desc_zh": "技能脚本目录",
            "desc_en": "Skills directory",
        },
    )
    creation_nudge_interval: int = Field(
        default=10,
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:237",
            "desc_zh": "提示创建技能的轮次间隔",
            "desc_en": "Turn interval for nudging skill creation",
        },
    )
    disabled: list[str] = Field(
        default_factory=list,
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:232",
            "desc_zh": "禁用的技能列表",
            "desc_en": "List of disabled skills",
        },
    )
    external_dirs: list[str] = Field(
        default_factory=list,
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:231",
            "desc_zh": "额外加载技能的外部目录",
            "desc_en": "External directories from which to load skills",
        },
    )
    allow_lazy_installs: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "dependencies/lazy_deps.py:168",
            "desc_zh": "是否允许技能运行时按需安装依赖",
            "desc_en": "Allow lazy on-demand dependency installs for skills",
        },
    )
    admission_policy: Literal["auto_write", "stage_for_review", "manual_only"] = Field(
        default="stage_for_review",
        json_schema_extra={
            "status": "effective", "ref": "skills/admission.py",
            "desc_zh": "技能自动沉淀准入策略:auto_write 按风险自动写 / stage_for_review 低风险自动高风险暂存 / manual_only 一律暂存",
            "desc_en": "Skill auto-distillation admission policy",
        },
    )
    auto_write_risk: Literal["low", "high"] = Field(
        default="low",
        json_schema_extra={
            "status": "effective", "ref": "skills/admission.py",
            "desc_zh": "auto_write 档下允许自动写盘的最高风险等级",
            "desc_en": "Highest risk level auto-written under the auto_write policy",
        },
    )


# ── Bus configs ─────────────────────────────────────────────────────────────

class BusConfig(_Base):
    max_queue_size: int = Field(
        default=1000,
        json_schema_extra={
            "status": "effective", "ref": "app.py:75",
            "desc_zh": "事件总线队列容量上限",
            "desc_en": "Event bus queue capacity",
        },
    )
    max_concurrency: int = Field(
        default=50,
        json_schema_extra={
            "status": "effective", "ref": "app.py:76",
            "desc_zh": "事件总线并发处理上限",
            "desc_en": "Event bus max concurrent handlers",
        },
    )


# ── Rate limit configs ──────────────────────────────────────────────────────

class RateLimitConfig(_Base):
    session_rpm: int = Field(
        default=20,
        json_schema_extra={
            "status": "effective", "ref": "app.py:81",
            "desc_zh": "单会话每分钟请求上限",
            "desc_en": "Per-session requests-per-minute cap",
        },
    )
    session_burst: int = Field(
        default=5,
        json_schema_extra={
            "status": "effective", "ref": "app.py:82",
            "desc_zh": "单会话突发请求上限",
            "desc_en": "Per-session burst allowance",
        },
    )


# ── Circuit breaker configs ─────────────────────────────────────────────────

class CircuitBreakerConfig(_Base):
    failure_threshold: int = Field(
        default=5,
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:253",
            "desc_zh": "触发熔断的连续失败次数",
            "desc_en": "Consecutive failures that trip the breaker",
        },
    )
    recovery_seconds: float = Field(
        default=60.0,
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:254",
            "desc_zh": "熔断后尝试恢复的等待时间(秒)",
            "desc_en": "Wait before attempting recovery after tripping (seconds)",
        },
    )
    half_open_max: int = Field(
        default=2,
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:255",
            "desc_zh": "半开状态允许的试探请求数",
            "desc_en": "Probe requests allowed in half-open state",
        },
    )


# ── Root config ──────────────────────────────────────────────────────────────

class PlanningConfig(_Base):
    enabled: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:189",
            "desc_zh": "是否启用任务规划",
            "desc_en": "Enable task planning",
        },
    )
    default_strategy: str = Field(
        default="auto",
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:193",
            "desc_zh": "默认规划策略",
            "desc_en": "Default planning strategy",
        },
    )
    max_tree_depth: int = Field(
        default=5,
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:194",
            "desc_zh": "规划树最大深度",
            "desc_en": "Maximum planning tree depth",
        },
    )
    max_branches: int = Field(
        default=3,
        json_schema_extra={
            "status": "effective", "ref": "agent/planning/strategies.py:131",
            "desc_zh": "思维树(ToT)策略探索的候选分支数",
            "desc_en": "Number of candidate branches the Tree-of-Thought strategy explores",
        },
    )
    reflection_enabled: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:195",
            "desc_zh": "是否启用规划反思",
            "desc_en": "Enable planning reflection",
        },
    )


class A2AConfig(_Base):
    enabled: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "gateway/server.py:209",
            "desc_zh": "是否启用 A2A(agent-to-agent)接口",
            "desc_en": "Enable the A2A (agent-to-agent) interface",
        },
    )
    agent_name: str = Field(
        default="echo-agent",
        json_schema_extra={
            "status": "effective", "ref": "gateway/server.py:213",
            "desc_zh": "对外暴露的 A2A 代理名",
            "desc_en": "Agent name exposed over A2A",
        },
    )
    agent_description: str = Field(
        default="A modular AI agent framework",
        json_schema_extra={
            "status": "effective", "ref": "gateway/server.py:214",
            "desc_zh": "对外暴露的 A2A 代理描述",
            "desc_en": "Agent description exposed over A2A",
        },
    )
    capabilities: list[str] = Field(
        default_factory=lambda: ["chat", "tool_use"],
        json_schema_extra={
            "status": "effective", "ref": "gateway/server.py:212",
            "desc_zh": "A2A AgentCard 对外声明的能力标签",
            "desc_en": "Capability tags advertised in the A2A AgentCard",
        },
    )


class PluginsConfig(_Base):
    """Plugin system configuration."""

    enabled: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "plugins/manager.py:62",
            "desc_zh": "是否启用插件系统",
            "desc_en": "Enable the plugin system",
        },
    )
    allow: list[str] = Field(
        default_factory=list,
        json_schema_extra={
            "status": "effective", "ref": "plugins/manager.py:103",
            "desc_zh": "允许加载的插件白名单",
            "desc_en": "Allowlist of plugins permitted to load",
        },
    )
    deny: list[str] = Field(
        default_factory=list,
        json_schema_extra={
            "status": "effective", "ref": "plugins/manager.py:102",
            "desc_zh": "禁止加载的插件黑名单",
            "desc_en": "Blocklist of plugins forbidden from loading",
        },
    )
    extra_dirs: list[str] = Field(
        default_factory=list,
        json_schema_extra={
            "status": "effective", "ref": "plugins/manager.py:67",
            "desc_zh": "额外的插件搜索目录",
            "desc_en": "Additional plugin search directories",
        },
    )
    config: dict[str, dict[str, Any]] = Field(
        default_factory=dict,
        json_schema_extra={
            "status": "effective", "ref": "plugins/manager.py:140",
            "desc_zh": "各插件的自定义配置(键为插件名)",
            "desc_en": "Per-plugin custom configuration keyed by plugin",
        },
    )
    trusted_plugins: list[str] = Field(
        default_factory=list,
        json_schema_extra={
            "status": "effective", "ref": "plugins/manager.py:144",
            "desc_zh": "免权限校验的可信插件列表",
            "desc_en": "Trusted plugins exempt from permission checks",
        },
    )
    permission_mode: Literal["compat", "strict"] = Field(
        default="compat",
        json_schema_extra={
            "status": "effective", "ref": "plugins/manager.py:146",
            "desc_zh": "插件权限模式",
            "desc_en": "Plugin permission mode",
        },
    )


class EvalConfig(_Base):
    dataset_path: str = Field(
        default="data/eval",
        json_schema_extra={
            "status": "effective", "ref": "__main__.py:31",
            "desc_zh": "评测数据集路径",
            "desc_en": "Evaluation dataset path",
        },
    )
    timeout_per_case: int = Field(
        default=120,
        json_schema_extra={
            "status": "effective", "ref": "__main__.py:61",
            "desc_zh": "单条评测用例超时(秒)",
            "desc_en": "Timeout per evaluation case (seconds)",
        },
    )


class EvolutionConfig(_Base):
    """Self-evolving skill harness — see echo_agent/evolution/."""

    enabled: bool = Field(
        default=False,
        json_schema_extra={
            "status": "effective", "ref": "app.py:151",
            "desc_zh": "是否启用自进化技能引擎",
            "desc_en": "Enable the self-evolving skill engine",
        },
    )
    trigger_mode: Literal["manual", "threshold", "scheduled"] = Field(
        default="manual",
        json_schema_extra={
            "status": "effective", "ref": "evolution/engine.py:101",
            "desc_zh": "进化触发模式",
            "desc_en": "Evolution trigger mode",
        },
    )
    threshold_trajectories: int = Field(
        default=50,
        json_schema_extra={
            "status": "effective", "ref": "evolution/engine.py:102",
            "desc_zh": "threshold 模式触发所需轨迹数",
            "desc_en": "Trajectory count triggering threshold mode",
        },
    )
    cron_expression: str = Field(
        default="0 4 * * *",
        json_schema_extra={
            "status": "effective", "ref": "evolution/engine.py:103",
            "desc_zh": "scheduled 模式的 cron 表达式",
            "desc_en": "Cron expression for scheduled mode",
        },
    )
    max_candidates_per_run: int = Field(
        default=3,
        json_schema_extra={
            "status": "effective", "ref": "evolution/engine.py:80",
            "desc_zh": "单次进化生成的候选上限",
            "desc_en": "Maximum candidates generated per run",
        },
    )
    max_trajectories_per_run: int = Field(
        default=200,
        json_schema_extra={
            "status": "effective", "ref": "evolution/engine.py:347",
            "desc_zh": "单次进化处理的轨迹上限",
            "desc_en": "Maximum trajectories processed per run",
        },
    )
    eval_dataset_path: str = Field(
        default="data/eval/baseline.yaml",
        json_schema_extra={
            "status": "effective", "ref": "app.py:157",
            "desc_zh": "进化评测基线数据集路径",
            "desc_en": "Evolution evaluation baseline dataset path",
        },
    )
    regression_threshold: float = Field(
        default=0.05,
        json_schema_extra={
            "status": "effective", "ref": "evolution/engine.py:91",
            "desc_zh": "判定回归的分数下降阈值",
            "desc_en": "Score-drop threshold that flags a regression",
        },
    )
    require_strict_improvement: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "evolution/engine.py:92",
            "desc_zh": "是否要求严格改进才晋升",
            "desc_en": "Require strict improvement before promotion",
        },
    )
    min_eval_cases: int = Field(
        default=3,
        json_schema_extra={
            "status": "effective", "ref": "evolution/gate.py:490",
            "desc_zh": "晋升所需的最小评测用例数,样本不足则判定不确定不晋升",
            "desc_en": "Minimum eval cases required to promote; fewer is inconclusive",
        },
    )
    record_trajectories: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "evolution/engine.py:126",
            "desc_zh": "是否记录执行轨迹用于进化",
            "desc_en": "Record execution trajectories for evolution",
        },
    )
    trajectory_retention_days: int = Field(
        default=30,
        json_schema_extra={
            "status": "effective", "ref": "evolution/engine.py:128",
            "desc_zh": "轨迹保留天数",
            "desc_en": "Trajectory retention period (days)",
        },
    )
    evolver_model: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "evolution/engine.py:81",
            "desc_zh": "执行进化所用模型",
            "desc_en": "Model used to perform evolution",
        },
    )
    skill_size_limit_bytes: int = Field(
        default=50_000,
        json_schema_extra={
            "status": "effective", "ref": "evolution/engine.py:82",
            "desc_zh": "进化产出技能的大小上限(字节)",
            "desc_en": "Size limit for evolved skills (bytes)",
        },
    )
    redact_args: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "evolution/engine.py:72",
            "desc_zh": "记录轨迹时是否脱敏工具参数",
            "desc_en": "Redact tool arguments when recording trajectories",
        },
    )
    eval_parallel: int = Field(
        default=2,
        json_schema_extra={
            "status": "effective", "ref": "app.py:167",
            "desc_zh": "进化评测并发度",
            "desc_en": "Evolution evaluation parallelism",
        },
    )
    eval_timeout_seconds: int = Field(
        default=60,
        json_schema_extra={
            "status": "effective", "ref": "app.py:168",
            "desc_zh": "进化评测单用例超时(秒)",
            "desc_en": "Evolution evaluation per-case timeout (seconds)",
        },
    )
    cooldown_seconds_after_promote: int = Field(
        default=86_400,
        json_schema_extra={
            "status": "effective", "ref": "evolution/engine.py:93",
            "desc_zh": "晋升后再次进化的冷却时间(秒)",
            "desc_en": "Cooldown after a promotion before evolving again (seconds)",
        },
    )
    auto_promote: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "evolution/engine.py:94",
            "desc_zh": "是否自动晋升通过评测的候选",
            "desc_en": "Auto-promote candidates that pass evaluation",
        },
    )
    candidate_review_required: bool = Field(
        default=False,
        json_schema_extra={
            "status": "effective", "ref": "evolution/engine.py:95",
            "desc_zh": "晋升前是否需要人工审查候选",
            "desc_en": "Require human review of candidates before promotion",
        },
    )


class UIConfig(_Base):
    """User interface preferences (CLI / setup wizard)."""

    locale: Literal["en", "zh", "auto"] = Field(
        default="auto",
        json_schema_extra={
            "status": "effective", "ref": "cli/setup.py:1054",
            "desc_zh": "界面语言",
            "desc_en": "Interface language",
        },
    )


class ToolConcurrencyConfig(_Base):
    """Concurrent execution of read-only, non-overlapping tool calls."""

    enabled: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "agent/pipeline/inference_stage.py",
            "desc_zh": "是否对只读、路径不冲突的工具并发执行",
            "desc_en": "Run read-only, non-overlapping tool calls concurrently",
        },
    )
    max_concurrent: int = Field(
        default=4,
        ge=1,
        json_schema_extra={
            "status": "effective", "ref": "agent/pipeline/inference_stage.py",
            "desc_zh": "工具并发上限(1 等价关闭并发,退化为串行)",
            "desc_en": "Max concurrent tools (1 disables concurrency = serial)",
        },
    )


class HeartbeatConfig(_Base):
    """Long-running-turn progress heartbeat (level-triggered feedback)."""

    enabled: bool = Field(
        default=True,
        json_schema_extra={
            "status": "effective", "ref": "agent/progress_heartbeat.py",
            "desc_zh": "长任务静默时是否定时播报进度心跳",
            "desc_en": "Emit periodic progress heartbeat during long-running turns",
        },
    )
    first_delay_sec: int = Field(
        default=30, ge=0,
        json_schema_extra={
            "status": "effective", "ref": "agent/progress_heartbeat.py",
            "desc_zh": "首条心跳前的静默阈值(秒),短任务不触发",
            "desc_en": "Silence threshold (sec) before the first heartbeat",
        },
    )
    min_interval_sec: int = Field(
        default=60, ge=1,
        json_schema_extra={
            "status": "effective", "ref": "agent/progress_heartbeat.py",
            "desc_zh": "两次可见反馈之间的最小间隔(秒),压制高频里程碑",
            "desc_en": "Minimum interval (sec) between visible feedback",
        },
    )
    verbosity: Literal["key_milestones", "every_tool", "silent"] = Field(
        default="key_milestones",
        json_schema_extra={
            "status": "effective", "ref": "channels/manager.py",
            "desc_zh": "心跳详细度:仅关键里程碑/每个工具/不发文字",
            "desc_en": "Heartbeat verbosity tier",
        },
    )
    template: str = Field(
        default="⏳ {activity}（已用时 {elapsed}）",
        json_schema_extra={
            "status": "effective", "ref": "agent/progress_heartbeat.py",
            "desc_zh": "心跳文案模板,支持 {elapsed} 与 {activity} 占位",
            "desc_en": "Heartbeat text template with {elapsed}/{activity}",
        },
    )


class InspectionConfig(_Base):
    enabled: bool = Field(
        default=False,
        json_schema_extra={
            "status": "effective", "ref": "app.py",
            "desc_zh": "是否开启主动巡检（默认关；需在 INSPECT.md 声明巡检项）",
            "desc_en": "Enable proactive inspection (default off; declare items in INSPECT.md)",
        },
    )
    tick_interval_sec: int = Field(
        default=300,
        json_schema_extra={
            "status": "effective", "ref": "app.py",
            "desc_zh": "巡检节拍器扫描到期项的间隔（秒），非每项巡检频率",
            "desc_en": "Inspection tick interval (seconds) for scanning due items",
        },
    )
    inspect_file: str = Field(
        default="INSPECT.md",
        json_schema_extra={
            "status": "effective", "ref": "agent/inspection/store.py",
            "desc_zh": "巡检清单文件名（workspace 相对路径）",
            "desc_en": "Inspection checklist filename (workspace-relative)",
        },
    )
    max_items_per_tick: int = Field(
        default=5,
        json_schema_extra={
            "status": "effective", "ref": "agent/inspection/store.py",
            "desc_zh": "单次节拍最多投给 agent 的到期巡检项数",
            "desc_en": "Max due items dispatched to the agent per tick",
        },
    )
    deliver_channel: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "app.py",
            "desc_zh": "巡检告警投递通道（空则用注册时的 session 兜底）",
            "desc_en": "Inspection alert delivery channel (empty falls back to registering session)",
        },
    )
    deliver_chat_id: str = Field(
        default="",
        json_schema_extra={
            "status": "effective", "ref": "app.py",
            "desc_zh": "巡检告警投递会话 id（空则用注册时的 session 兜底）",
            "desc_en": "Inspection alert delivery chat id (empty falls back to registering session)",
        },
    )


class AgentBehaviorConfig(_Base):
    """High-level agent loop tuning surfaced by the setup wizard.

    These mirror knobs scattered across other configs but give the wizard
    a single, opinionated home so users don't have to know that
    ``max_iterations`` lives elsewhere. ``AgentLoop`` reads from here when
    a value is non-default; otherwise it falls back to its built-in 40.
    """

    max_iterations: int = Field(
        default=40,
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:236",
            "desc_zh": "agent 主循环最大迭代数",
            "desc_en": "Maximum iterations of the agent main loop",
        },
    )
    tool_concurrency: ToolConcurrencyConfig = Field(
        default_factory=ToolConcurrencyConfig,
    )
    heartbeat: HeartbeatConfig = Field(default_factory=HeartbeatConfig)
    inspection: InspectionConfig = Field(default_factory=InspectionConfig)


class CostConfig(_Base):
    enabled: bool = Field(
        default=False,
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:283",
            "desc_zh": "是否启用成本追踪与预算控制",
            "desc_en": "Enable cost tracking and budget control",
        },
    )
    daily_budget_usd: float = Field(
        default=0.0,
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:284",
            "desc_zh": "每日成本预算(美元,0 为不限)",
            "desc_en": "Daily cost budget in USD (0 = unlimited)",
        },
    )
    soft_threshold_ratio: float = Field(
        default=0.8,
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:285",
            "desc_zh": "达到预算该比例时发出软告警",
            "desc_en": "Budget ratio at which a soft warning is raised",
        },
    )
    pricing_overrides: dict = Field(
        default_factory=dict,
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:286",
            "desc_zh": "模型定价覆盖表",
            "desc_en": "Model pricing override table",
        },
    )


class Config(_Base):
    security: SecurityConfig = Field(default_factory=SecurityConfig)
    channels: ChannelsConfig = Field(default_factory=ChannelsConfig)
    models: ModelsConfig = Field(default_factory=ModelsConfig)
    tools: ToolsConfig = Field(default_factory=ToolsConfig)
    execution: ExecutionConfig = Field(default_factory=ExecutionConfig)
    permissions: PermissionsConfig = Field(default_factory=PermissionsConfig)
    credentials: CredentialSecurityConfig = Field(default_factory=CredentialSecurityConfig)
    session: SessionConfig = Field(default_factory=SessionConfig)
    memory: MemoryConfig = Field(default_factory=MemoryConfig)
    knowledge: KnowledgeConfig = Field(default_factory=KnowledgeConfig)
    multi_agent: MultiAgentConfig = Field(default_factory=MultiAgentConfig)
    scheduler: SchedulerConfig = Field(default_factory=SchedulerConfig)
    checkpoint: CheckpointConfig = Field(default_factory=CheckpointConfig)
    validation: ValidationConfig = Field(default_factory=ValidationConfig)
    media_understanding: MediaUnderstandingConfig = Field(default_factory=MediaUnderstandingConfig)
    storage: StorageConfig = Field(default_factory=StorageConfig)
    observability: ObservabilityConfig = Field(default_factory=ObservabilityConfig)
    skills: SkillsConfig = Field(default_factory=SkillsConfig)
    compression: CompressionConfig = Field(default_factory=CompressionConfig)
    gateway: GatewayConfig = Field(default_factory=GatewayConfig)
    planning: PlanningConfig = Field(default_factory=PlanningConfig)
    a2a: A2AConfig = Field(default_factory=A2AConfig)
    evaluation: EvalConfig = Field(default_factory=EvalConfig)
    bus: BusConfig = Field(default_factory=BusConfig)
    rate_limit: RateLimitConfig = Field(default_factory=RateLimitConfig)
    circuit_breaker: CircuitBreakerConfig = Field(default_factory=CircuitBreakerConfig)
    plugins: PluginsConfig = Field(default_factory=PluginsConfig)
    ui: UIConfig = Field(default_factory=UIConfig)
    agent: AgentBehaviorConfig = Field(default_factory=AgentBehaviorConfig)
    evolution: EvolutionConfig = Field(default_factory=EvolutionConfig)
    cost: CostConfig = Field(default_factory=CostConfig)
    workspace: str = Field(
        default="~/.echo-agent",
        json_schema_extra={
            "status": "effective", "ref": "app.py:64",
            "desc_zh": "agent 工作区根目录",
            "desc_en": "Agent workspace root directory",
        },
    )
