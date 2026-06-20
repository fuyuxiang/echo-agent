# Echo Agent Configuration Reference

## security

| Field | snake | type | default | choices | description |
|---|---|---|---|---|---|
| `security.profile` | `security.profile` | Literal | `personal_cli` | personal_cli/daemon/public_gateway | Overall security profile preset |

## channels

| Field | snake | type | default | choices | description |
|---|---|---|---|---|---|
| `channels.telegram.enabled` | `channels.telegram.enabled` | bool | `false` | — | Enable the Telegram channel |
| `channels.telegram.token` | `channels.telegram.token` | str | `""` | — | Telegram bot API token |
| `channels.telegram.allowFrom` | `channels.telegram.allow_from` | list | `[]` | — | Allowlist of user IDs permitted to interact (empty = all) |
| `channels.telegram.proxy` | `channels.telegram.proxy` | str | None | `null` | — | Proxy URL used to reach the Telegram API |
| `channels.telegram.groupPolicy` | `channels.telegram.group_policy` | Literal | `mention` | open/mention | Group reply policy: open = all messages, mention = only when @-mentioned |
| `channels.telegram.reactionsEnabled` | `channels.telegram.reactions_enabled` | bool | `true` | — | Whether to add emoji reactions to messages |
| `channels.discord.enabled` | `channels.discord.enabled` | bool | `false` | — | Enable the Discord channel |
| `channels.discord.token` | `channels.discord.token` | str | `""` | — | Discord bot token |
| `channels.discord.allowFrom` | `channels.discord.allow_from` | list | `[]` | — | Allowlist of user IDs permitted to interact (empty = all) |
| `channels.discord.groupPolicy` | `channels.discord.group_policy` | Literal | `mention` | open/mention | Group reply policy: open = all messages, mention = only when @-mentioned |
| `channels.discord.reactionsEnabled` | `channels.discord.reactions_enabled` | bool | `true` | — | Whether to add emoji reactions to messages |
| `channels.webhook.enabled` | `channels.webhook.enabled` | bool | `false` | — | Enable the webhook channel |
| `channels.webhook.host` | `channels.webhook.host` | str | `0.0.0.0` | — | Webhook server bind address |
| `channels.webhook.port` | `channels.webhook.port` | int | `8080` | — | Webhook server listen port |
| `channels.webhook.secret` | `channels.webhook.secret` | str | `""` | — | Secret used to verify inbound request signatures |
| `channels.webhook.path` | `channels.webhook.path` | str | `/webhook` | — | HTTP path on which webhooks are received |
| `channels.webhook.maxPending` | `channels.webhook.max_pending` | int | `1000` | — | Maximum number of pending webhook requests queued |
| `channels.cli.enabled` | `channels.cli.enabled` | bool | `true` | — | Enable the CLI channel |
| `channels.cron.enabled` | `channels.cron.enabled` | bool | `false` | — | Enable the cron channel |
| `channels.slack.enabled` | `channels.slack.enabled` | bool | `false` | — | Enable the Slack channel |
| `channels.slack.botToken` | `channels.slack.bot_token` | str | `""` | — | Slack bot token (xoxb-) |
| `channels.slack.appToken` | `channels.slack.app_token` | str | `""` | — | Slack app-level token (xapp-) for Socket Mode |
| `channels.slack.allowFrom` | `channels.slack.allow_from` | list | `[]` | — | Allowlist of user IDs permitted to interact (empty = all) |
| `channels.slack.reactionsEnabled` | `channels.slack.reactions_enabled` | bool | `true` | — | Whether to add emoji reactions to messages |
| `channels.whatsapp.enabled` | `channels.whatsapp.enabled` | bool | `false` | — | Enable the WhatsApp channel |
| `channels.whatsapp.verifyToken` | `channels.whatsapp.verify_token` | str | `""` | — | WhatsApp webhook verification token |
| `channels.whatsapp.accessToken` | `channels.whatsapp.access_token` | str | `""` | — | WhatsApp Cloud API access token |
| `channels.whatsapp.phoneNumberId` | `channels.whatsapp.phone_number_id` | str | `""` | — | WhatsApp phone number ID used for sending |
| `channels.whatsapp.webhookPath` | `channels.whatsapp.webhook_path` | str | `/whatsapp` | — | HTTP path on which WhatsApp webhooks are received |
| `channels.whatsapp.host` | `channels.whatsapp.host` | str | `0.0.0.0` | — | WhatsApp server bind address |
| `channels.whatsapp.port` | `channels.whatsapp.port` | int | `8081` | — | WhatsApp server listen port |
| `channels.weixin.enabled` | `channels.weixin.enabled` | bool | `false` | — | Enable the Weixin channel |
| `channels.weixin.accountId` | `channels.weixin.account_id` | str | `""` | — | Weixin customer-service account ID |
| `channels.weixin.token` | `channels.weixin.token` | str | `""` | — | Weixin access authentication token |
| `channels.weixin.baseUrl` | `channels.weixin.base_url` | str | `https://ilinkai.weixin.qq.com` | — | Weixin API base URL |
| `channels.weixin.cdnBaseUrl` | `channels.weixin.cdn_base_url` | str | `https://novac2c.cdn.weixin.qq.com/c2c` | — | Weixin media CDN base URL |
| `channels.weixin.allowFrom` | `channels.weixin.allow_from` | list | `[]` | — | Allowlist of user IDs permitted to interact (empty = all) |
| `channels.weixin.dmPolicy` | `channels.weixin.dm_policy` | str | `open` | — | Direct-message reply policy |
| `channels.weixin.dataDir` | `channels.weixin.data_dir` | str | `""` | — | Local data directory for the Weixin channel |
| `channels.qqbot.enabled` | `channels.qqbot.enabled` | bool | `false` | — | Enable the QQ bot channel |
| `channels.qqbot.appId` | `channels.qqbot.app_id` | str | `""` | — | QQ bot AppID |
| `channels.qqbot.appSecret` | `channels.qqbot.app_secret` | str | `""` | — | QQ bot AppSecret |
| `channels.qqbot.allowFrom` | `channels.qqbot.allow_from` | list | `[]` | — | Allowlist of user IDs permitted to interact (empty = all) |
| `channels.qqbot.sandbox` | `channels.qqbot.sandbox` | bool | `false` | — | Use the QQ sandbox environment |
| `channels.qqbot.markdownSupport` | `channels.qqbot.markdown_support` | bool | `false` | — | Enable Markdown message support |
| `channels.qqbot.mediaEnabled` | `channels.qqbot.media_enabled` | bool | `true` | — | Enable media (image/file) sending and receiving |
| `channels.qqbot.mediaMaxFileSizeMb` | `channels.qqbot.media_max_file_size_mb` | int | `20` | — | Maximum size per uploaded media file (MB) |
| `channels.qqbot.mediaUploadCacheSize` | `channels.qqbot.media_upload_cache_size` | int | `500` | — | Maximum number of cached media upload results |
| `channels.qqbot.mediaParseTags` | `channels.qqbot.media_parse_tags` | bool | `true` | — | Parse media tags embedded in messages |
| `channels.feishu.enabled` | `channels.feishu.enabled` | bool | `false` | — | Enable the Feishu channel |
| `channels.feishu.appId` | `channels.feishu.app_id` | str | `""` | — | Feishu app ID |
| `channels.feishu.appSecret` | `channels.feishu.app_secret` | str | `""` | — | Feishu app secret |
| `channels.feishu.verificationToken` | `channels.feishu.verification_token` | str | `""` | — | Feishu event callback verification token |
| `channels.feishu.encryptionKey` | `channels.feishu.encryption_key` | str | `""` | — | Feishu event encryption key |
| `channels.feishu.webhookPath` | `channels.feishu.webhook_path` | str | `/feishu` | — | HTTP path on which Feishu events are received |
| `channels.feishu.host` | `channels.feishu.host` | str | `0.0.0.0` | — | Feishu server bind address |
| `channels.feishu.port` | `channels.feishu.port` | int | `8083` | — | Feishu server listen port |
| `channels.dingtalk.enabled` | `channels.dingtalk.enabled` | bool | `false` | — | Enable the DingTalk channel |
| `channels.dingtalk.appKey` | `channels.dingtalk.app_key` | str | `""` | — | DingTalk app key |
| `channels.dingtalk.appSecret` | `channels.dingtalk.app_secret` | str | `""` | — | DingTalk app secret |
| `channels.dingtalk.robotCode` | `channels.dingtalk.robot_code` | str | `""` | — | DingTalk robot code |
| `channels.dingtalk.allowFrom` | `channels.dingtalk.allow_from` | list | `[]` | — | Allowlist of user IDs permitted to interact (empty = all) |
| `channels.email.enabled` | `channels.email.enabled` | bool | `false` | — | Enable the email channel |
| `channels.email.imapHost` | `channels.email.imap_host` | str | `""` | — | IMAP server host for receiving mail |
| `channels.email.imapPort` | `channels.email.imap_port` | int | `993` | — | IMAP server port for receiving mail |
| `channels.email.smtpHost` | `channels.email.smtp_host` | str | `""` | — | SMTP server host for sending mail |
| `channels.email.smtpPort` | `channels.email.smtp_port` | int | `465` | — | SMTP server port for sending mail |
| `channels.email.username` | `channels.email.username` | str | `""` | — | Mailbox login username |
| `channels.email.password` | `channels.email.password` | str | `""` | — | Mailbox login password or app token |
| `channels.email.useSsl` | `channels.email.use_ssl` | bool | `true` | — | Use SSL when connecting to mail servers |
| `channels.email.pollIntervalSeconds` | `channels.email.poll_interval_seconds` | int | `30` | — | Interval between new-mail polls (seconds) |
| `channels.email.allowFrom` | `channels.email.allow_from` | list | `[]` | — | Allowlist of sender addresses permitted to interact (empty = all) |
| `channels.wecom.enabled` | `channels.wecom.enabled` | bool | `false` | — | Enable the WeCom channel |
| `channels.wecom.corpId` | `channels.wecom.corp_id` | str | `""` | — | WeCom corporation ID |
| `channels.wecom.agentId` | `channels.wecom.agent_id` | str | `""` | — | WeCom application AgentId |
| `channels.wecom.secret` | `channels.wecom.secret` | str | `""` | — | WeCom application secret |
| `channels.wecom.token` | `channels.wecom.token` | str | `""` | — | WeCom callback verification token |
| `channels.wecom.webhookPath` | `channels.wecom.webhook_path` | str | `/wecom` | — | HTTP path on which WeCom events are received |
| `channels.wecom.host` | `channels.wecom.host` | str | `0.0.0.0` | — | WeCom server bind address |
| `channels.wecom.port` | `channels.wecom.port` | int | `8084` | — | WeCom server listen port |
| `channels.matrix.enabled` | `channels.matrix.enabled` | bool | `false` | — | Enable the Matrix channel |
| `channels.matrix.homeserver` | `channels.matrix.homeserver` | str | `""` | — | Matrix homeserver URL |
| `channels.matrix.userId` | `channels.matrix.user_id` | str | `""` | — | Matrix bot user ID |
| `channels.matrix.accessToken` | `channels.matrix.access_token` | str | `""` | — | Matrix access token |
| `channels.matrix.allowRooms` | `channels.matrix.allow_rooms` | list | `[]` | — | Allowlist of room IDs the bot responds in (empty = all) |
| `channels.matrix.reactionsEnabled` | `channels.matrix.reactions_enabled` | bool | `true` | — | Whether to add emoji reactions to messages |
| `channels.sendProgress` | `channels.send_progress` | bool | `true` | — | Send progress updates to the user |
| `channels.sendToolHints` | `channels.send_tool_hints` | bool | `true` | — | Send tool-invocation hints to the user |
| `channels.streamChannels` | `channels.stream_channels` | list | `['cli', 'telegram', 'discord', 'slack', 'gateway:*']` | — | Channels for which streaming incremental replies are enabled |
| `channels.streamFlushChars` | `channels.stream_flush_chars` | int | `180` | — | Character count that triggers a streaming flush |
| `channels.streamFlushIntervalMs` | `channels.stream_flush_interval_ms` | int | `1500` | — | Maximum interval between streaming flushes (ms) |
| `channels.streamParagraphMode` | `channels.stream_paragraph_mode` | bool | `true` | — | Flush streaming output on paragraph boundaries |
| `channels.transcriptionApiKey` | `channels.transcription_api_key` | str | `""` | — | API key for the voice transcription service |

