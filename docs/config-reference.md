# Echo Agent 配置参考

## security

| 字段 | snake | type | default | choices | 说明 |
|---|---|---|---|---|---|
| `security.profile` | `security.profile` | Literal | `personal_cli` | personal_cli/daemon/public_gateway | 整体安全档位预设 |

## channels

| 字段 | snake | type | default | choices | 说明 |
|---|---|---|---|---|---|
| `channels.telegram.enabled` | `channels.telegram.enabled` | bool | `false` | — | 是否启用 Telegram 通道 |
| `channels.telegram.token` | `channels.telegram.token` | str | `""` | — | Telegram Bot API token |
| `channels.telegram.allowFrom` | `channels.telegram.allow_from` | list | `[]` | — | 允许与机器人交互的用户白名单(空为不限制) |
| `channels.telegram.proxy` | `channels.telegram.proxy` | str | None | `null` | — | 访问 Telegram API 的代理地址 |
| `channels.telegram.groupPolicy` | `channels.telegram.group_policy` | Literal | `mention` | open/mention | 群聊响应策略:open 全部响应,mention 仅被@时响应 |
| `channels.telegram.reactionsEnabled` | `channels.telegram.reactions_enabled` | bool | `true` | — | 是否对消息添加表情回应 |
| `channels.discord.enabled` | `channels.discord.enabled` | bool | `false` | — | 是否启用 Discord 通道 |
| `channels.discord.token` | `channels.discord.token` | str | `""` | — | Discord Bot token |
| `channels.discord.allowFrom` | `channels.discord.allow_from` | list | `[]` | — | 允许与机器人交互的用户白名单(空为不限制) |
| `channels.discord.groupPolicy` | `channels.discord.group_policy` | Literal | `mention` | open/mention | 群聊响应策略:open 全部响应,mention 仅被@时响应 |
| `channels.discord.reactionsEnabled` | `channels.discord.reactions_enabled` | bool | `true` | — | 是否对消息添加表情回应 |
| `channels.webhook.enabled` | `channels.webhook.enabled` | bool | `false` | — | 是否启用 Webhook 通道 |
| `channels.webhook.host` | `channels.webhook.host` | str | `0.0.0.0` | — | Webhook 服务监听地址 |
| `channels.webhook.port` | `channels.webhook.port` | int | `8080` | — | Webhook 服务监听端口 |
| `channels.webhook.secret` | `channels.webhook.secret` | str | `""` | — | 校验入站请求签名的密钥 |
| `channels.webhook.path` | `channels.webhook.path` | str | `/webhook` | — | Webhook 接收路径 |
| `channels.webhook.maxPending` | `channels.webhook.max_pending` | int | `1000` | — | 待处理 webhook 请求队列上限 |
| `channels.cli.enabled` | `channels.cli.enabled` | bool | `true` | — | 是否启用命令行通道 |
| `channels.cron.enabled` | `channels.cron.enabled` | bool | `false` | — | 是否启用定时任务通道 |
| `channels.slack.enabled` | `channels.slack.enabled` | bool | `false` | — | 是否启用 Slack 通道 |
| `channels.slack.botToken` | `channels.slack.bot_token` | str | `""` | — | Slack bot token(xoxb-) |
| `channels.slack.appToken` | `channels.slack.app_token` | str | `""` | — | Slack app-level token(xapp-),用于 Socket 模式 |
| `channels.slack.allowFrom` | `channels.slack.allow_from` | list | `[]` | — | 允许与机器人交互的用户白名单(空为不限制) |
| `channels.slack.reactionsEnabled` | `channels.slack.reactions_enabled` | bool | `true` | — | 是否对消息添加表情回应 |
| `channels.whatsapp.enabled` | `channels.whatsapp.enabled` | bool | `false` | — | 是否启用 WhatsApp 通道 |
| `channels.whatsapp.verifyToken` | `channels.whatsapp.verify_token` | str | `""` | — | WhatsApp webhook 验证 token |
| `channels.whatsapp.accessToken` | `channels.whatsapp.access_token` | str | `""` | — | WhatsApp Cloud API 访问令牌 |
| `channels.whatsapp.phoneNumberId` | `channels.whatsapp.phone_number_id` | str | `""` | — | WhatsApp 发送号码 ID |
| `channels.whatsapp.webhookPath` | `channels.whatsapp.webhook_path` | str | `/whatsapp` | — | WhatsApp webhook 接收路径 |
| `channels.whatsapp.host` | `channels.whatsapp.host` | str | `0.0.0.0` | — | WhatsApp 服务监听地址 |
| `channels.whatsapp.port` | `channels.whatsapp.port` | int | `8081` | — | WhatsApp 服务监听端口 |
| `channels.weixin.enabled` | `channels.weixin.enabled` | bool | `false` | — | 是否启用微信(个人客服)通道 |
| `channels.weixin.accountId` | `channels.weixin.account_id` | str | `""` | — | 微信客服账号 ID |
| `channels.weixin.token` | `channels.weixin.token` | str | `""` | — | 微信接入鉴权 token |
| `channels.weixin.baseUrl` | `channels.weixin.base_url` | str | `https://ilinkai.weixin.qq.com` | — | 微信 API 基础地址 |
| `channels.weixin.cdnBaseUrl` | `channels.weixin.cdn_base_url` | str | `https://novac2c.cdn.weixin.qq.com/c2c` | — | 微信媒体 CDN 基础地址 |
| `channels.weixin.allowFrom` | `channels.weixin.allow_from` | list | `[]` | — | 允许与机器人交互的用户白名单(空为不限制) |
| `channels.weixin.dmPolicy` | `channels.weixin.dm_policy` | str | `open` | — | 私聊响应策略 |
| `channels.weixin.dataDir` | `channels.weixin.data_dir` | str | `""` | — | 微信通道本地数据目录 |
| `channels.qqbot.enabled` | `channels.qqbot.enabled` | bool | `false` | — | 是否启用 QQ 机器人通道 |
| `channels.qqbot.appId` | `channels.qqbot.app_id` | str | `""` | — | QQ 机器人 AppID |
| `channels.qqbot.appSecret` | `channels.qqbot.app_secret` | str | `""` | — | QQ 机器人 AppSecret |
| `channels.qqbot.allowFrom` | `channels.qqbot.allow_from` | list | `[]` | — | 允许与机器人交互的用户白名单(空为不限制) |
| `channels.qqbot.sandbox` | `channels.qqbot.sandbox` | bool | `false` | — | 是否使用 QQ 沙箱环境 |
| `channels.qqbot.markdownSupport` | `channels.qqbot.markdown_support` | bool | `false` | — | 是否启用 Markdown 消息支持 |
| `channels.qqbot.mediaEnabled` | `channels.qqbot.media_enabled` | bool | `true` | — | 是否启用媒体(图片/文件)收发 |
| `channels.qqbot.mediaMaxFileSizeMb` | `channels.qqbot.media_max_file_size_mb` | int | `20` | — | 媒体上传单文件大小上限(MB) |
| `channels.qqbot.mediaUploadCacheSize` | `channels.qqbot.media_upload_cache_size` | int | `500` | — | 媒体上传结果缓存条目上限 |
| `channels.qqbot.mediaParseTags` | `channels.qqbot.media_parse_tags` | bool | `true` | — | 是否解析消息中的媒体标签 |
| `channels.feishu.enabled` | `channels.feishu.enabled` | bool | `false` | — | 是否启用飞书通道 |
| `channels.feishu.appId` | `channels.feishu.app_id` | str | `""` | — | 飞书应用 App ID |
| `channels.feishu.appSecret` | `channels.feishu.app_secret` | str | `""` | — | 飞书应用 App Secret |
| `channels.feishu.verificationToken` | `channels.feishu.verification_token` | str | `""` | — | 飞书事件回调验证 token |
| `channels.feishu.encryptionKey` | `channels.feishu.encryption_key` | str | `""` | — | 飞书事件加密密钥 |
| `channels.feishu.webhookPath` | `channels.feishu.webhook_path` | str | `/feishu` | — | 飞书事件接收路径 |
| `channels.feishu.host` | `channels.feishu.host` | str | `0.0.0.0` | — | 飞书服务监听地址 |
| `channels.feishu.port` | `channels.feishu.port` | int | `8083` | — | 飞书服务监听端口 |
| `channels.dingtalk.enabled` | `channels.dingtalk.enabled` | bool | `false` | — | 是否启用钉钉通道 |
| `channels.dingtalk.appKey` | `channels.dingtalk.app_key` | str | `""` | — | 钉钉应用 AppKey |
| `channels.dingtalk.appSecret` | `channels.dingtalk.app_secret` | str | `""` | — | 钉钉应用 AppSecret |
| `channels.dingtalk.robotCode` | `channels.dingtalk.robot_code` | str | `""` | — | 钉钉机器人编码 |
| `channels.dingtalk.allowFrom` | `channels.dingtalk.allow_from` | list | `[]` | — | 允许与机器人交互的用户白名单(空为不限制) |
| `channels.email.enabled` | `channels.email.enabled` | bool | `false` | — | 是否启用邮件通道 |
| `channels.email.imapHost` | `channels.email.imap_host` | str | `""` | — | 收信 IMAP 服务器地址 |
| `channels.email.imapPort` | `channels.email.imap_port` | int | `993` | — | 收信 IMAP 服务器端口 |
| `channels.email.smtpHost` | `channels.email.smtp_host` | str | `""` | — | 发信 SMTP 服务器地址 |
| `channels.email.smtpPort` | `channels.email.smtp_port` | int | `465` | — | 发信 SMTP 服务器端口 |
| `channels.email.username` | `channels.email.username` | str | `""` | — | 邮箱登录用户名 |
| `channels.email.password` | `channels.email.password` | str | `""` | — | 邮箱登录密码或授权码 |
| `channels.email.useSsl` | `channels.email.use_ssl` | bool | `true` | — | 是否使用 SSL 连接邮件服务器 |
| `channels.email.pollIntervalSeconds` | `channels.email.poll_interval_seconds` | int | `30` | — | 轮询新邮件的间隔(秒) |
| `channels.email.allowFrom` | `channels.email.allow_from` | list | `[]` | — | 允许交互的发件人邮箱白名单(空为不限制) |
| `channels.wecom.enabled` | `channels.wecom.enabled` | bool | `false` | — | 是否启用企业微信通道 |
| `channels.wecom.corpId` | `channels.wecom.corp_id` | str | `""` | — | 企业微信企业 ID |
| `channels.wecom.agentId` | `channels.wecom.agent_id` | str | `""` | — | 企业微信应用 AgentId |
| `channels.wecom.secret` | `channels.wecom.secret` | str | `""` | — | 企业微信应用 Secret |
| `channels.wecom.token` | `channels.wecom.token` | str | `""` | — | 企业微信回调校验 token |
| `channels.wecom.encodingAesKey` | `channels.wecom.encoding_aes_key` | str | `""` | — | 企业微信加密回调的 EncodingAESKey,留空则为明文模式 |
| `channels.wecom.webhookPath` | `channels.wecom.webhook_path` | str | `/wecom` | — | 企业微信事件接收路径 |
| `channels.wecom.host` | `channels.wecom.host` | str | `0.0.0.0` | — | 企业微信服务监听地址 |
| `channels.wecom.port` | `channels.wecom.port` | int | `8084` | — | 企业微信服务监听端口 |
| `channels.matrix.enabled` | `channels.matrix.enabled` | bool | `false` | — | 是否启用 Matrix 通道 |
| `channels.matrix.homeserver` | `channels.matrix.homeserver` | str | `""` | — | Matrix homeserver 地址 |
| `channels.matrix.userId` | `channels.matrix.user_id` | str | `""` | — | Matrix 机器人用户 ID |
| `channels.matrix.accessToken` | `channels.matrix.access_token` | str | `""` | — | Matrix 访问令牌 |
| `channels.matrix.allowRooms` | `channels.matrix.allow_rooms` | list | `[]` | — | 允许响应的房间白名单(空为不限制) |
| `channels.matrix.reactionsEnabled` | `channels.matrix.reactions_enabled` | bool | `true` | — | 是否对消息添加表情回应 |
| `channels.sendProgress` | `channels.send_progress` | bool | `true` | — | 是否向用户推送处理进度提示 |
| `channels.sendToolHints` | `channels.send_tool_hints` | bool | `true` | — | 是否推送工具调用提示 |
| `channels.streamChannels` | `channels.stream_channels` | list | `['cli', 'telegram', 'discord', 'slack', 'gateway:*']` | — | 启用流式增量回复的通道列表 |
| `channels.streamFlushChars` | `channels.stream_flush_chars` | int | `180` | — | 流式回复累计多少字符后推送一段 |
| `channels.streamFlushIntervalMs` | `channels.stream_flush_interval_ms` | int | `1500` | — | 流式回复推送的最大时间间隔(毫秒) |
| `channels.streamParagraphMode` | `channels.stream_paragraph_mode` | bool | `true` | — | 是否按段落边界切分流式推送 |
| `channels.transcriptionApiKey` | `channels.transcription_api_key` | str | `""` | — | 语音转写服务的 API key |