## models

| Field | snake | type | default | choices | description |
|---|---|---|---|---|---|
| `models.defaultModel` | `models.default_model` | str | `""` | — | Default model used when no route matches |
| `models.providers` | `models.providers` | list | `[]` | — | List of model provider configurations |
| `models.providers[].name` | `models.providers[].name` | str | `""` | — | Provider name referenced by routes |
| `models.providers[].apiKey` | `models.providers[].api_key` | str | `""` | — | Provider API key |
| `models.providers[].apiBase` | `models.providers[].api_base` | str | `""` | — | Provider API base URL |
| `models.providers[].models` | `models.providers[].models` | list | `[]` | — | Models served by this provider |
| `models.providers[].extraHeaders` | `models.providers[].extra_headers` | dict | `{}` | — | Extra HTTP headers attached to requests |
| `models.providers[].timeoutSeconds` | `models.providers[].timeout_seconds` | int | `120` | — | Per-request timeout (seconds) |
| `models.providers[].rateLimitRpm` | `models.providers[].rate_limit_rpm` | int | `0` | — | Provider request-per-minute cap (0 = unlimited) |
| `models.providers[].credentialPool` | `models.providers[].credential_pool` | list | `[]` | — | Pool of API keys rotated for this provider |
| `models.routes` | `models.routes` | list | `[]` | — | List of task-to-model routing rules |
| `models.routes[].model` | `models.routes[].model` | str | `""` | — | Model name used by this route |
| `models.routes[].provider` | `models.routes[].provider` | str | `""` | — | Provider name bound to this route |
| `models.routes[].taskTypes` | `models.routes[].task_types` | list | `[]` | — | Task types that match this route |
| `models.routes[].fallbackModels` | `models.routes[].fallback_models` | list | `[]` | — | Fallback models when the primary fails |
| `models.routes[].maxTokens` | `models.routes[].max_tokens` | int | `4096` | — | Maximum tokens generated for this route |
| `models.routes[].temperature` | `models.routes[].temperature` | float | `0.7` | — | Sampling temperature for this route |
| `models.fallbackModel` | `models.fallback_model` | str | `""` | — | Global fallback model |

## tools

| Field | snake | type | default | choices | description |
|---|---|---|---|---|---|
| `tools.profile` | `tools.profile` | Literal | `full` | minimal/messaging/coding/full | Preset tool profile |
| `tools.allow` | `tools.allow` | list | `[]` | — | Explicit allowlist of tools overriding the profile |
| `tools.alsoAllow` | `tools.also_allow` | list | `[]` | — | Tools additionally allowed on top of the profile |
| `tools.deny` | `tools.deny` | list | `[]` | — | Explicit blocklist of tools |
| `tools.exec.enabled` | `tools.exec.enabled` | bool | `true` | — | Enable the shell/process execution tool |
| `tools.exec.maxOutputChars` | `tools.exec.max_output_chars` | int | `16000` | — | Maximum characters of command output before truncation |
| `tools.exec.host` | `tools.exec.host` | Literal | `sandbox` | auto/local/sandbox/container/remote | Host environment in which commands execute |
| `tools.exec.security` | `tools.exec.security` | Literal | `allowlist` | deny/allowlist/full | Command execution security mode |
| `tools.exec.ask` | `tools.exec.ask` | Literal | `on_miss` | off/on_miss/always | When to ask for approval before running a command |
| `tools.exec.safeBins` | `tools.exec.safe_bins` | list | `['awk', 'cat', 'date', 'echo', 'find', 'grep', 'head', 'ls', 'pwd', 'rg', 'sed', 'sort', 'tail', 'tr', 'uniq', 'wc']` | — | Commands allowed without approval under allowlist mode |
| `tools.exec.allowedCommands` | `tools.exec.allowed_commands` | list | `[]` | — | Additional allowlist of commands permitted to run |
| `tools.exec.blockedCommands` | `tools.exec.blocked_commands` | list | `[]` | — | Blocklist of commands forbidden from running |
| `tools.web.enabled` | `tools.web.enabled` | bool | `false` | — | Enable the web access tool |
| `tools.web.proxy` | `tools.web.proxy` | str | None | `null` | — | Proxy URL used for web access |
| `tools.web.timeoutSeconds` | `tools.web.timeout_seconds` | int | `30` | — | Web request timeout (seconds) |
| `tools.web.searchApiKey` | `tools.web.search_api_key` | str | `""` | — | Search service API key |
| `tools.web.searchProvider` | `tools.web.search_provider` | Literal | `brave` | brave/tavily/serpapi/searxng | Web search service provider |
| `tools.web.searchApiBase` | `tools.web.search_api_base` | str | `""` | — | Search service API base URL |
| `tools.web.allowPrivateAddresses` | `tools.web.allow_private_addresses` | bool | `false` | — | Allow web_fetch to reach private/loopback addresses (SSRF risk) |
| `tools.restrictToWorkspace` | `tools.restrict_to_workspace` | bool | `false` | — | Restrict file operations to the workspace |
| `tools.safeWriteRoot` | `tools.safe_write_root` | str | `""` | — | Root directory under which writes are permitted |
| `tools.mcpServers` | `tools.mcp_servers` | dict | `{}` | — | MCP server configurations keyed by name |
| `tools.mcpServers{}.command` | `tools.mcp_servers{}.command` | str | `""` | — | Command launching the MCP server over stdio |
| `tools.mcpServers{}.args` | `tools.mcp_servers{}.args` | list | `[]` | — | Arguments for the MCP server launch command |
| `tools.mcpServers{}.env` | `tools.mcp_servers{}.env` | dict | `{}` | — | Environment variables for the MCP server process |
| `tools.mcpServers{}.url` | `tools.mcp_servers{}.url` | str | `""` | — | MCP server URL for HTTP transport |
| `tools.mcpServers{}.headers` | `tools.mcp_servers{}.headers` | dict | `{}` | — | Custom headers for the MCP HTTP connection |
| `tools.mcpServers{}.auth` | `tools.mcp_servers{}.auth` | str | `""` | — | Authentication credential for the MCP server |
| `tools.mcpServers{}.enabled` | `tools.mcp_servers{}.enabled` | bool | `true` | — | Enable this MCP server |
| `tools.mcpServers{}.timeout` | `tools.mcp_servers{}.timeout` | int | `120` | — | MCP call timeout (seconds) |
| `tools.mcpServers{}.connectTimeout` | `tools.mcp_servers{}.connect_timeout` | int | `60` | — | MCP server connection timeout (seconds) |
| `tools.mcpServers{}.toolsInclude` | `tools.mcp_servers{}.tools_include` | list | `[]` | — | Allowlist of MCP tools to expose (empty = all) |
| `tools.mcpServers{}.toolsExclude` | `tools.mcp_servers{}.tools_exclude` | list | `[]` | — | Blocklist of MCP tools to exclude |
| `tools.mcpSecurityPolicy` | `tools.mcp_security_policy` | Literal | `block` | warn/block | MCP tool security policy: warn or block |
| `tools.imageGen.backend` | `tools.image_gen.backend` | str | `openai` | — | Image generation backend |
| `tools.imageGen.apiKey` | `tools.image_gen.api_key` | str | `""` | — | OpenAI-compatible backend API key |
| `tools.imageGen.apiBase` | `tools.image_gen.api_base` | str | `""` | — | OpenAI-compatible backend API base URL |
| `tools.imageGen.model` | `tools.image_gen.model` | str | `""` | — | Image generation model name |
| `tools.imageGen.falKey` | `tools.image_gen.fal_key` | str | `""` | — | FAL.ai backend access key |
| `tools.imageGen.falModel` | `tools.image_gen.fal_model` | str | `""` | — | FAL.ai image generation model name |
| `tools.tts.openaiApiKey` | `tools.tts.openai_api_key` | str | `""` | — | OpenAI TTS API key |
| `tools.tts.openaiApiBase` | `tools.tts.openai_api_base` | str | `""` | — | OpenAI TTS API base URL |
| `tools.tts.model` | `tools.tts.model` | str | `""` | — | TTS model name |
| `tools.tts.defaultBackend` | `tools.tts.default_backend` | str | `edge` | — | Default text-to-speech backend |
| `tools.tts.defaultVoice` | `tools.tts.default_voice` | str | `""` | — | Default synthesis voice |
| `tools.codeExec.enabled` | `tools.code_exec.enabled` | bool | `true` | — | Enable the code execution tool |
| `tools.codeExec.timeoutSeconds` | `tools.code_exec.timeout_seconds` | int | `30` | — | Code execution timeout (seconds) |
| `tools.codeExec.allowedLanguages` | `tools.code_exec.allowed_languages` | list | `['python', 'javascript', 'bash']` | — | Languages permitted for code execution |