## models

| 字段 | snake | type | default | choices | 说明 |
|---|---|---|---|---|---|
| `models.defaultModel` | `models.default_model` | str | `""` | — | 无匹配路由时使用的默认模型 |
| `models.providers` | `models.providers` | list | `[]` | — | 模型提供商配置列表 |
| `models.providers[].name` | `models.providers[].name` | str | `""` | — | 提供商名称(路由引用此名) |
| `models.providers[].apiKey` | `models.providers[].api_key` | str | `""` | — | 提供商 API 密钥 |
| `models.providers[].apiBase` | `models.providers[].api_base` | str | `""` | — | 提供商 API 基础地址 |
| `models.providers[].models` | `models.providers[].models` | list | `[]` | — | 该提供商支持的模型列表 |
| `models.providers[].extraHeaders` | `models.providers[].extra_headers` | dict | `{}` | — | 附加到请求的自定义 HTTP 头 |
| `models.providers[].maxRetries` | `models.providers[].max_retries` | int | `3` | — | 瞬时错误时的最大重试次数(指数退避) |
| `models.providers[].timeoutSeconds` | `models.providers[].timeout_seconds` | int | `120` | — | 单次请求超时(秒) |
| `models.providers[].rateLimitRpm` | `models.providers[].rate_limit_rpm` | int | `0` | — | 该提供商每分钟请求上限(0 为不限) |
| `models.providers[].credentialPool` | `models.providers[].credential_pool` | list | `[]` | — | 轮换使用的多个 API 密钥池 |
| `models.routes` | `models.routes` | list | `[]` | — | 任务到模型的路由规则列表 |
| `models.routes[].model` | `models.routes[].model` | str | `""` | — | 该路由使用的模型名 |
| `models.routes[].provider` | `models.routes[].provider` | str | `""` | — | 该路由绑定的提供商名 |
| `models.routes[].taskTypes` | `models.routes[].task_types` | list | `[]` | — | 命中此路由的任务类型列表 |
| `models.routes[].fallbackModels` | `models.routes[].fallback_models` | list | `[]` | — | 主模型失败时的回退模型列表 |
| `models.routes[].maxTokens` | `models.routes[].max_tokens` | int | `4096` | — | 该路由生成的最大 token 数 |
| `models.routes[].temperature` | `models.routes[].temperature` | float | `0.7` | — | 该路由的采样温度 |
| `models.fallbackModel` | `models.fallback_model` | str | `""` | — | 全局兜底模型 |