## execution

| Field | snake | type | default | choices | description |
|---|---|---|---|---|---|
| `execution.defaultExecutor` | `execution.default_executor` | Literal | `sandbox` | local/sandbox/container/remote | Default command executor type |
| `execution.sandboxRoot` | `execution.sandbox_root` | str | `/tmp/echo-agent-sandbox` | — | Root directory for the sandbox executor |
| `execution.containerImage` | `execution.container_image` | str | `""` | — | Image used by the container executor |
| `execution.remoteHost` | `execution.remote_host` | str | `""` | — | Target host for the remote executor |
| `execution.remoteUser` | `execution.remote_user` | str | `root` | — | Login user for the remote executor |
| `execution.remoteKeyPath` | `execution.remote_key_path` | str | `""` | — | SSH private key path for the remote executor |
| `execution.remoteStrictHostKey` | `execution.remote_strict_host_key` | Literal | `accept-new` | no/accept-new/yes | SSH strict host key checking policy |
| `execution.remoteConnectTimeout` | `execution.remote_connect_timeout` | int | `10` | — | Remote executor connection timeout (seconds) |
| `execution.networkPolicy` | `execution.network_policy` | Literal | `deny` | allow/deny/restricted | Network access policy for the execution environment |

## permissions

| Field | snake | type | default | choices | description |
|---|---|---|---|---|---|
| `permissions.adminUsers` | `permissions.admin_users` | list | `[]` | — | Global administrator users |
| `permissions.approval.requireApproval` | `permissions.approval.require_approval` | list | `['cronjob', 'exec', 'execute_code', 'process', 'skill_install', 'skill_manage']` | — | Tools/actions that require approval before running |
| `permissions.approval.autoApprove` | `permissions.approval.auto_approve` | list | `[]` | — | Tools/actions auto-approved without prompting |
| `permissions.approval.autoDeny` | `permissions.approval.auto_deny` | list | `[]` | — | Tools/actions auto-denied |
| `permissions.approval.defaultPolicy` | `permissions.approval.default_policy` | Literal | `approve` | approve/deny/ask | Default approval policy when no rule matches |
| `permissions.approval.waitTimeoutSeconds` | `permissions.approval.wait_timeout_seconds` | int | `300` | — | Timeout while waiting for human approval (seconds) |
| `permissions.approval.cliAutoApprove` | `permissions.approval.cli_auto_approve` | bool | `true` | — | Auto-approve actions on the CLI channel |
| `permissions.approval.trustedChannels` | `permissions.approval.trusted_channels` | list | `[]` | — | Trusted channels exempt from approval |
| `permissions.approval.mode` | `permissions.approval.mode` | Literal | `smart` | manual/smart/off | Approval mode: manual, smart, or off |
| `permissions.approval.smartModel` | `permissions.approval.smart_model` | str | `""` | — | Model used to judge approvals in smart mode |
| `permissions.approval.unattendedPolicy` | `permissions.approval.unattended_policy` | Literal | `deny` | deny/allow_safe | Approval policy when running unattended |
| `permissions.elevated.enabled` | `permissions.elevated.enabled` | bool | `false` | — | Enable the elevated-permission mechanism |
| `permissions.elevated.allowFrom` | `permissions.elevated.allow_from` | dict | `{}` | — | Per-channel mapping of users allowed to elevate |

## credentials

| Field | snake | type | default | choices | description |
|---|---|---|---|---|---|
| `credentials.encryptionKeyEnv` | `credentials.encryption_key_env` | str | `ECHO_AGENT_CREDENTIAL_KEY` | — | Environment variable holding the credential encryption key |
| `credentials.requireEncryption` | `credentials.require_encryption` | bool | `true` | — | Require credential encryption |

## session

| Field | snake | type | default | choices | description |
|---|---|---|---|---|---|
| `session.maxHistoryMessages` | `session.max_history_messages` | int | `500` | — | Maximum history messages retained per session |
| `session.expiryHours` | `session.expiry_hours` | int | `72` | — | Session expiry time (hours) |
| `session.contextWindowTokens` | `session.context_window_tokens` | int | `65536` | — | Context window token budget |
| `session.introductionEnabled` | `session.introduction_enabled` | bool | `true` | — | Send a self-introduction on new sessions |
| `session.introductionTemplate` | `session.introduction_template` | str | `""` | — | Self-introduction template |
| `session.historyImageTtlMinutes` | `session.history_image_ttl_minutes` | int | `30` | — | Time-to-live for images in history (minutes) |
| `session.historyImageLimit` | `session.history_image_limit` | int | `4` | — | Maximum images retained in history |
| `session.historyImageSkipIfCurrent` | `session.history_image_skip_if_current` | bool | `true` | — | Skip history images when the current turn already has one |

## memory

| Field | snake | type | default | choices | description |
|---|---|---|---|---|---|
| `memory.enabled` | `memory.enabled` | bool | `true` | — | Enable cognitive memory |
| `memory.scopePolicy` | `memory.scope_policy` | Literal | `session` | legacy/session | Memory scope policy |
| `memory.consolidationThreshold` | `memory.consolidation_threshold` | int | `20` | — | Entry threshold that triggers memory consolidation |
| `memory.vectorEnabled` | `memory.vector_enabled` | bool | `true` | — | Enable vector-based memory retrieval |
| `memory.vectorDimensions` | `memory.vector_dimensions` | int | `1536` | — | Memory embedding vector dimensions |
| `memory.maxUserMemories` | `memory.max_user_memories` | int | `1000` | — | Maximum stored memories per user |
| `memory.maxEnvMemories` | `memory.max_env_memories` | int | `500` | — | Maximum stored environment memories |
| `memory.memoryNudgeInterval` | `memory.memory_nudge_interval` | int | `10` | — | Turn interval for nudging the model to store memories |
| `memory.importanceDecayDays` | `memory.importance_decay_days` | float | `30.0` | — | Memory importance decay period (days) |
| `memory.snapshotEnabled` | `memory.snapshot_enabled` | bool | `true` | — | Inject memory snapshots into context |
| `memory.contradictionDetection` | `memory.contradiction_detection` | bool | `true` | — | Enable memory contradiction detection |
| `memory.sleepConsolidation` | `memory.sleep_consolidation` | bool | `true` | — | Enable idle-time (sleep) memory consolidation |
| `memory.archivalThreshold` | `memory.archival_threshold` | float | `0.05` | — | Archival score threshold; entries below it move to the archival tier |
| `memory.forgetThreshold` | `memory.forget_threshold` | float | `0.01` | — | Forget score threshold; entries below it are forgotten |
| `memory.maxWorkingMemory` | `memory.max_working_memory` | int | `20` | — | Maximum working-memory entries |
| `memory.embeddingModel` | `memory.embedding_model` | str | `""` | — | Embedding model used for memory vectorization |
| `memory.embedTimeoutSeconds` | `memory.embed_timeout_seconds` | float | `1.5` | — | Query-embedding timeout (seconds); falls back to keyword search on timeout |
| `memory.contradictionScanOnStore` | `memory.contradiction_scan_on_store` | bool | `false` | — | Scan for contradictions at memory store time |

## knowledge