## tools

| 字段 | snake | type | default | choices | 说明 |
|---|---|---|---|---|---|
| `tools.profile` | `tools.profile` | Literal | `full` | minimal/messaging/coding/full | 工具集预设档位 |
| `tools.allow` | `tools.allow` | list | `[]` | — | 覆盖档位、显式允许的工具列表 |
| `tools.alsoAllow` | `tools.also_allow` | list | `[]` | — | 在档位基础上额外允许的工具 |
| `tools.deny` | `tools.deny` | list | `[]` | — | 显式禁用的工具列表 |
| `tools.exec.enabled` | `tools.exec.enabled` | bool | `true` | — | 是否启用 shell/进程执行工具 |
| `tools.exec.maxOutputChars` | `tools.exec.max_output_chars` | int | `16000` | — | 命令输出截断的最大字符数 |
| `tools.exec.host` | `tools.exec.host` | Literal | `sandbox` | auto/local/sandbox/container/remote | 命令执行所在的宿主环境 |
| `tools.exec.security` | `tools.exec.security` | Literal | `allowlist` | deny/allowlist/full | 命令执行安全模式 |
| `tools.exec.ask` | `tools.exec.ask` | Literal | `on_miss` | off/on_miss/always | 命令执行前的审批询问策略 |
| `tools.exec.safeBins` | `tools.exec.safe_bins` | list | `['awk', 'cat', 'date', 'echo', 'find', 'grep', 'head', 'ls', 'pwd', 'rg', 'sed', 'sort', 'tail', 'tr', 'uniq', 'wc']` | — | allowlist 模式下免审批直接放行的安全命令 |
| `tools.exec.allowedCommands` | `tools.exec.allowed_commands` | list | `[]` | — | 额外允许执行的命令白名单 |
| `tools.exec.blockedCommands` | `tools.exec.blocked_commands` | list | `[]` | — | 禁止执行的命令黑名单 |
| `tools.web.enabled` | `tools.web.enabled` | bool | `false` | — | 是否启用网络访问工具 |
| `tools.web.proxy` | `tools.web.proxy` | str | None | `null` | — | 网络访问使用的代理地址 |
| `tools.web.timeoutSeconds` | `tools.web.timeout_seconds` | int | `30` | — | 网络请求超时(秒) |
| `tools.web.searchApiKey` | `tools.web.search_api_key` | str | `""` | — | 搜索服务 API key |
| `tools.web.searchProvider` | `tools.web.search_provider` | Literal | `brave` | brave/tavily/serpapi/searxng | 网络搜索服务提供商 |
| `tools.web.searchApiBase` | `tools.web.search_api_base` | str | `""` | — | 搜索服务 API 基础地址 |
| `tools.web.allowPrivateAddresses` | `tools.web.allow_private_addresses` | bool | `false` | — | 是否允许 web_fetch 访问私有/回环地址(SSRF 风险) |
| `tools.restrictToWorkspace` | `tools.restrict_to_workspace` | bool | `false` | — | 是否将文件操作限制在工作区内 |
| `tools.safeWriteRoot` | `tools.safe_write_root` | str | `""` | — | 允许写入的根目录 |
| `tools.inboundDocumentEnabled` | `tools.inbound_document_enabled` | bool | `true` | — | 是否自动下载、解密并解析入站文档附件(docx/xlsx/pptx/pdf) |
| `tools.inboundDocumentMaxChars` | `tools.inbound_document_max_chars` | int | `8000` | — | 入站文档自动注入正文的字符上限,超出则注入摘要并提示用 read_document 读全文 |
| `tools.mcpServers` | `tools.mcp_servers` | dict | `{}` | — | MCP 服务配置(键为服务名) |
| `tools.mcpServers{}.command` | `tools.mcp_servers{}.command` | str | `""` | — | stdio 传输方式下启动 MCP 服务的命令 |
| `tools.mcpServers{}.args` | `tools.mcp_servers{}.args` | list | `[]` | — | 启动 MCP 服务命令的参数 |
| `tools.mcpServers{}.env` | `tools.mcp_servers{}.env` | dict | `{}` | — | MCP 服务进程的环境变量 |
| `tools.mcpServers{}.url` | `tools.mcp_servers{}.url` | str | `""` | — | HTTP 传输方式下 MCP 服务地址 |
| `tools.mcpServers{}.headers` | `tools.mcp_servers{}.headers` | dict | `{}` | — | HTTP 连接 MCP 服务的自定义头 |
| `tools.mcpServers{}.auth` | `tools.mcp_servers{}.auth` | str | `""` | — | MCP 服务的认证凭据 |
| `tools.mcpServers{}.enabled` | `tools.mcp_servers{}.enabled` | bool | `true` | — | 是否启用该 MCP 服务 |
| `tools.mcpServers{}.timeout` | `tools.mcp_servers{}.timeout` | int | `120` | — | MCP 调用超时(秒) |
| `tools.mcpServers{}.connectTimeout` | `tools.mcp_servers{}.connect_timeout` | int | `60` | — | 连接 MCP 服务的超时(秒) |
| `tools.mcpServers{}.toolsInclude` | `tools.mcp_servers{}.tools_include` | list | `[]` | — | 仅暴露的 MCP 工具白名单(空为全部) |
| `tools.mcpServers{}.toolsExclude` | `tools.mcp_servers{}.tools_exclude` | list | `[]` | — | 排除的 MCP 工具黑名单 |
| `tools.mcpSecurityPolicy` | `tools.mcp_security_policy` | Literal | `block` | warn/block | MCP 工具安全策略:warn 仅告警,block 拦截 |
| `tools.imageGen.backend` | `tools.image_gen.backend` | str | `openai` | — | 图像生成后端 |
| `tools.imageGen.apiKey` | `tools.image_gen.api_key` | str | `""` | — | OpenAI 兼容后端 API key |
| `tools.imageGen.apiBase` | `tools.image_gen.api_base` | str | `""` | — | OpenAI 兼容后端 API 基础地址 |
| `tools.imageGen.model` | `tools.image_gen.model` | str | `""` | — | 图像生成模型名 |
| `tools.imageGen.falKey` | `tools.image_gen.fal_key` | str | `""` | — | FAL.ai 后端访问密钥 |
| `tools.imageGen.falModel` | `tools.image_gen.fal_model` | str | `""` | — | FAL.ai 图像生成模型名 |
| `tools.tts.openaiApiKey` | `tools.tts.openai_api_key` | str | `""` | — | OpenAI TTS API key |
| `tools.tts.openaiApiBase` | `tools.tts.openai_api_base` | str | `""` | — | OpenAI TTS API 基础地址 |
| `tools.tts.model` | `tools.tts.model` | str | `""` | — | TTS 模型名 |
| `tools.tts.defaultBackend` | `tools.tts.default_backend` | str | `edge` | — | 默认语音合成后端 |
| `tools.tts.defaultVoice` | `tools.tts.default_voice` | str | `""` | — | 默认语音音色 |
| `tools.codeExec.enabled` | `tools.code_exec.enabled` | bool | `true` | — | 是否启用代码执行工具 |
| `tools.codeExec.timeoutSeconds` | `tools.code_exec.timeout_seconds` | int | `30` | — | 代码执行超时(秒) |
| `tools.codeExec.allowedLanguages` | `tools.code_exec.allowed_languages` | list | `['python', 'javascript', 'bash']` | — | 允许执行的代码语言列表 |