| Field | snake | type | default | choices | description |
|---|---|---|---|---|---|
| `knowledge.enabled` | `knowledge.enabled` | bool | `true` | — | Enable knowledge-base retrieval |
| `knowledge.docsDir` | `knowledge.docs_dir` | str | `data/knowledge` | — | Knowledge base documents directory |
| `knowledge.indexPath` | `knowledge.index_path` | str | `data/knowledge_index.json` | — | Knowledge base index file path |
| `knowledge.autoIndex` | `knowledge.auto_index` | bool | `true` | — | Automatically index the documents directory |
| `knowledge.chunkSize` | `knowledge.chunk_size` | int | `1200` | — | Document chunk size (characters) |
| `knowledge.chunkOverlap` | `knowledge.chunk_overlap` | int | `120` | — | Overlap between adjacent chunks (characters) |
| `knowledge.maxResults` | `knowledge.max_results` | int | `5` | — | Maximum knowledge retrieval results returned |
| `knowledge.allowedExtensions` | `knowledge.allowed_extensions` | list | `['.md', '.txt', '.rst', '.json', '.yaml', '.yml', '.py']` | — | Document extensions eligible for indexing |

## multiAgent

| Field | snake | type | default | choices | description |
|---|---|---|---|---|---|
| `multiAgent.enabled` | `multi_agent.enabled` | bool | `true` | — | Enable multi-agent delegation |
| `multiAgent.maxDepth` | `multi_agent.max_depth` | int | `3` | — | Maximum delegation nesting depth |
| `multiAgent.maxParallelWorkers` | `multi_agent.max_parallel_workers` | int | `4` | — | Maximum parallel workers |
| `multiAgent.maxIterations` | `multi_agent.max_iterations` | int | `12` | — | Default maximum iterations per worker |
| `multiAgent.auditPath` | `multi_agent.audit_path` | str | `data/delegation_audit.jsonl` | — | Delegation audit log path |
| `multiAgent.workerProfiles` | `multi_agent.worker_profiles` | list | `[]` | — | List of worker profile configurations |
| `multiAgent.workerProfiles[].id` | `multi_agent.worker_profiles[].id` | str | `""` | — | Worker profile ID |
| `multiAgent.workerProfiles[].name` | `multi_agent.worker_profiles[].name` | str | `""` | — | Worker profile name |
| `multiAgent.workerProfiles[].description` | `multi_agent.worker_profiles[].description` | str | `""` | — | Worker profile description |
| `multiAgent.workerProfiles[].instructions` | `multi_agent.worker_profiles[].instructions` | str | `""` | — | Worker profile system instructions |
| `multiAgent.workerProfiles[].defaultTools` | `multi_agent.worker_profiles[].default_tools` | list | `[]` | — | Default tools available to the worker |
| `multiAgent.workerProfiles[].model` | `multi_agent.worker_profiles[].model` | str | `""` | — | Model used by the worker |
| `multiAgent.workerProfiles[].maxIterations` | `multi_agent.worker_profiles[].max_iterations` | int | `12` | — | Maximum iterations per worker task |
| `multiAgent.workerProfiles[].maxTokens` | `multi_agent.worker_profiles[].max_tokens` | int | `4096` | — | Maximum tokens generated by the worker |
| `multiAgent.workerProfiles[].temperature` | `multi_agent.worker_profiles[].temperature` | float | `0.4` | — | Worker sampling temperature |

## scheduler

| Field | snake | type | default | choices | description |
|---|---|---|---|---|---|
| `scheduler.enabled` | `scheduler.enabled` | bool | `true` | — | Enable the task scheduler |
| `scheduler.maxConcurrentJobs` | `scheduler.max_concurrent_jobs` | int | `10` | — | Maximum concurrent scheduled jobs |

## storage

| Field | snake | type | default | choices | description |
|---|---|---|---|---|---|
| `storage.databasePath` | `storage.database_path` | str | `data/echo_agent.db` | — | SQLite database file path |
| `storage.sessionsDir` | `storage.sessions_dir` | str | `data/sessions` | — | Directory storing session data |
| `storage.memoryDir` | `storage.memory_dir` | str | `data/memory` | — | Directory storing memory data |
| `storage.logsDir` | `storage.logs_dir` | str | `data/logs` | — | Directory storing log files |

## observability

| Field | snake | type | default | choices | description |
|---|---|---|---|---|---|
| `observability.logLevel` | `observability.log_level` | str | `INFO` | — | Logging level |
| `observability.healthCheckIntervalSeconds` | `observability.health_check_interval_seconds` | int | `60` | — | Health check interval (seconds) |
| `observability.otelEnabled` | `observability.otel_enabled` | bool | `true` | — | Enable OpenTelemetry metrics export |
| `observability.otelEndpoint` | `observability.otel_endpoint` | str | `""` | — | OpenTelemetry export endpoint |
| `observability.otelServiceName` | `observability.otel_service_name` | str | `echo-agent` | — | OpenTelemetry service name |
| `observability.otelExportIntervalMs` | `observability.otel_export_interval_ms` | int | `5000` | — | OpenTelemetry metrics export interval (ms) |

## skills

| Field | snake | type | default | choices | description |
|---|---|---|---|---|---|
| `skills.skillsDir` | `skills.skills_dir` | str | `skills` | — | Skills directory |
| `skills.creationNudgeInterval` | `skills.creation_nudge_interval` | int | `10` | — | Turn interval for nudging skill creation |
| `skills.disabled` | `skills.disabled` | list | `[]` | — | List of disabled skills |
| `skills.externalDirs` | `skills.external_dirs` | list | `[]` | — | External directories from which to load skills |
| `skills.allowLazyInstalls` | `skills.allow_lazy_installs` | bool | `true` | — | Allow lazy on-demand dependency installs for skills |

## compression

| Field | snake | type | default | choices | description |
|---|---|---|---|---|---|
| `compression.enabled` | `compression.enabled` | bool | `true` | — | Enable context compression |
| `compression.triggerRatio` | `compression.trigger_ratio` | float | `0.7` | — | Context usage ratio that triggers compression |
| `compression.tailBudgetRatio` | `compression.tail_budget_ratio` | float | `0.4` | — | Budget ratio reserved for tail messages after compression |
| `compression.headProtectCount` | `compression.head_protect_count` | int | `3` | — | Number of head messages protected from compression |
| `compression.summaryTargetRatio` | `compression.summary_target_ratio` | float | `0.2` | — | Target summary length relative to source |
| `compression.summaryMinTokens` | `compression.summary_min_tokens` | int | `2000` | — | Minimum summary tokens |
| `compression.summaryMaxTokens` | `compression.summary_max_tokens` | int | `12000` | — | Maximum summary tokens |
| `compression.summaryModel` | `compression.summary_model` | str | `""` | — | Model used to generate summaries |
| `compression.summaryCooldownSeconds` | `compression.summary_cooldown_seconds` | int | `600` | — | Cooldown between compressions (seconds) |
| `compression.toolPruningEnabled` | `compression.tool_pruning_enabled` | bool | `true` | — | Enable pruning of tool results |
| `compression.toolPruningTailBudgetRatio` | `compression.tool_pruning_tail_budget_ratio` | float | `0.3` | — | Tail budget ratio retained when pruning tool results |
| `compression.maxCompressionCount` | `compression.max_compression_count` | int | `10` | — | Maximum compressions per session |

## gateway

| Field | snake | type | default | choices | description |
|---|---|---|---|---|---|
| `gateway.enabled` | `gateway.enabled` | bool | `false` | — | Enable the gateway service |
| `gateway.host` | `gateway.host` | str | `0.0.0.0` | — | Gateway bind address |
| `gateway.port` | `gateway.port` | int | `9000` | — | Gateway listen port |
| `gateway.apiPrefix` | `gateway.api_prefix` | str | `/api/v1` | — | Gateway API path prefix |
| `gateway.wsPath` | `gateway.ws_path` | str | `/ws` | — | Gateway WebSocket path |
| `gateway.sessionPolicy.mode` | `gateway.session_policy.mode` | Literal | `idle` | daily/idle/both/none | Gateway session reset policy |
| `gateway.sessionPolicy.dailyResetHour` | `gateway.session_policy.daily_reset_hour` | int | `4` | — | Hour of day to reset sessions (0-23) |
| `gateway.sessionPolicy.idleTimeoutMinutes` | `gateway.session_policy.idle_timeout_minutes` | int | `1440` | — | Session idle timeout (minutes) |
| `gateway.auth.mode` | `gateway.auth.mode` | Literal | `allowlist` | open/allowlist/pairing | Gateway authentication mode |
| `gateway.auth.allowedUsers` | `gateway.auth.allowed_users` | list | `[]` | — | Allowlist of users permitted to access the gateway |
| `gateway.auth.adminUsers` | `gateway.auth.admin_users` | list | `[]` | — | Gateway administrator users |
| `gateway.auth.apiTokens` | `gateway.auth.api_tokens` | list | `[]` | — | Gateway API access tokens |
| `gateway.auth.tokenHeader` | `gateway.auth.token_header` | str | `X-Echo-Agent-Token` | — | Request header carrying the API token |
| `gateway.auth.pairingTtlSeconds` | `gateway.auth.pairing_ttl_seconds` | int | `300` | — | Pairing-mode token time-to-live (seconds) |
| `gateway.platforms` | `gateway.platforms` | dict | `{}` | — | Per-platform gateway configurations keyed by platform |
| `gateway.platforms{}.rateLimitRpm` | `gateway.platforms{}.rate_limit_rpm` | int | `30` | — | Per-minute request cap for this platform |
| `gateway.mediaCacheDir` | `gateway.media_cache_dir` | str | `data/media_cache` | — | Gateway media cache directory |
| `gateway.mediaCacheMaxMb` | `gateway.media_cache_max_mb` | int | `500` | — | Media cache size limit (MB) |
| `gateway.emitProgressEvents` | `gateway.emit_progress_events` | bool | `true` | — | Emit progress events to gateway clients |
| `gateway.progressDebug` | `gateway.progress_debug` | bool | `false` | — | Emit progress-event debug information |
| `gateway.hooksDir` | `gateway.hooks_dir` | str | `""` | — | Gateway hook scripts directory |

## planning

| Field | snake | type | default | choices | description |
|---|---|---|---|---|---|
| `planning.enabled` | `planning.enabled` | bool | `true` | — | Enable task planning |
| `planning.defaultStrategy` | `planning.default_strategy` | str | `auto` | — | Default planning strategy |
| `planning.maxTreeDepth` | `planning.max_tree_depth` | int | `5` | — | Maximum planning tree depth |
| `planning.reflectionEnabled` | `planning.reflection_enabled` | bool | `true` | — | Enable planning reflection |

## a2A

| Field | snake | type | default | choices | description |
|---|---|---|---|---|---|
| `a2A.enabled` | `a2a.enabled` | bool | `true` | — | Enable the A2A (agent-to-agent) interface |
| `a2A.agentName` | `a2a.agent_name` | str | `echo-agent` | — | Agent name exposed over A2A |
| `a2A.agentDescription` | `a2a.agent_description` | str | `A modular AI agent framework` | — | Agent description exposed over A2A |

## evaluation

| Field | snake | type | default | choices | description |
|---|---|---|---|---|---|
| `evaluation.datasetPath` | `evaluation.dataset_path` | str | `data/eval` | — | Evaluation dataset path |
| `evaluation.timeoutPerCase` | `evaluation.timeout_per_case` | int | `120` | — | Timeout per evaluation case (seconds) |

## bus

| Field | snake | type | default | choices | description |
|---|---|---|---|---|---|
| `bus.maxQueueSize` | `bus.max_queue_size` | int | `1000` | — | Event bus queue capacity |
| `bus.maxConcurrency` | `bus.max_concurrency` | int | `50` | — | Event bus max concurrent handlers |