## execution

| 字段 | snake | type | default | choices | 说明 |
|---|---|---|---|---|---|
| `execution.defaultExecutor` | `execution.default_executor` | Literal | `sandbox` | local/sandbox/container/remote | 默认命令执行器类型 |
| `execution.sandboxRoot` | `execution.sandbox_root` | str | `/tmp/echo-agent-sandbox` | — | sandbox 执行器的根目录 |
| `execution.containerImage` | `execution.container_image` | str | `""` | — | container 执行器使用的镜像 |
| `execution.remoteHost` | `execution.remote_host` | str | `""` | — | remote 执行器的目标主机 |
| `execution.remoteUser` | `execution.remote_user` | str | `root` | — | remote 执行器登录用户名 |
| `execution.remoteKeyPath` | `execution.remote_key_path` | str | `""` | — | remote 执行器 SSH 私钥路径 |
| `execution.remoteStrictHostKey` | `execution.remote_strict_host_key` | Literal | `accept-new` | no/accept-new/yes | SSH 主机密钥严格校验策略 |
| `execution.remoteConnectTimeout` | `execution.remote_connect_timeout` | int | `10` | — | remote 执行器连接超时(秒) |
| `execution.networkPolicy` | `execution.network_policy` | Literal | `deny` | allow/deny/restricted | 执行环境的网络访问策略 |

## permissions

| 字段 | snake | type | default | choices | 说明 |
|---|---|---|---|---|---|
| `permissions.adminUsers` | `permissions.admin_users` | list | `[]` | — | 全局管理员用户列表 |
| `permissions.approval.requireApproval` | `permissions.approval.require_approval` | list | `['cronjob', 'dep_install', 'exec', 'execute_code', 'process', 'skill_install', 'skill_manage']` | — | 执行前必须审批的工具/动作列表 |
| `permissions.approval.autoApprove` | `permissions.approval.auto_approve` | list | `[]` | — | 自动批准的工具/动作列表 |
| `permissions.approval.autoDeny` | `permissions.approval.auto_deny` | list | `[]` | — | 自动拒绝的工具/动作列表 |
| `permissions.approval.defaultPolicy` | `permissions.approval.default_policy` | Literal | `approve` | approve/deny/ask | 未命中规则时的默认审批策略 |
| `permissions.approval.waitTimeoutSeconds` | `permissions.approval.wait_timeout_seconds` | int | `300` | — | 等待人工审批的超时(秒) |
| `permissions.approval.cliAutoApprove` | `permissions.approval.cli_auto_approve` | bool | `true` | — | CLI 通道是否自动批准 |
| `permissions.approval.trustedChannels` | `permissions.approval.trusted_channels` | list | `[]` | — | 免审批的可信通道列表 |
| `permissions.approval.mode` | `permissions.approval.mode` | Literal | `smart` | manual/smart/off | 审批模式:manual 全人工,smart 智能判定,off 关闭 |
| `permissions.approval.smartModel` | `permissions.approval.smart_model` | str | `""` | — | smart 模式判定审批所用模型 |
| `permissions.approval.unattendedPolicy` | `permissions.approval.unattended_policy` | Literal | `deny` | deny/allow_safe | 无人值守时的审批策略 |
| `permissions.elevated.enabled` | `permissions.elevated.enabled` | bool | `false` | — | 是否启用提权操作机制 |
| `permissions.elevated.allowFrom` | `permissions.elevated.allow_from` | dict | `{}` | — | 各通道允许提权的用户映射 |

## credentials

| 字段 | snake | type | default | choices | 说明 |
|---|---|---|---|---|---|
| `credentials.encryptionKeyEnv` | `credentials.encryption_key_env` | str | `ECHO_AGENT_CREDENTIAL_KEY` | — | 存放凭据加密密钥的环境变量名 |
| `credentials.requireEncryption` | `credentials.require_encryption` | bool | `true` | — | 是否强制要求凭据加密 |

## session

| 字段 | snake | type | default | choices | 说明 |
|---|---|---|---|---|---|
| `session.maxHistoryMessages` | `session.max_history_messages` | int | `500` | — | 单会话保留的最大历史消息数 |
| `session.expiryHours` | `session.expiry_hours` | int | `72` | — | 会话过期时间(小时) |
| `session.contextWindowTokens` | `session.context_window_tokens` | int | `65536` | — | 上下文窗口 token 上限 |
| `session.introductionEnabled` | `session.introduction_enabled` | bool | `true` | — | 是否在新会话发送自我介绍 |
| `session.introductionTemplate` | `session.introduction_template` | str | `""` | — | 自我介绍模板 |
| `session.historyImageTtlMinutes` | `session.history_image_ttl_minutes` | int | `30` | — | 历史图片保留时长(分钟) |
| `session.historyImageLimit` | `session.history_image_limit` | int | `4` | — | 历史中保留的最大图片数 |
| `session.historyImageSkipIfCurrent` | `session.history_image_skip_if_current` | bool | `true` | — | 当前轮已带图时是否跳过历史图片 |
| `session.groupSessionScope` | `session.group_session_scope` | Literal | `per_user` | per_user/shared | 群聊会话隔离策略:per_user 每人独立会话(默认,防群内串话),shared 整群共享一个会话 |

## memory

| 字段 | snake | type | default | choices | 说明 |
|---|---|---|---|---|---|
| `memory.enabled` | `memory.enabled` | bool | `true` | — | 是否启用认知记忆 |
| `memory.scopePolicy` | `memory.scope_policy` | Literal | `session` | legacy/session | 记忆作用域策略 |
| `memory.consolidationThreshold` | `memory.consolidation_threshold` | int | `20` | — | 触发记忆整合的条目阈值 |
| `memory.vectorEnabled` | `memory.vector_enabled` | bool | `true` | — | 是否启用向量检索记忆 |
| `memory.vectorDimensions` | `memory.vector_dimensions` | int | `1536` | — | 记忆向量维度 |
| `memory.maxUserMemories` | `memory.max_user_memories` | int | `1000` | — | 单用户记忆条目上限 |
| `memory.maxEnvMemories` | `memory.max_env_memories` | int | `500` | — | 环境记忆条目上限 |
| `memory.memoryNudgeInterval` | `memory.memory_nudge_interval` | int | `10` | — | 提示模型记录记忆的轮次间隔 |
| `memory.importanceDecayDays` | `memory.importance_decay_days` | float | `30.0` | — | 记忆重要性衰减周期(天) |
| `memory.snapshotEnabled` | `memory.snapshot_enabled` | bool | `true` | — | 是否启用记忆快照注入上下文 |
| `memory.contradictionDetection` | `memory.contradiction_detection` | bool | `true` | — | 是否启用记忆矛盾检测 |
| `memory.sleepConsolidation` | `memory.sleep_consolidation` | bool | `true` | — | 是否启用空闲期记忆整合 |
| `memory.archivalThreshold` | `memory.archival_threshold` | float | `0.05` | — | 记忆归档分数阈值,低于此值进入归档层 |
| `memory.forgetThreshold` | `memory.forget_threshold` | float | `0.01` | — | 记忆遗忘分数阈值,低于此值被遗忘 |
| `memory.maxWorkingMemory` | `memory.max_working_memory` | int | `20` | — | 工作记忆条目上限 |
| `memory.embeddingModel` | `memory.embedding_model` | str | `""` | — | 记忆向量化使用的嵌入模型 |
| `memory.embedTimeoutSeconds` | `memory.embed_timeout_seconds` | float | `1.5` | — | 查询向量化的单次超时(秒),超时降级为关键词检索 |
| `memory.contradictionScanOnStore` | `memory.contradiction_scan_on_store` | bool | `false` | — | 是否在写入记忆时即时扫描矛盾 |
| `memory.autoResolveContradictions` | `memory.auto_resolve_contradictions` | bool | `false` | — | 睡眠整合时自动消解同 key 矛盾(newest-wins),默认关闭只检测不消解 |

## knowledge

| 字段 | snake | type | default | choices | 说明 |
|---|---|---|---|---|---|
| `knowledge.enabled` | `knowledge.enabled` | bool | `true` | — | 是否启用知识库检索 |
| `knowledge.docsDir` | `knowledge.docs_dir` | str | `data/knowledge` | — | 知识库文档目录 |
| `knowledge.indexPath` | `knowledge.index_path` | str | `data/knowledge_index.json` | — | 知识库索引文件路径 |
| `knowledge.autoIndex` | `knowledge.auto_index` | bool | `true` | — | 是否自动索引文档目录 |
| `knowledge.chunkSize` | `knowledge.chunk_size` | int | `1200` | — | 文档切块大小(字符) |
| `knowledge.chunkOverlap` | `knowledge.chunk_overlap` | int | `120` | — | 相邻切块重叠大小(字符) |
| `knowledge.maxResults` | `knowledge.max_results` | int | `5` | — | 知识检索返回的最大结果数 |
| `knowledge.allowedExtensions` | `knowledge.allowed_extensions` | list | `['.md', '.txt', '.rst', '.json', '.yaml', '.yml', '.py']` | — | 允许索引的文档扩展名 |