## rateLimit

| Field | snake | type | default | choices | description |
|---|---|---|---|---|---|
| `rateLimit.sessionRpm` | `rate_limit.session_rpm` | int | `20` | — | Per-session requests-per-minute cap |
| `rateLimit.sessionBurst` | `rate_limit.session_burst` | int | `5` | — | Per-session burst allowance |

## circuitBreaker

| Field | snake | type | default | choices | description |
|---|---|---|---|---|---|
| `circuitBreaker.failureThreshold` | `circuit_breaker.failure_threshold` | int | `5` | — | Consecutive failures that trip the breaker |
| `circuitBreaker.recoverySeconds` | `circuit_breaker.recovery_seconds` | float | `60.0` | — | Wait before attempting recovery after tripping (seconds) |
| `circuitBreaker.halfOpenMax` | `circuit_breaker.half_open_max` | int | `2` | — | Probe requests allowed in half-open state |

## plugins

| Field | snake | type | default | choices | description |
|---|---|---|---|---|---|
| `plugins.enabled` | `plugins.enabled` | bool | `true` | — | Enable the plugin system |
| `plugins.allow` | `plugins.allow` | list | `[]` | — | Allowlist of plugins permitted to load |
| `plugins.deny` | `plugins.deny` | list | `[]` | — | Blocklist of plugins forbidden from loading |
| `plugins.extraDirs` | `plugins.extra_dirs` | list | `[]` | — | Additional plugin search directories |
| `plugins.config` | `plugins.config` | dict | `{}` | — | Per-plugin custom configuration keyed by plugin |
| `plugins.trustedPlugins` | `plugins.trusted_plugins` | list | `[]` | — | Trusted plugins exempt from permission checks |
| `plugins.permissionMode` | `plugins.permission_mode` | Literal | `compat` | compat/strict | Plugin permission mode |

## ui

| Field | snake | type | default | choices | description |
|---|---|---|---|---|---|
| `ui.locale` | `ui.locale` | Literal | `auto` | en/zh/auto | Interface language |

## agent

| Field | snake | type | default | choices | description |
|---|---|---|---|---|---|
| `agent.maxIterations` | `agent.max_iterations` | int | `40` | — | Maximum iterations of the agent main loop |

## evolution

| Field | snake | type | default | choices | description |
|---|---|---|---|---|---|
| `evolution.enabled` | `evolution.enabled` | bool | `false` | — | Enable the self-evolving skill engine |
| `evolution.triggerMode` | `evolution.trigger_mode` | Literal | `manual` | manual/threshold/scheduled | Evolution trigger mode |
| `evolution.thresholdTrajectories` | `evolution.threshold_trajectories` | int | `50` | — | Trajectory count triggering threshold mode |
| `evolution.cronExpression` | `evolution.cron_expression` | str | `0 4 * * *` | — | Cron expression for scheduled mode |
| `evolution.maxCandidatesPerRun` | `evolution.max_candidates_per_run` | int | `3` | — | Maximum candidates generated per run |
| `evolution.maxTrajectoriesPerRun` | `evolution.max_trajectories_per_run` | int | `200` | — | Maximum trajectories processed per run |
| `evolution.evalDatasetPath` | `evolution.eval_dataset_path` | str | `data/eval/baseline.yaml` | — | Evolution evaluation baseline dataset path |
| `evolution.regressionThreshold` | `evolution.regression_threshold` | float | `0.05` | — | Score-drop threshold that flags a regression |
| `evolution.requireStrictImprovement` | `evolution.require_strict_improvement` | bool | `true` | — | Require strict improvement before promotion |
| `evolution.recordTrajectories` | `evolution.record_trajectories` | bool | `true` | — | Record execution trajectories for evolution |
| `evolution.trajectoryRetentionDays` | `evolution.trajectory_retention_days` | int | `30` | — | Trajectory retention period (days) |
| `evolution.evolverModel` | `evolution.evolver_model` | str | `""` | — | Model used to perform evolution |
| `evolution.skillSizeLimitBytes` | `evolution.skill_size_limit_bytes` | int | `50000` | — | Size limit for evolved skills (bytes) |
| `evolution.redactArgs` | `evolution.redact_args` | bool | `true` | — | Redact tool arguments when recording trajectories |
| `evolution.evalParallel` | `evolution.eval_parallel` | int | `2` | — | Evolution evaluation parallelism |
| `evolution.evalTimeoutSeconds` | `evolution.eval_timeout_seconds` | int | `60` | — | Evolution evaluation per-case timeout (seconds) |
| `evolution.cooldownSecondsAfterPromote` | `evolution.cooldown_seconds_after_promote` | int | `86400` | — | Cooldown after a promotion before evolving again (seconds) |
| `evolution.autoPromote` | `evolution.auto_promote` | bool | `true` | — | Auto-promote candidates that pass evaluation |
| `evolution.candidateReviewRequired` | `evolution.candidate_review_required` | bool | `false` | — | Require human review of candidates before promotion |

## cost

| Field | snake | type | default | choices | description |
|---|---|---|---|---|---|
| `cost.enabled` | `cost.enabled` | bool | `false` | — | Enable cost tracking and budget control |
| `cost.dailyBudgetUsd` | `cost.daily_budget_usd` | float | `0.0` | — | Daily cost budget in USD (0 = unlimited) |
| `cost.softThresholdRatio` | `cost.soft_threshold_ratio` | float | `0.8` | — | Budget ratio at which a soft warning is raised |
| `cost.pricingOverrides` | `cost.pricing_overrides` | dict | `{}` | — | Model pricing override table |

## workspace

| Field | snake | type | default | choices | description |
|---|---|---|---|---|---|
| `workspace` | `workspace` | str | `~/.echo-agent` | — | Agent workspace root directory |