## multiAgent

| 字段 | snake | type | default | choices | 说明 |
|---|---|---|---|---|---|
| `multiAgent.enabled` | `multi_agent.enabled` | bool | `true` | — | 是否启用多代理委派 |
| `multiAgent.maxDepth` | `multi_agent.max_depth` | int | `3` | — | 委派嵌套最大深度 |
| `multiAgent.maxParallelWorkers` | `multi_agent.max_parallel_workers` | int | `4` | — | 并行子代理数上限 |
| `multiAgent.maxIterations` | `multi_agent.max_iterations` | int | `12` | — | 子代理默认最大迭代数 |
| `multiAgent.auditPath` | `multi_agent.audit_path` | str | `data/delegation_audit.jsonl` | — | 委派审计日志路径 |
| `multiAgent.workerProfiles` | `multi_agent.worker_profiles` | list | `[]` | — | 子代理画像配置列表 |
| `multiAgent.workerProfiles[].id` | `multi_agent.worker_profiles[].id` | str | `""` | — | 子代理画像 ID |
| `multiAgent.workerProfiles[].name` | `multi_agent.worker_profiles[].name` | str | `""` | — | 子代理名称 |
| `multiAgent.workerProfiles[].description` | `multi_agent.worker_profiles[].description` | str | `""` | — | 子代理用途描述 |
| `multiAgent.workerProfiles[].instructions` | `multi_agent.worker_profiles[].instructions` | str | `""` | — | 子代理系统指令 |
| `multiAgent.workerProfiles[].defaultTools` | `multi_agent.worker_profiles[].default_tools` | list | `[]` | — | 子代理默认可用工具 |
| `multiAgent.workerProfiles[].model` | `multi_agent.worker_profiles[].model` | str | `""` | — | 子代理使用的模型 |
| `multiAgent.workerProfiles[].maxIterations` | `multi_agent.worker_profiles[].max_iterations` | int | `12` | — | 子代理单任务最大迭代数 |
| `multiAgent.workerProfiles[].maxTokens` | `multi_agent.worker_profiles[].max_tokens` | int | `4096` | — | 子代理生成最大 token 数 |
| `multiAgent.workerProfiles[].temperature` | `multi_agent.worker_profiles[].temperature` | float | `0.4` | — | 子代理采样温度 |

## scheduler

| 字段 | snake | type | default | choices | 说明 |
|---|---|---|---|---|---|
| `scheduler.enabled` | `scheduler.enabled` | bool | `true` | — | 是否启用任务调度器 |
| `scheduler.maxConcurrentJobs` | `scheduler.max_concurrent_jobs` | int | `10` | — | 并发任务数上限 |

## storage

| 字段 | snake | type | default | choices | 说明 |
|---|---|---|---|---|---|
| `storage.databasePath` | `storage.database_path` | str | `data/echo_agent.db` | — | SQLite 数据库文件路径 |
| `storage.sessionsDir` | `storage.sessions_dir` | str | `data/sessions` | — | 会话数据存储目录 |
| `storage.memoryDir` | `storage.memory_dir` | str | `data/memory` | — | 记忆数据存储目录 |
| `storage.logsDir` | `storage.logs_dir` | str | `data/logs` | — | 日志文件存储目录 |

## observability

| 字段 | snake | type | default | choices | 说明 |
|---|---|---|---|---|---|
| `observability.logLevel` | `observability.log_level` | str | `INFO` | — | 日志级别 |
| `observability.traceEnabled` | `observability.trace_enabled` | bool | `true` | — | 是否记录执行轨迹(关闭则不写 trace 文件) |
| `observability.maxTraceFiles` | `observability.max_trace_files` | int | `500` | — | trace 文件保留数量上限,超出按最旧优先轮转删除;<=0 表示不限制(禁用轮转) |
| `observability.healthCheckIntervalSeconds` | `observability.health_check_interval_seconds` | int | `60` | — | 健康检查间隔(秒) |
| `observability.otelEnabled` | `observability.otel_enabled` | bool | `true` | — | 是否启用 OpenTelemetry 指标导出 |
| `observability.otelEndpoint` | `observability.otel_endpoint` | str | `""` | — | OpenTelemetry 导出端点 |
| `observability.otelServiceName` | `observability.otel_service_name` | str | `echo-agent` | — | OpenTelemetry 服务名 |
| `observability.otelExportIntervalMs` | `observability.otel_export_interval_ms` | int | `5000` | — | OpenTelemetry 指标导出间隔(毫秒) |

## skills

| 字段 | snake | type | default | choices | 说明 |
|---|---|---|---|---|---|
| `skills.skillsDir` | `skills.skills_dir` | str | `skills` | — | 技能脚本目录 |
| `skills.creationNudgeInterval` | `skills.creation_nudge_interval` | int | `10` | — | 提示创建技能的轮次间隔 |
| `skills.disabled` | `skills.disabled` | list | `[]` | — | 禁用的技能列表 |
| `skills.externalDirs` | `skills.external_dirs` | list | `[]` | — | 额外加载技能的外部目录 |
| `skills.allowLazyInstalls` | `skills.allow_lazy_installs` | bool | `true` | — | 是否允许技能运行时按需安装依赖 |

## compression

| 字段 | snake | type | default | choices | 说明 |
|---|---|---|---|---|---|
| `compression.enabled` | `compression.enabled` | bool | `true` | — | 是否启用上下文压缩 |
| `compression.triggerRatio` | `compression.trigger_ratio` | float | `0.7` | — | 上下文占用达到该比例时触发压缩 |
| `compression.tailBudgetRatio` | `compression.tail_budget_ratio` | float | `0.4` | — | 压缩后保留尾部消息的预算比例 |
| `compression.headProtectCount` | `compression.head_protect_count` | int | `3` | — | 压缩时保护不动的头部消息数 |
| `compression.summaryTargetRatio` | `compression.summary_target_ratio` | float | `0.2` | — | 摘要相对原文的目标长度比例 |
| `compression.summaryMinTokens` | `compression.summary_min_tokens` | int | `2000` | — | 摘要最小 token 数 |
| `compression.summaryMaxTokens` | `compression.summary_max_tokens` | int | `12000` | — | 摘要最大 token 数 |
| `compression.summaryModel` | `compression.summary_model` | str | `""` | — | 生成摘要使用的模型 |
| `compression.summaryCooldownSeconds` | `compression.summary_cooldown_seconds` | int | `600` | — | 两次压缩之间的冷却时间(秒) |
| `compression.toolPruningEnabled` | `compression.tool_pruning_enabled` | bool | `true` | — | 是否启用工具结果剪枝 |
| `compression.toolPruningTailBudgetRatio` | `compression.tool_pruning_tail_budget_ratio` | float | `0.3` | — | 工具结果剪枝保留尾部的预算比例 |
| `compression.maxCompressionCount` | `compression.max_compression_count` | int | `10` | — | 单会话最大压缩次数 |

## gateway

| 字段 | snake | type | default | choices | 说明 |
|---|---|---|---|---|---|
| `gateway.enabled` | `gateway.enabled` | bool | `false` | — | 是否启用网关服务 |
| `gateway.host` | `gateway.host` | str | `0.0.0.0` | — | 网关监听地址 |
| `gateway.port` | `gateway.port` | int | `9000` | — | 网关监听端口 |
| `gateway.apiPrefix` | `gateway.api_prefix` | str | `/api/v1` | — | 网关 API 路径前缀 |
| `gateway.wsPath` | `gateway.ws_path` | str | `/ws` | — | 网关 WebSocket 路径 |
| `gateway.sessionPolicy.mode` | `gateway.session_policy.mode` | Literal | `idle` | daily/idle/both/none | 网关会话重置策略 |
| `gateway.sessionPolicy.dailyResetHour` | `gateway.session_policy.daily_reset_hour` | int | `4` | — | 每日重置会话的小时(0-23) |
| `gateway.sessionPolicy.idleTimeoutMinutes` | `gateway.session_policy.idle_timeout_minutes` | int | `1440` | — | 会话空闲超时(分钟) |
| `gateway.auth.mode` | `gateway.auth.mode` | Literal | `allowlist` | open/allowlist/pairing | 网关鉴权模式 |
| `gateway.auth.allowedUsers` | `gateway.auth.allowed_users` | list | `[]` | — | 允许访问网关的用户白名单 |
| `gateway.auth.adminUsers` | `gateway.auth.admin_users` | list | `[]` | — | 网关管理员用户列表 |
| `gateway.auth.apiTokens` | `gateway.auth.api_tokens` | list | `[]` | — | 网关 API 访问令牌列表 |
| `gateway.auth.adminTokens` | `gateway.auth.admin_tokens` | list | `[]` | — | 高危管理接口(关停/技能导入安装删除/知识库上传删除)专用令牌；为空时回退到 api_tokens |
| `gateway.auth.allowedOrigins` | `gateway.auth.allowed_origins` | list | `[]` | — | 浏览器 Origin 白名单(CSRF 防护)；留空则不启用 CSRF 检查(默认),配置后仅放行白名单内的跨站浏览器请求,非浏览器客户端始终不受影响 |
| `gateway.auth.tokenHeader` | `gateway.auth.token_header` | str | `X-Echo-Agent-Token` | — | 携带 API 令牌的请求头名 |
| `gateway.auth.pairingTtlSeconds` | `gateway.auth.pairing_ttl_seconds` | int | `300` | — | 配对模式令牌有效期(秒) |
| `gateway.platforms` | `gateway.platforms` | dict | `{}` | — | 各接入平台的网关配置(键为平台名) |
| `gateway.platforms{}.rateLimitRpm` | `gateway.platforms{}.rate_limit_rpm` | int | `30` | — | 该平台每分钟请求上限 |
| `gateway.mediaCacheDir` | `gateway.media_cache_dir` | str | `data/media_cache` | — | 网关媒体缓存目录 |
| `gateway.mediaCacheMaxMb` | `gateway.media_cache_max_mb` | int | `500` | — | 媒体缓存大小上限(MB) |
| `gateway.emitProgressEvents` | `gateway.emit_progress_events` | bool | `true` | — | 是否向网关客户端推送进度事件 |
| `gateway.progressDebug` | `gateway.progress_debug` | bool | `false` | — | 是否输出进度事件调试信息 |
| `gateway.hooksDir` | `gateway.hooks_dir` | str | `""` | — | 网关钩子脚本目录 |

## planning

| 字段 | snake | type | default | choices | 说明 |
|---|---|---|---|---|---|
| `planning.enabled` | `planning.enabled` | bool | `true` | — | 是否启用任务规划 |
| `planning.defaultStrategy` | `planning.default_strategy` | str | `auto` | — | 默认规划策略 |
| `planning.maxTreeDepth` | `planning.max_tree_depth` | int | `5` | — | 规划树最大深度 |
| `planning.maxBranches` | `planning.max_branches` | int | `3` | — | 思维树(ToT)策略探索的候选分支数 |
| `planning.reflectionEnabled` | `planning.reflection_enabled` | bool | `true` | — | 是否启用规划反思 |

## a2A

| 字段 | snake | type | default | choices | 说明 |
|---|---|---|---|---|---|
| `a2A.enabled` | `a2a.enabled` | bool | `true` | — | 是否启用 A2A(agent-to-agent)接口 |
| `a2A.agentName` | `a2a.agent_name` | str | `echo-agent` | — | 对外暴露的 A2A 代理名 |
| `a2A.agentDescription` | `a2a.agent_description` | str | `A modular AI agent framework` | — | 对外暴露的 A2A 代理描述 |
| `a2A.capabilities` | `a2a.capabilities` | list | `['chat', 'tool_use']` | — | A2A AgentCard 对外声明的能力标签 |

## evaluation

| 字段 | snake | type | default | choices | 说明 |
|---|---|---|---|---|---|
| `evaluation.datasetPath` | `evaluation.dataset_path` | str | `data/eval` | — | 评测数据集路径 |
| `evaluation.timeoutPerCase` | `evaluation.timeout_per_case` | int | `120` | — | 单条评测用例超时(秒) |

## bus

| 字段 | snake | type | default | choices | 说明 |
|---|---|---|---|---|---|
| `bus.maxQueueSize` | `bus.max_queue_size` | int | `1000` | — | 事件总线队列容量上限 |
| `bus.maxConcurrency` | `bus.max_concurrency` | int | `50` | — | 事件总线并发处理上限 |

## rateLimit

| 字段 | snake | type | default | choices | 说明 |
|---|---|---|---|---|---|
| `rateLimit.sessionRpm` | `rate_limit.session_rpm` | int | `20` | — | 单会话每分钟请求上限 |
| `rateLimit.sessionBurst` | `rate_limit.session_burst` | int | `5` | — | 单会话突发请求上限 |

## circuitBreaker

| 字段 | snake | type | default | choices | 说明 |
|---|---|---|---|---|---|
| `circuitBreaker.failureThreshold` | `circuit_breaker.failure_threshold` | int | `5` | — | 触发熔断的连续失败次数 |
| `circuitBreaker.recoverySeconds` | `circuit_breaker.recovery_seconds` | float | `60.0` | — | 熔断后尝试恢复的等待时间(秒) |
| `circuitBreaker.halfOpenMax` | `circuit_breaker.half_open_max` | int | `2` | — | 半开状态允许的试探请求数 |

## plugins

| 字段 | snake | type | default | choices | 说明 |
|---|---|---|---|---|---|
| `plugins.enabled` | `plugins.enabled` | bool | `true` | — | 是否启用插件系统 |
| `plugins.allow` | `plugins.allow` | list | `[]` | — | 允许加载的插件白名单 |
| `plugins.deny` | `plugins.deny` | list | `[]` | — | 禁止加载的插件黑名单 |
| `plugins.extraDirs` | `plugins.extra_dirs` | list | `[]` | — | 额外的插件搜索目录 |
| `plugins.config` | `plugins.config` | dict | `{}` | — | 各插件的自定义配置(键为插件名) |
| `plugins.trustedPlugins` | `plugins.trusted_plugins` | list | `[]` | — | 免权限校验的可信插件列表 |
| `plugins.permissionMode` | `plugins.permission_mode` | Literal | `compat` | compat/strict | 插件权限模式 |

## ui

| 字段 | snake | type | default | choices | 说明 |
|---|---|---|---|---|---|
| `ui.locale` | `ui.locale` | Literal | `auto` | en/zh/auto | 界面语言 |

## agent

| 字段 | snake | type | default | choices | 说明 |
|---|---|---|---|---|---|
| `agent.maxIterations` | `agent.max_iterations` | int | `40` | — | agent 主循环最大迭代数 |

## evolution

| 字段 | snake | type | default | choices | 说明 |
|---|---|---|---|---|---|
| `evolution.enabled` | `evolution.enabled` | bool | `false` | — | 是否启用自进化技能引擎 |
| `evolution.triggerMode` | `evolution.trigger_mode` | Literal | `manual` | manual/threshold/scheduled | 进化触发模式 |
| `evolution.thresholdTrajectories` | `evolution.threshold_trajectories` | int | `50` | — | threshold 模式触发所需轨迹数 |
| `evolution.cronExpression` | `evolution.cron_expression` | str | `0 4 * * *` | — | scheduled 模式的 cron 表达式 |
| `evolution.maxCandidatesPerRun` | `evolution.max_candidates_per_run` | int | `3` | — | 单次进化生成的候选上限 |
| `evolution.maxTrajectoriesPerRun` | `evolution.max_trajectories_per_run` | int | `200` | — | 单次进化处理的轨迹上限 |
| `evolution.evalDatasetPath` | `evolution.eval_dataset_path` | str | `data/eval/baseline.yaml` | — | 进化评测基线数据集路径 |
| `evolution.regressionThreshold` | `evolution.regression_threshold` | float | `0.05` | — | 判定回归的分数下降阈值 |
| `evolution.requireStrictImprovement` | `evolution.require_strict_improvement` | bool | `true` | — | 是否要求严格改进才晋升 |
| `evolution.minEvalCases` | `evolution.min_eval_cases` | int | `3` | — | 晋升所需的最小评测用例数,样本不足则判定不确定不晋升 |
| `evolution.recordTrajectories` | `evolution.record_trajectories` | bool | `true` | — | 是否记录执行轨迹用于进化 |
| `evolution.trajectoryRetentionDays` | `evolution.trajectory_retention_days` | int | `30` | — | 轨迹保留天数 |
| `evolution.evolverModel` | `evolution.evolver_model` | str | `""` | — | 执行进化所用模型 |
| `evolution.skillSizeLimitBytes` | `evolution.skill_size_limit_bytes` | int | `50000` | — | 进化产出技能的大小上限(字节) |
| `evolution.redactArgs` | `evolution.redact_args` | bool | `true` | — | 记录轨迹时是否脱敏工具参数 |
| `evolution.evalParallel` | `evolution.eval_parallel` | int | `2` | — | 进化评测并发度 |
| `evolution.evalTimeoutSeconds` | `evolution.eval_timeout_seconds` | int | `60` | — | 进化评测单用例超时(秒) |
| `evolution.cooldownSecondsAfterPromote` | `evolution.cooldown_seconds_after_promote` | int | `86400` | — | 晋升后再次进化的冷却时间(秒) |
| `evolution.autoPromote` | `evolution.auto_promote` | bool | `true` | — | 是否自动晋升通过评测的候选 |
| `evolution.candidateReviewRequired` | `evolution.candidate_review_required` | bool | `false` | — | 晋升前是否需要人工审查候选 |

## cost

| 字段 | snake | type | default | choices | 说明 |
|---|---|---|---|---|---|
| `cost.enabled` | `cost.enabled` | bool | `false` | — | 是否启用成本追踪与预算控制 |
| `cost.dailyBudgetUsd` | `cost.daily_budget_usd` | float | `0.0` | — | 每日成本预算(美元,0 为不限) |
| `cost.softThresholdRatio` | `cost.soft_threshold_ratio` | float | `0.8` | — | 达到预算该比例时发出软告警 |
| `cost.pricingOverrides` | `cost.pricing_overrides` | dict | `{}` | — | 模型定价覆盖表 |

## workspace

| 字段 | snake | type | default | choices | 说明 |
|---|---|---|---|---|---|
| `workspace` | `workspace` | str | `~/.echo-agent` | — | agent 工作区根目录 |

