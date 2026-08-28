# Environment Variables Reference

Echo Agent configuration can be overridden via environment variables. This is useful for containerized deployments, CI/CD pipelines, and secrets management.

---

## Naming Convention

All environment variables use the `ECHO_AGENT_` prefix with double underscores (`__`) for nesting.

### Mapping Rules

| Config Path (YAML) | Environment Variable |
|--------------------|--------------------|
| `gateway.port` | `ECHO_AGENT_GATEWAY__PORT` |
| `gateway.auth.mode` | `ECHO_AGENT_GATEWAY__AUTH__MODE` |
| `models.default.api_key` | `ECHO_AGENT_MODELS__DEFAULT__API_KEY` |
| `channels.telegram.token` | `ECHO_AGENT_CHANNELS__TELEGRAM__TOKEN` |
| `security.profile` | `ECHO_AGENT_SECURITY__PROFILE` |

### Pattern

```
ECHO_AGENT_<SECTION>__<SUBSECTION>__<FIELD>
```

- All letters are **UPPERCASE**
- Single underscores within field names stay as-is
- Nesting levels are separated by **double underscore** (`__`)

!!! tip "Quick reference"
    Take the YAML dotted path, replace dots with `__`, uppercase everything, prepend `ECHO_AGENT_`.

---

## Precedence

Environment variables sit in the middle of the configuration loading order:

```
Package defaults → User YAML → ECHO_AGENT_ env vars → CLI overrides → Profile defaults → Validation
```

Env vars override config file values but are overridden by explicit CLI flags.

A camelCase key in YAML (`networkPolicy`) and its snake_case form
(`network_policy`) are the same setting. Every source is normalized to the
snake_case field name before merging, so an env var reliably overrides a
camelCase key written by the setup wizard or by hand. Normalization happens in
memory on load and never rewrites your config file.

---

## Type Coercion

Environment variables are always strings. Whether a value is parsed as JSON is decided by the field's declared type in the schema — never by what the value looks like. Scalars stay strings and are coerced by pydantic during validation:

| Target Type | Env Value | Result |
|-------------|-----------|--------|
| `bool` | `true`, `1`, `yes`, `on` | `True` |
| `bool` | `false`, `0`, `no`, `off` | `False` |
| `int` | `"3000"` | `3000` |
| `float` | `"0.5"` | `0.5` |
| `list` | `'["item1","item2"]'` | `["item1", "item2"]` (JSON) |
| `dict` | `'{"key": "value"}'` | `{"key": "value"}` (JSON) |
| `str` | `"hello"` | `"hello"` |
| `str` | `"false"` | `"false"` (stays a string) |

Because the decision follows the declared type, a secret or token whose value happens to read `false`, `null` or `[]` is passed through verbatim to a `str` field rather than becoming a bool/None.

```bash
export ECHO_AGENT_GATEWAY__AUTH__ADMIN_TOKENS='["ephemeral-token"]'
export ECHO_AGENT_TOOLS__DENY='["shell", "process"]'
export ECHO_AGENT_CHANNELS__TELEGRAM__TOKEN=false   # the string "false"
```

Mapping-of-submodel fields (`tools.mcp_servers`, `gateway.platforms`) accept either a whole JSON object or a per-key override addressed as `<field>__<key>__<subfield>`:

```bash
export ECHO_AGENT_TOOLS__MCP_SERVERS__MYSRV__ARGS='["-m", "myserver"]'
```

!!! warning "List syntax"
    Lists must use JSON array syntax. Comma-separated values are not split — they reach validation as a single string and fail. Malformed JSON is left as the raw string so pydantic names the offending field.

!!! note "Keys containing double underscores"
    A user-chosen key containing `__` collides with the level separator, so the path cannot be resolved and the value is treated as a string. Configure these in YAML instead.

---

## Provider Credentials

A provider API key can be written into the config, discovered from a conventional
variable name (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, ...), or read from a
variable you name yourself with `apiKeyEnv` — useful when a host process injects
an ephemeral secret and you do not want it persisted to disk:

```yaml
models:
  providers:
    - name: openai
      apiKeyEnv: MY_HOST_INJECTED_KEY
```

Resolution order is `apiKey` (explicit) > `apiKeyEnv` > the conventional variable
name for that provider. With none of them set, behaviour is unchanged: the
provider either reports a missing key or allows keyless access.

---

## Variable Reference

### Core

| Variable | Type | Default | Description |
|----------|------|---------|-------------|
| `ECHO_AGENT_STORAGE__BASE_DIR` | str | `~/.echo-agent` | Base directory for all data |
| `ECHO_AGENT_RUNTIME__WORKERS` | int | `4` | Number of async worker tasks |
| `ECHO_AGENT_RUNTIME__MAX_TURNS` | int | `50` | Maximum agent turns per request |
| `ECHO_AGENT_RUNTIME__TIMEOUT` | int | `300` | Request timeout in seconds |

### Security

| Variable | Type | Default | Description |
|----------|------|---------|-------------|
| `ECHO_AGENT_SECURITY__PROFILE` | str | `standard` | Security profile: `minimal`, `standard`, `extended` |
| `ECHO_AGENT_TOOLS__PROFILE` | str | `messaging` | Tool profile: `minimal`, `messaging`, `coding`, `full` |
| `ECHO_AGENT_PERMISSIONS__REQUIRE_APPROVAL` | bool | `true` | Require approval for high-risk tools |

### Gateway
| Variable | Type | Default | Description |
|----------|------|---------|-------------|

| Config Path | Environment Variable | Type | Default |
|-------------|---------------------|------|---------|
| `security.profile` | `ECHO_AGENT_SECURITY__PROFILE` | _LiteralGenericAlias | 'personal_cli' |
| `channels.telegram.enabled` | `ECHO_AGENT_CHANNELS__TELEGRAM__ENABLED` | type | False |
| `channels.telegram.token` | `ECHO_AGENT_CHANNELS__TELEGRAM__TOKEN` | type | '' |
| `channels.telegram.allow_from` | `ECHO_AGENT_CHANNELS__TELEGRAM__ALLOW_FROM` | GenericAlias | PydanticUndefined |
| `channels.telegram.proxy` | `ECHO_AGENT_CHANNELS__TELEGRAM__PROXY` | UnionType | None |
| `channels.telegram.group_policy` | `ECHO_AGENT_CHANNELS__TELEGRAM__GROUP_POLICY` | _LiteralGenericAlias | 'mention' |
| `channels.telegram.reactions_enabled` | `ECHO_AGENT_CHANNELS__TELEGRAM__REACTIONS_ENABLED` | type | True |
| `channels.telegram.data_dir` | `ECHO_AGENT_CHANNELS__TELEGRAM__DATA_DIR` | type | '' |
| `channels.discord.enabled` | `ECHO_AGENT_CHANNELS__DISCORD__ENABLED` | type | False |
| `channels.discord.token` | `ECHO_AGENT_CHANNELS__DISCORD__TOKEN` | type | '' |
| `channels.discord.allow_from` | `ECHO_AGENT_CHANNELS__DISCORD__ALLOW_FROM` | GenericAlias | PydanticUndefined |
| `channels.discord.group_policy` | `ECHO_AGENT_CHANNELS__DISCORD__GROUP_POLICY` | _LiteralGenericAlias | 'mention' |
| `channels.discord.reactions_enabled` | `ECHO_AGENT_CHANNELS__DISCORD__REACTIONS_ENABLED` | type | True |
| `channels.webhook.enabled` | `ECHO_AGENT_CHANNELS__WEBHOOK__ENABLED` | type | False |
| `channels.webhook.host` | `ECHO_AGENT_CHANNELS__WEBHOOK__HOST` | type | '0.0.0.0' |
| `channels.webhook.port` | `ECHO_AGENT_CHANNELS__WEBHOOK__PORT` | type | 8080 |
| `channels.webhook.secret` | `ECHO_AGENT_CHANNELS__WEBHOOK__SECRET` | type | '' |
| `channels.webhook.path` | `ECHO_AGENT_CHANNELS__WEBHOOK__PATH` | type | '/webhook' |
| `channels.webhook.max_pending` | `ECHO_AGENT_CHANNELS__WEBHOOK__MAX_PENDING` | type | 1000 |
| `channels.cli.enabled` | `ECHO_AGENT_CHANNELS__CLI__ENABLED` | type | True |
| `channels.cron.enabled` | `ECHO_AGENT_CHANNELS__CRON__ENABLED` | type | False |
| `channels.slack.enabled` | `ECHO_AGENT_CHANNELS__SLACK__ENABLED` | type | False |
| `channels.slack.bot_token` | `ECHO_AGENT_CHANNELS__SLACK__BOT_TOKEN` | type | '' |
| `channels.slack.app_token` | `ECHO_AGENT_CHANNELS__SLACK__APP_TOKEN` | type | '' |
| `channels.slack.allow_from` | `ECHO_AGENT_CHANNELS__SLACK__ALLOW_FROM` | GenericAlias | PydanticUndefined |
| `channels.slack.reactions_enabled` | `ECHO_AGENT_CHANNELS__SLACK__REACTIONS_ENABLED` | type | True |
| `channels.whatsapp.enabled` | `ECHO_AGENT_CHANNELS__WHATSAPP__ENABLED` | type | False |
| `channels.whatsapp.verify_token` | `ECHO_AGENT_CHANNELS__WHATSAPP__VERIFY_TOKEN` | type | '' |
| `channels.whatsapp.access_token` | `ECHO_AGENT_CHANNELS__WHATSAPP__ACCESS_TOKEN` | type | '' |
| `channels.whatsapp.phone_number_id` | `ECHO_AGENT_CHANNELS__WHATSAPP__PHONE_NUMBER_ID` | type | '' |
| `channels.whatsapp.webhook_path` | `ECHO_AGENT_CHANNELS__WHATSAPP__WEBHOOK_PATH` | type | '/whatsapp' |
| `channels.whatsapp.host` | `ECHO_AGENT_CHANNELS__WHATSAPP__HOST` | type | '0.0.0.0' |
| `channels.whatsapp.port` | `ECHO_AGENT_CHANNELS__WHATSAPP__PORT` | type | 8081 |
| `channels.whatsapp.app_secret` | `ECHO_AGENT_CHANNELS__WHATSAPP__APP_SECRET` | type | '' |
| `channels.whatsapp.allow_from` | `ECHO_AGENT_CHANNELS__WHATSAPP__ALLOW_FROM` | GenericAlias | PydanticUndefined |
| `channels.whatsapp.group_policy` | `ECHO_AGENT_CHANNELS__WHATSAPP__GROUP_POLICY` | type | 'mention' |
| `channels.weixin.enabled` | `ECHO_AGENT_CHANNELS__WEIXIN__ENABLED` | type | False |
| `channels.weixin.account_id` | `ECHO_AGENT_CHANNELS__WEIXIN__ACCOUNT_ID` | type | '' |
| `channels.weixin.token` | `ECHO_AGENT_CHANNELS__WEIXIN__TOKEN` | type | '' |
| `channels.weixin.base_url` | `ECHO_AGENT_CHANNELS__WEIXIN__BASE_URL` | type | 'https://ilinkai.weixin.qq.com' |
| `channels.weixin.cdn_base_url` | `ECHO_AGENT_CHANNELS__WEIXIN__CDN_BASE_URL` | type | 'https://novac2c.cdn.weixin.qq.com/c2c' |
| `channels.weixin.allow_from` | `ECHO_AGENT_CHANNELS__WEIXIN__ALLOW_FROM` | GenericAlias | PydanticUndefined |
| `channels.weixin.dm_policy` | `ECHO_AGENT_CHANNELS__WEIXIN__DM_POLICY` | type | 'open' |
| `channels.weixin.data_dir` | `ECHO_AGENT_CHANNELS__WEIXIN__DATA_DIR` | type | '' |
| `channels.weixin.typing_indicator` | `ECHO_AGENT_CHANNELS__WEIXIN__TYPING_INDICATOR` | type | True |
| `channels.qqbot.enabled` | `ECHO_AGENT_CHANNELS__QQBOT__ENABLED` | type | False |
| `channels.qqbot.app_id` | `ECHO_AGENT_CHANNELS__QQBOT__APP_ID` | type | '' |
| `channels.qqbot.app_secret` | `ECHO_AGENT_CHANNELS__QQBOT__APP_SECRET` | type | '' |
| `channels.qqbot.allow_from` | `ECHO_AGENT_CHANNELS__QQBOT__ALLOW_FROM` | GenericAlias | PydanticUndefined |
| `channels.qqbot.sandbox` | `ECHO_AGENT_CHANNELS__QQBOT__SANDBOX` | type | False |
| `channels.qqbot.markdown_support` | `ECHO_AGENT_CHANNELS__QQBOT__MARKDOWN_SUPPORT` | type | True |
| `channels.qqbot.media_enabled` | `ECHO_AGENT_CHANNELS__QQBOT__MEDIA_ENABLED` | type | True |
| `channels.qqbot.media_max_file_size_mb` | `ECHO_AGENT_CHANNELS__QQBOT__MEDIA_MAX_FILE_SIZE_MB` | type | 20 |
| `channels.qqbot.media_upload_cache_size` | `ECHO_AGENT_CHANNELS__QQBOT__MEDIA_UPLOAD_CACHE_SIZE` | type | 500 |
| `channels.qqbot.media_parse_tags` | `ECHO_AGENT_CHANNELS__QQBOT__MEDIA_PARSE_TAGS` | type | True |
| `channels.feishu.enabled` | `ECHO_AGENT_CHANNELS__FEISHU__ENABLED` | type | False |
| `channels.feishu.app_id` | `ECHO_AGENT_CHANNELS__FEISHU__APP_ID` | type | '' |
| `channels.feishu.app_secret` | `ECHO_AGENT_CHANNELS__FEISHU__APP_SECRET` | type | '' |
| `channels.feishu.verification_token` | `ECHO_AGENT_CHANNELS__FEISHU__VERIFICATION_TOKEN` | type | '' |
| `channels.feishu.encryption_key` | `ECHO_AGENT_CHANNELS__FEISHU__ENCRYPTION_KEY` | type | '' |
| `channels.feishu.webhook_path` | `ECHO_AGENT_CHANNELS__FEISHU__WEBHOOK_PATH` | type | '/feishu' |
| `channels.feishu.host` | `ECHO_AGENT_CHANNELS__FEISHU__HOST` | type | '0.0.0.0' |
| `channels.feishu.port` | `ECHO_AGENT_CHANNELS__FEISHU__PORT` | type | 8083 |
| `channels.feishu.group_policy` | `ECHO_AGENT_CHANNELS__FEISHU__GROUP_POLICY` | type | 'mention' |
| `channels.feishu.bot_open_id` | `ECHO_AGENT_CHANNELS__FEISHU__BOT_OPEN_ID` | type | '' |
| `channels.dingtalk.enabled` | `ECHO_AGENT_CHANNELS__DINGTALK__ENABLED` | type | False |
| `channels.dingtalk.app_key` | `ECHO_AGENT_CHANNELS__DINGTALK__APP_KEY` | type | '' |
| `channels.dingtalk.app_secret` | `ECHO_AGENT_CHANNELS__DINGTALK__APP_SECRET` | type | '' |
| `channels.dingtalk.robot_code` | `ECHO_AGENT_CHANNELS__DINGTALK__ROBOT_CODE` | type | '' |
| `channels.dingtalk.allow_from` | `ECHO_AGENT_CHANNELS__DINGTALK__ALLOW_FROM` | GenericAlias | PydanticUndefined |
| `channels.email.enabled` | `ECHO_AGENT_CHANNELS__EMAIL__ENABLED` | type | False |
| `channels.email.imap_host` | `ECHO_AGENT_CHANNELS__EMAIL__IMAP_HOST` | type | '' |
| `channels.email.imap_port` | `ECHO_AGENT_CHANNELS__EMAIL__IMAP_PORT` | type | 993 |
| `channels.email.smtp_host` | `ECHO_AGENT_CHANNELS__EMAIL__SMTP_HOST` | type | '' |
| `channels.email.smtp_port` | `ECHO_AGENT_CHANNELS__EMAIL__SMTP_PORT` | type | 465 |
| `channels.email.username` | `ECHO_AGENT_CHANNELS__EMAIL__USERNAME` | type | '' |
| `channels.email.password` | `ECHO_AGENT_CHANNELS__EMAIL__PASSWORD` | type | '' |
| `channels.email.use_ssl` | `ECHO_AGENT_CHANNELS__EMAIL__USE_SSL` | type | True |
| `channels.email.poll_interval_seconds` | `ECHO_AGENT_CHANNELS__EMAIL__POLL_INTERVAL_SECONDS` | type | 30 |
| `channels.email.allow_from` | `ECHO_AGENT_CHANNELS__EMAIL__ALLOW_FROM` | GenericAlias | PydanticUndefined |
| `channels.wecom.enabled` | `ECHO_AGENT_CHANNELS__WECOM__ENABLED` | type | False |
| `channels.wecom.corp_id` | `ECHO_AGENT_CHANNELS__WECOM__CORP_ID` | type | '' |
| `channels.wecom.agent_id` | `ECHO_AGENT_CHANNELS__WECOM__AGENT_ID` | type | '' |
| `channels.wecom.secret` | `ECHO_AGENT_CHANNELS__WECOM__SECRET` | type | '' |
| `channels.wecom.token` | `ECHO_AGENT_CHANNELS__WECOM__TOKEN` | type | '' |
| `channels.wecom.encoding_aes_key` | `ECHO_AGENT_CHANNELS__WECOM__ENCODING_AES_KEY` | type | '' |
| `channels.wecom.webhook_path` | `ECHO_AGENT_CHANNELS__WECOM__WEBHOOK_PATH` | type | '/wecom' |
| `channels.wecom.host` | `ECHO_AGENT_CHANNELS__WECOM__HOST` | type | '0.0.0.0' |
| `channels.wecom.port` | `ECHO_AGENT_CHANNELS__WECOM__PORT` | type | 8084 |
| `channels.matrix.enabled` | `ECHO_AGENT_CHANNELS__MATRIX__ENABLED` | type | False |
| `channels.matrix.homeserver` | `ECHO_AGENT_CHANNELS__MATRIX__HOMESERVER` | type | '' |
| `channels.matrix.user_id` | `ECHO_AGENT_CHANNELS__MATRIX__USER_ID` | type | '' |
| `channels.matrix.access_token` | `ECHO_AGENT_CHANNELS__MATRIX__ACCESS_TOKEN` | type | '' |
| `channels.matrix.allow_rooms` | `ECHO_AGENT_CHANNELS__MATRIX__ALLOW_ROOMS` | GenericAlias | PydanticUndefined |
| `channels.matrix.reactions_enabled` | `ECHO_AGENT_CHANNELS__MATRIX__REACTIONS_ENABLED` | type | True |
| `channels.send_progress` | `ECHO_AGENT_CHANNELS__SEND_PROGRESS` | type | True |
| `channels.send_tool_hints` | `ECHO_AGENT_CHANNELS__SEND_TOOL_HINTS` | type | True |
| `channels.stream_channels` | `ECHO_AGENT_CHANNELS__STREAM_CHANNELS` | GenericAlias | PydanticUndefined |
| `channels.stream_flush_chars` | `ECHO_AGENT_CHANNELS__STREAM_FLUSH_CHARS` | type | 180 |
| `channels.stream_flush_interval_ms` | `ECHO_AGENT_CHANNELS__STREAM_FLUSH_INTERVAL_MS` | type | 1500 |
| `channels.stream_paragraph_mode` | `ECHO_AGENT_CHANNELS__STREAM_PARAGRAPH_MODE` | type | True |
| `channels.stream_local_flush_chars` | `ECHO_AGENT_CHANNELS__STREAM_LOCAL_FLUSH_CHARS` | type | 24 |
| `channels.stream_local_flush_interval_ms` | `ECHO_AGENT_CHANNELS__STREAM_LOCAL_FLUSH_INTERVAL_MS` | type | 100 |
| `channels.stream_local_channels` | `ECHO_AGENT_CHANNELS__STREAM_LOCAL_CHANNELS` | GenericAlias | PydanticUndefined |
| `channels.stream_optimistic_channels` | `ECHO_AGENT_CHANNELS__STREAM_OPTIMISTIC_CHANNELS` | GenericAlias | PydanticUndefined |
| `channels.transcription_api_key` | `ECHO_AGENT_CHANNELS__TRANSCRIPTION_API_KEY` | type | '' |
| `models.default_model` | `ECHO_AGENT_MODELS__DEFAULT_MODEL` | type | '' |
| `models.providers` | `ECHO_AGENT_MODELS__PROVIDERS` | GenericAlias | PydanticUndefined |
| `models.routes` | `ECHO_AGENT_MODELS__ROUTES` | GenericAlias | PydanticUndefined |
| `models.fallback_model` | `ECHO_AGENT_MODELS__FALLBACK_MODEL` | type | '' |
| `models.model_windows` | `ECHO_AGENT_MODELS__MODEL_WINDOWS` | GenericAlias | PydanticUndefined |
| `tools.profile` | `ECHO_AGENT_TOOLS__PROFILE` | _LiteralGenericAlias | 'full' |
| `tools.allow` | `ECHO_AGENT_TOOLS__ALLOW` | GenericAlias | PydanticUndefined |
| `tools.also_allow` | `ECHO_AGENT_TOOLS__ALSO_ALLOW` | GenericAlias | PydanticUndefined |
| `tools.deny` | `ECHO_AGENT_TOOLS__DENY` | GenericAlias | PydanticUndefined |
| `tools.exec.enabled` | `ECHO_AGENT_TOOLS__EXEC__ENABLED` | type | True |
| `tools.exec.max_output_chars` | `ECHO_AGENT_TOOLS__EXEC__MAX_OUTPUT_CHARS` | type | 2000000 |
| `tools.exec.host` | `ECHO_AGENT_TOOLS__EXEC__HOST` | _LiteralGenericAlias | 'sandbox' |
| `tools.exec.security` | `ECHO_AGENT_TOOLS__EXEC__SECURITY` | _LiteralGenericAlias | 'allowlist' |
| `tools.exec.ask` | `ECHO_AGENT_TOOLS__EXEC__ASK` | _LiteralGenericAlias | 'on_miss' |
| `tools.exec.safe_bins` | `ECHO_AGENT_TOOLS__EXEC__SAFE_BINS` | GenericAlias | PydanticUndefined |
| `tools.exec.allowed_commands` | `ECHO_AGENT_TOOLS__EXEC__ALLOWED_COMMANDS` | GenericAlias | PydanticUndefined |
| `tools.exec.blocked_commands` | `ECHO_AGENT_TOOLS__EXEC__BLOCKED_COMMANDS` | GenericAlias | PydanticUndefined |
| `tools.web.enabled` | `ECHO_AGENT_TOOLS__WEB__ENABLED` | type | False |
| `tools.web.proxy` | `ECHO_AGENT_TOOLS__WEB__PROXY` | UnionType | None |
| `tools.web.timeout_seconds` | `ECHO_AGENT_TOOLS__WEB__TIMEOUT_SECONDS` | type | 30 |
| `tools.web.search_api_key` | `ECHO_AGENT_TOOLS__WEB__SEARCH_API_KEY` | type | '' |
| `tools.web.search_provider` | `ECHO_AGENT_TOOLS__WEB__SEARCH_PROVIDER` | _LiteralGenericAlias | 'brave' |
| `tools.web.search_api_base` | `ECHO_AGENT_TOOLS__WEB__SEARCH_API_BASE` | type | '' |
| `tools.web.allow_private_addresses` | `ECHO_AGENT_TOOLS__WEB__ALLOW_PRIVATE_ADDRESSES` | type | False |
| `tools.browser.enabled` | `ECHO_AGENT_TOOLS__BROWSER__ENABLED` | type | True |
| `tools.browser.max_sessions` | `ECHO_AGENT_TOOLS__BROWSER__MAX_SESSIONS` | type | 3 |
| `tools.browser.max_total_sessions` | `ECHO_AGENT_TOOLS__BROWSER__MAX_TOTAL_SESSIONS` | type | 10 |
| `tools.browser.session_idle_timeout_sec` | `ECHO_AGENT_TOOLS__BROWSER__SESSION_IDLE_TIMEOUT_SEC` | type | 300 |
| `tools.browser.max_snapshot_chars` | `ECHO_AGENT_TOOLS__BROWSER__MAX_SNAPSHOT_CHARS` | type | 8000 |
| `tools.browser.headless` | `ECHO_AGENT_TOOLS__BROWSER__HEADLESS` | type | True |
| `tools.browser.nav_timeout_sec` | `ECHO_AGENT_TOOLS__BROWSER__NAV_TIMEOUT_SEC` | type | 30 |
| `tools.browser.allow_private_addresses` | `ECHO_AGENT_TOOLS__BROWSER__ALLOW_PRIVATE_ADDRESSES` | type | False |
| `tools.browser.dialog_policy` | `ECHO_AGENT_TOOLS__BROWSER__DIALOG_POLICY` | type | 'dismiss' |
| `tools.browser.allow_evaluate` | `ECHO_AGENT_TOOLS__BROWSER__ALLOW_EVALUATE` | type | True |
| `tools.browser.allow_unsafe_evaluate` | `ECHO_AGENT_TOOLS__BROWSER__ALLOW_UNSAFE_EVALUATE` | type | False |
| `tools.browser.persist_login_state` | `ECHO_AGENT_TOOLS__BROWSER__PERSIST_LOGIN_STATE` | type | False |
| `tools.browser.viewport_width` | `ECHO_AGENT_TOOLS__BROWSER__VIEWPORT_WIDTH` | type | 1280 |
| `tools.browser.viewport_height` | `ECHO_AGENT_TOOLS__BROWSER__VIEWPORT_HEIGHT` | type | 800 |
| `tools.browser.user_agent` | `ECHO_AGENT_TOOLS__BROWSER__USER_AGENT` | type | '' |
| `tools.restrict_to_workspace` | `ECHO_AGENT_TOOLS__RESTRICT_TO_WORKSPACE` | type | False |
| `tools.safe_write_root` | `ECHO_AGENT_TOOLS__SAFE_WRITE_ROOT` | type | '' |
| `tools.inbound_document_enabled` | `ECHO_AGENT_TOOLS__INBOUND_DOCUMENT_ENABLED` | type | True |
| `tools.inbound_document_max_chars` | `ECHO_AGENT_TOOLS__INBOUND_DOCUMENT_MAX_CHARS` | type | 8000 |
| `tools.mcp_servers` | `ECHO_AGENT_TOOLS__MCP_SERVERS` | GenericAlias | PydanticUndefined |
| `tools.mcp_security_policy` | `ECHO_AGENT_TOOLS__MCP_SECURITY_POLICY` | _LiteralGenericAlias | 'block' |
| `tools.image_gen.enabled` | `ECHO_AGENT_TOOLS__IMAGE_GEN__ENABLED` | type | True |
| `tools.image_gen.backend` | `ECHO_AGENT_TOOLS__IMAGE_GEN__BACKEND` | type | 'openai' |
| `tools.image_gen.api_key` | `ECHO_AGENT_TOOLS__IMAGE_GEN__API_KEY` | type | '' |
| `tools.image_gen.api_base` | `ECHO_AGENT_TOOLS__IMAGE_GEN__API_BASE` | type | '' |
| `tools.image_gen.model` | `ECHO_AGENT_TOOLS__IMAGE_GEN__MODEL` | type | '' |
| `tools.image_gen.fal_key` | `ECHO_AGENT_TOOLS__IMAGE_GEN__FAL_KEY` | type | '' |
| `tools.image_gen.fal_model` | `ECHO_AGENT_TOOLS__IMAGE_GEN__FAL_MODEL` | type | '' |
| `tools.tts.enabled` | `ECHO_AGENT_TOOLS__TTS__ENABLED` | type | True |
| `tools.tts.openai_api_key` | `ECHO_AGENT_TOOLS__TTS__OPENAI_API_KEY` | type | '' |
| `tools.tts.openai_api_base` | `ECHO_AGENT_TOOLS__TTS__OPENAI_API_BASE` | type | '' |
| `tools.tts.model` | `ECHO_AGENT_TOOLS__TTS__MODEL` | type | '' |
| `tools.tts.default_backend` | `ECHO_AGENT_TOOLS__TTS__DEFAULT_BACKEND` | type | 'edge' |
| `tools.tts.default_voice` | `ECHO_AGENT_TOOLS__TTS__DEFAULT_VOICE` | type | '' |
| `tools.code_exec.enabled` | `ECHO_AGENT_TOOLS__CODE_EXEC__ENABLED` | type | True |
| `tools.code_exec.timeout_seconds` | `ECHO_AGENT_TOOLS__CODE_EXEC__TIMEOUT_SECONDS` | type | 30 |
| `tools.code_exec.allowed_languages` | `ECHO_AGENT_TOOLS__CODE_EXEC__ALLOWED_LANGUAGES` | GenericAlias | PydanticUndefined |
| `tools.mcp.enabled` | `ECHO_AGENT_TOOLS__MCP__ENABLED` | type | True |
| `execution.default_executor` | `ECHO_AGENT_EXECUTION__DEFAULT_EXECUTOR` | _LiteralGenericAlias | 'sandbox' |
| `execution.sandbox_root` | `ECHO_AGENT_EXECUTION__SANDBOX_ROOT` | type | '/tmp/echo-agent-sandbox' |
| `execution.container_image` | `ECHO_AGENT_EXECUTION__CONTAINER_IMAGE` | type | '' |
| `execution.remote_host` | `ECHO_AGENT_EXECUTION__REMOTE_HOST` | type | '' |
| `execution.remote_user` | `ECHO_AGENT_EXECUTION__REMOTE_USER` | type | 'root' |
| `execution.remote_key_path` | `ECHO_AGENT_EXECUTION__REMOTE_KEY_PATH` | type | '' |
| `execution.remote_strict_host_key` | `ECHO_AGENT_EXECUTION__REMOTE_STRICT_HOST_KEY` | _LiteralGenericAlias | 'accept-new' |
| `execution.remote_connect_timeout` | `ECHO_AGENT_EXECUTION__REMOTE_CONNECT_TIMEOUT` | type | 10 |
| `execution.network_policy` | `ECHO_AGENT_EXECUTION__NETWORK_POLICY` | _LiteralGenericAlias | 'deny' |
| `execution.max_background_tasks` | `ECHO_AGENT_EXECUTION__MAX_BACKGROUND_TASKS` | type | 64 |
| `permissions.admin_users` | `ECHO_AGENT_PERMISSIONS__ADMIN_USERS` | GenericAlias | PydanticUndefined |
| `permissions.approval.require_approval` | `ECHO_AGENT_PERMISSIONS__APPROVAL__REQUIRE_APPROVAL` | GenericAlias | PydanticUndefined |
| `permissions.approval.auto_approve` | `ECHO_AGENT_PERMISSIONS__APPROVAL__AUTO_APPROVE` | GenericAlias | PydanticUndefined |
| `permissions.approval.auto_deny` | `ECHO_AGENT_PERMISSIONS__APPROVAL__AUTO_DENY` | GenericAlias | PydanticUndefined |
| `permissions.approval.default_policy` | `ECHO_AGENT_PERMISSIONS__APPROVAL__DEFAULT_POLICY` | _LiteralGenericAlias | 'approve' |
| `permissions.approval.wait_timeout_seconds` | `ECHO_AGENT_PERMISSIONS__APPROVAL__WAIT_TIMEOUT_SECONDS` | type | 300 |
| `permissions.approval.cli_auto_approve` | `ECHO_AGENT_PERMISSIONS__APPROVAL__CLI_AUTO_APPROVE` | type | True |
| `permissions.approval.trusted_channels` | `ECHO_AGENT_PERMISSIONS__APPROVAL__TRUSTED_CHANNELS` | GenericAlias | PydanticUndefined |
| `permissions.approval.mode` | `ECHO_AGENT_PERMISSIONS__APPROVAL__MODE` | _LiteralGenericAlias | 'smart' |
| `permissions.approval.smart_model` | `ECHO_AGENT_PERMISSIONS__APPROVAL__SMART_MODEL` | type | '' |
| `permissions.approval.unattended_policy` | `ECHO_AGENT_PERMISSIONS__APPROVAL__UNATTENDED_POLICY` | _LiteralGenericAlias | 'deny' |
| `permissions.elevated.enabled` | `ECHO_AGENT_PERMISSIONS__ELEVATED__ENABLED` | type | False |
| `permissions.elevated.allow_from` | `ECHO_AGENT_PERMISSIONS__ELEVATED__ALLOW_FROM` | GenericAlias | PydanticUndefined |
| `credentials.encryption_key_env` | `ECHO_AGENT_CREDENTIALS__ENCRYPTION_KEY_ENV` | type | 'ECHO_AGENT_CREDENTIAL_KEY' |
| `credentials.require_encryption` | `ECHO_AGENT_CREDENTIALS__REQUIRE_ENCRYPTION` | type | True |
| `session.max_history_messages` | `ECHO_AGENT_SESSION__MAX_HISTORY_MESSAGES` | type | 500 |
| `session.expiry_hours` | `ECHO_AGENT_SESSION__EXPIRY_HOURS` | type | 72 |
| `session.context_window_tokens` | `ECHO_AGENT_SESSION__CONTEXT_WINDOW_TOKENS` | type | 0 |
| `session.compression_window_cap` | `ECHO_AGENT_SESSION__COMPRESSION_WINDOW_CAP` | type | 200000 |
| `session.introduction_enabled` | `ECHO_AGENT_SESSION__INTRODUCTION_ENABLED` | type | True |
| `session.im_clarify_pending_ttl_seconds` | `ECHO_AGENT_SESSION__IM_CLARIFY_PENDING_TTL_SECONDS` | type | 300 |
| `session.introduction_template` | `ECHO_AGENT_SESSION__INTRODUCTION_TEMPLATE` | type | '' |
| `session.history_image_ttl_minutes` | `ECHO_AGENT_SESSION__HISTORY_IMAGE_TTL_MINUTES` | type | 30 |
| `session.history_image_limit` | `ECHO_AGENT_SESSION__HISTORY_IMAGE_LIMIT` | type | 4 |
| `session.history_image_skip_if_current` | `ECHO_AGENT_SESSION__HISTORY_IMAGE_SKIP_IF_CURRENT` | type | True |
| `session.group_session_scope` | `ECHO_AGENT_SESSION__GROUP_SESSION_SCOPE` | _LiteralGenericAlias | 'per_user' |
| `memory.enabled` | `ECHO_AGENT_MEMORY__ENABLED` | type | True |
| `memory.scope_policy` | `ECHO_AGENT_MEMORY__SCOPE_POLICY` | _LiteralGenericAlias | 'session' |
| `memory.cross_channel_owner` | `ECHO_AGENT_MEMORY__CROSS_CHANNEL_OWNER` | type | True |
| `memory.owner_key` | `ECHO_AGENT_MEMORY__OWNER_KEY` | type | 'owner' |
| `memory.allow_model_environment_writes` | `ECHO_AGENT_MEMORY__ALLOW_MODEL_ENVIRONMENT_WRITES` | type | False |
| `memory.principal_bindings` | `ECHO_AGENT_MEMORY__PRINCIPAL_BINDINGS` | GenericAlias | PydanticUndefined |
| `memory.retrieval_on_miss` | `ECHO_AGENT_MEMORY__RETRIEVAL_ON_MISS` | _LiteralGenericAlias | 'degrade' |
| `memory.retrieval_miss_timeout_seconds` | `ECHO_AGENT_MEMORY__RETRIEVAL_MISS_TIMEOUT_SECONDS` | type | 0.8 |
| `memory.cache_ttl_seconds` | `ECHO_AGENT_MEMORY__CACHE_TTL_SECONDS` | type | 60.0 |
| `memory.cache_jaccard_min` | `ECHO_AGENT_MEMORY__CACHE_JACCARD_MIN` | type | 0.3 |
| `memory.consolidation_threshold` | `ECHO_AGENT_MEMORY__CONSOLIDATION_THRESHOLD` | type | 20 |
| `memory.narrative_episode_count` | `ECHO_AGENT_MEMORY__NARRATIVE_EPISODE_COUNT` | type | 3 |
| `memory.vector_enabled` | `ECHO_AGENT_MEMORY__VECTOR_ENABLED` | type | True |
| `memory.vector_dimensions` | `ECHO_AGENT_MEMORY__VECTOR_DIMENSIONS` | type | 0 |
| `memory.max_user_memories` | `ECHO_AGENT_MEMORY__MAX_USER_MEMORIES` | type | 1000 |
| `memory.max_env_memories` | `ECHO_AGENT_MEMORY__MAX_ENV_MEMORIES` | type | 500 |
| `memory.memory_nudge_interval` | `ECHO_AGENT_MEMORY__MEMORY_NUDGE_INTERVAL` | type | 10 |
| `memory.importance_decay_days` | `ECHO_AGENT_MEMORY__IMPORTANCE_DECAY_DAYS` | type | 30.0 |
| `memory.snapshot_enabled` | `ECHO_AGENT_MEMORY__SNAPSHOT_ENABLED` | type | True |
| `memory.snapshot_layering` | `ECHO_AGENT_MEMORY__SNAPSHOT_LAYERING` | type | True |
| `memory.snapshot_user_core_max` | `ECHO_AGENT_MEMORY__SNAPSHOT_USER_CORE_MAX` | type | 12 |
| `memory.snapshot_env_core_max` | `ECHO_AGENT_MEMORY__SNAPSHOT_ENV_CORE_MAX` | type | 8 |
| `memory.contradiction_detection` | `ECHO_AGENT_MEMORY__CONTRADICTION_DETECTION` | type | True |
| `memory.sleep_consolidation` | `ECHO_AGENT_MEMORY__SLEEP_CONSOLIDATION` | type | True |
| `memory.archival_threshold` | `ECHO_AGENT_MEMORY__ARCHIVAL_THRESHOLD` | type | 0.05 |
| `memory.forget_threshold` | `ECHO_AGENT_MEMORY__FORGET_THRESHOLD` | type | 0.01 |
| `memory.lineage_max_versions` | `ECHO_AGENT_MEMORY__LINEAGE_MAX_VERSIONS` | type | 3 |
| `memory.lineage_retention_days` | `ECHO_AGENT_MEMORY__LINEAGE_RETENTION_DAYS` | type | 90 |
| `memory.max_working_memory` | `ECHO_AGENT_MEMORY__MAX_WORKING_MEMORY` | type | 20 |
| `memory.embedding_backend` | `ECHO_AGENT_MEMORY__EMBEDDING_BACKEND` | _LiteralGenericAlias | 'auto' |
| `memory.embedding_model` | `ECHO_AGENT_MEMORY__EMBEDDING_MODEL` | type | '' |
| `memory.local_embedding_model` | `ECHO_AGENT_MEMORY__LOCAL_EMBEDDING_MODEL` | type | 'BAAI/bge-small-zh-v1.5' |
| `memory.hf_embedding_endpoint` | `ECHO_AGENT_MEMORY__HF_EMBEDDING_ENDPOINT` | type | 'https://hf-mirror.com' |
| `memory.embed_timeout_seconds` | `ECHO_AGENT_MEMORY__EMBED_TIMEOUT_SECONDS` | type | 1.5 |
| `memory.rrf_min_similarity` | `ECHO_AGENT_MEMORY__RRF_MIN_SIMILARITY` | type | 0.3 |
| `memory.rerank_enabled` | `ECHO_AGENT_MEMORY__RERANK_ENABLED` | type | True |
| `memory.rerank_model` | `ECHO_AGENT_MEMORY__RERANK_MODEL` | type | 'BAAI/bge-reranker-base' |
| `memory.rerank_top_k` | `ECHO_AGENT_MEMORY__RERANK_TOP_K` | type | 10 |
| `memory.rerank_min_score` | `ECHO_AGENT_MEMORY__RERANK_MIN_SCORE` | type | 0.0 |
| `memory.rerank_timeout_seconds` | `ECHO_AGENT_MEMORY__RERANK_TIMEOUT_SECONDS` | type | 5.0 |
| `memory.rerank_load_timeout_seconds` | `ECHO_AGENT_MEMORY__RERANK_LOAD_TIMEOUT_SECONDS` | type | 60.0 |
| `memory.embed_load_timeout_seconds` | `ECHO_AGENT_MEMORY__EMBED_LOAD_TIMEOUT_SECONDS` | type | 60.0 |
| `memory.local_embedding_cache_dir` | `ECHO_AGENT_MEMORY__LOCAL_EMBEDDING_CACHE_DIR` | type | '~/.echo-agent/models/fastembed' |
| `memory.local_embedding_max_load_attempts` | `ECHO_AGENT_MEMORY__LOCAL_EMBEDDING_MAX_LOAD_ATTEMPTS` | type | 5 |
| `memory.local_embedding_retry_backoff_seconds` | `ECHO_AGENT_MEMORY__LOCAL_EMBEDDING_RETRY_BACKOFF_SECONDS` | type | 30.0 |
| `memory.contradiction_scan_on_store` | `ECHO_AGENT_MEMORY__CONTRADICTION_SCAN_ON_STORE` | type | False |
| `memory.auto_resolve_contradictions` | `ECHO_AGENT_MEMORY__AUTO_RESOLVE_CONTRADICTIONS` | type | False |
| `memory.reflection_enabled` | `ECHO_AGENT_MEMORY__REFLECTION_ENABLED` | type | True |
| `knowledge.enabled` | `ECHO_AGENT_KNOWLEDGE__ENABLED` | type | True |
| `knowledge.docs_dir` | `ECHO_AGENT_KNOWLEDGE__DOCS_DIR` | type | 'data/knowledge' |
| `knowledge.index_path` | `ECHO_AGENT_KNOWLEDGE__INDEX_PATH` | type | 'data/knowledge_index.json' |
| `knowledge.auto_index` | `ECHO_AGENT_KNOWLEDGE__AUTO_INDEX` | type | True |
| `knowledge.chunk_size` | `ECHO_AGENT_KNOWLEDGE__CHUNK_SIZE` | type | 1200 |
| `knowledge.chunk_overlap` | `ECHO_AGENT_KNOWLEDGE__CHUNK_OVERLAP` | type | 120 |
| `knowledge.max_results` | `ECHO_AGENT_KNOWLEDGE__MAX_RESULTS` | type | 5 |
| `knowledge.allowed_extensions` | `ECHO_AGENT_KNOWLEDGE__ALLOWED_EXTENSIONS` | GenericAlias | PydanticUndefined |
| `multi_agent.enabled` | `ECHO_AGENT_MULTI_AGENT__ENABLED` | type | True |
| `multi_agent.max_depth` | `ECHO_AGENT_MULTI_AGENT__MAX_DEPTH` | type | 3 |
| `multi_agent.max_parallel_workers` | `ECHO_AGENT_MULTI_AGENT__MAX_PARALLEL_WORKERS` | type | 4 |
| `multi_agent.max_iterations` | `ECHO_AGENT_MULTI_AGENT__MAX_ITERATIONS` | type | 12 |
| `multi_agent.audit_path` | `ECHO_AGENT_MULTI_AGENT__AUDIT_PATH` | type | 'data/delegation_audit.jsonl' |
| `multi_agent.worker_profiles` | `ECHO_AGENT_MULTI_AGENT__WORKER_PROFILES` | GenericAlias | PydanticUndefined |
| `scheduler.enabled` | `ECHO_AGENT_SCHEDULER__ENABLED` | type | True |
| `scheduler.max_concurrent_jobs` | `ECHO_AGENT_SCHEDULER__MAX_CONCURRENT_JOBS` | type | 10 |
| `checkpoint.enabled` | `ECHO_AGENT_CHECKPOINT__ENABLED` | type | True |
| `checkpoint.store_path` | `ECHO_AGENT_CHECKPOINT__STORE_PATH` | type | '~/.echo-agent/checkpoints/store' |
| `checkpoint.max_snapshots_per_workspace` | `ECHO_AGENT_CHECKPOINT__MAX_SNAPSHOTS_PER_WORKSPACE` | type | 20 |
| `checkpoint.max_total_size_mb` | `ECHO_AGENT_CHECKPOINT__MAX_TOTAL_SIZE_MB` | type | 500 |
| `checkpoint.max_file_size_mb` | `ECHO_AGENT_CHECKPOINT__MAX_FILE_SIZE_MB` | type | 10 |
| `validation.enabled` | `ECHO_AGENT_VALIDATION__ENABLED` | type | True |
| `validation.timeout_sec` | `ECHO_AGENT_VALIDATION__TIMEOUT_SEC` | type | 5.0 |
| `validation.max_diagnostics` | `ECHO_AGENT_VALIDATION__MAX_DIAGNOSTICS` | type | 10 |
| `validation.max_file_size_kb` | `ECHO_AGENT_VALIDATION__MAX_FILE_SIZE_KB` | type | 512 |
| `media_understanding.audio_enabled` | `ECHO_AGENT_MEDIA_UNDERSTANDING__AUDIO_ENABLED` | type | True |
| `media_understanding.audio_provider` | `ECHO_AGENT_MEDIA_UNDERSTANDING__AUDIO_PROVIDER` | type | 'auto' |
| `media_understanding.min_audio_size_kb` | `ECHO_AGENT_MEDIA_UNDERSTANDING__MIN_AUDIO_SIZE_KB` | type | 1.0 |
| `media_understanding.max_audio_size_kb` | `ECHO_AGENT_MEDIA_UNDERSTANDING__MAX_AUDIO_SIZE_KB` | type | 25000 |
| `media_understanding.local_model_size` | `ECHO_AGENT_MEDIA_UNDERSTANDING__LOCAL_MODEL_SIZE` | type | 'base' |
| `media_understanding.video_enabled` | `ECHO_AGENT_MEDIA_UNDERSTANDING__VIDEO_ENABLED` | type | True |
| `media_understanding.video_frame_count` | `ECHO_AGENT_MEDIA_UNDERSTANDING__VIDEO_FRAME_COUNT` | type | 4 |
| `media_understanding.video_vision_model` | `ECHO_AGENT_MEDIA_UNDERSTANDING__VIDEO_VISION_MODEL` | type | '' |
| `media_understanding.video_vision_prompt` | `ECHO_AGENT_MEDIA_UNDERSTANDING__VIDEO_VISION_PROMPT` | type | '简要描述这段视频的画面内容。' |
| `media_understanding.min_video_size_kb` | `ECHO_AGENT_MEDIA_UNDERSTANDING__MIN_VIDEO_SIZE_KB` | type | 1.0 |
| `media_understanding.max_video_size_kb` | `ECHO_AGENT_MEDIA_UNDERSTANDING__MAX_VIDEO_SIZE_KB` | type | 204800 |
| `media_understanding.video_ffmpeg_concurrency` | `ECHO_AGENT_MEDIA_UNDERSTANDING__VIDEO_FFMPEG_CONCURRENCY` | type | 2 |
| `media_understanding.transcription_base_url` | `ECHO_AGENT_MEDIA_UNDERSTANDING__TRANSCRIPTION_BASE_URL` | type | 'https://api.groq.com/openai/v1' |
| `media_understanding.transcription_model` | `ECHO_AGENT_MEDIA_UNDERSTANDING__TRANSCRIPTION_MODEL` | type | 'whisper-large-v3' |
| `runtime.single_instance` | `ECHO_AGENT_RUNTIME__SINGLE_INSTANCE` | type | True |
| `storage.database_path` | `ECHO_AGENT_STORAGE__DATABASE_PATH` | type | 'data/echo_agent.db' |
| `storage.sessions_dir` | `ECHO_AGENT_STORAGE__SESSIONS_DIR` | type | 'data/sessions' |
| `storage.memory_dir` | `ECHO_AGENT_STORAGE__MEMORY_DIR` | type | 'data/memory' |
| `storage.logs_dir` | `ECHO_AGENT_STORAGE__LOGS_DIR` | type | 'data/logs' |
| `storage.spill_dir` | `ECHO_AGENT_STORAGE__SPILL_DIR` | type | 'data/spill' |
| `spill.enabled` | `ECHO_AGENT_SPILL__ENABLED` | type | True |
| `spill.max_inline_chars` | `ECHO_AGENT_SPILL__MAX_INLINE_CHARS` | type | 6000 |
| `spill.retention_days` | `ECHO_AGENT_SPILL__RETENTION_DAYS` | type | 7 |
| `spill.max_total_mb` | `ECHO_AGENT_SPILL__MAX_TOTAL_MB` | type | 512 |
| `spill.sweep_interval_hours` | `ECHO_AGENT_SPILL__SWEEP_INTERVAL_HOURS` | type | 6 |
| `observability.log_level` | `ECHO_AGENT_OBSERVABILITY__LOG_LEVEL` | type | 'INFO' |
| `observability.trace_enabled` | `ECHO_AGENT_OBSERVABILITY__TRACE_ENABLED` | type | True |
| `observability.max_trace_files` | `ECHO_AGENT_OBSERVABILITY__MAX_TRACE_FILES` | type | 500 |
| `observability.health_check_interval_seconds` | `ECHO_AGENT_OBSERVABILITY__HEALTH_CHECK_INTERVAL_SECONDS` | type | 60 |
| `observability.otel_enabled` | `ECHO_AGENT_OBSERVABILITY__OTEL_ENABLED` | type | True |
| `observability.otel_endpoint` | `ECHO_AGENT_OBSERVABILITY__OTEL_ENDPOINT` | type | '' |
| `observability.otel_service_name` | `ECHO_AGENT_OBSERVABILITY__OTEL_SERVICE_NAME` | type | 'echo-agent' |
| `observability.otel_export_interval_ms` | `ECHO_AGENT_OBSERVABILITY__OTEL_EXPORT_INTERVAL_MS` | type | 5000 |
| `observability.loop_watchdog_enabled` | `ECHO_AGENT_OBSERVABILITY__LOOP_WATCHDOG_ENABLED` | type | True |
| `observability.loop_watchdog_warn_seconds` | `ECHO_AGENT_OBSERVABILITY__LOOP_WATCHDOG_WARN_SECONDS` | type | 5.0 |
| `observability.loop_watchdog_kill_seconds` | `ECHO_AGENT_OBSERVABILITY__LOOP_WATCHDOG_KILL_SECONDS` | type | 30.0 |
| `observability.loop_watchdog_check_interval_seconds` | `ECHO_AGENT_OBSERVABILITY__LOOP_WATCHDOG_CHECK_INTERVAL_SECONDS` | type | 5.0 |
| `observability.loop_watchdog_max_restarts_per_hour` | `ECHO_AGENT_OBSERVABILITY__LOOP_WATCHDOG_MAX_RESTARTS_PER_HOUR` | type | 5 |
| `skills.enabled` | `ECHO_AGENT_SKILLS__ENABLED` | type | True |
| `skills.skills_dir` | `ECHO_AGENT_SKILLS__SKILLS_DIR` | type | 'skills' |
| `skills.creation_nudge_interval` | `ECHO_AGENT_SKILLS__CREATION_NUDGE_INTERVAL` | type | 10 |
| `skills.disabled` | `ECHO_AGENT_SKILLS__DISABLED` | GenericAlias | PydanticUndefined |
| `skills.external_dirs` | `ECHO_AGENT_SKILLS__EXTERNAL_DIRS` | GenericAlias | PydanticUndefined |
| `skills.allow_lazy_installs` | `ECHO_AGENT_SKILLS__ALLOW_LAZY_INSTALLS` | type | True |
| `skills.admission_policy` | `ECHO_AGENT_SKILLS__ADMISSION_POLICY` | _LiteralGenericAlias | 'stage_for_review' |
| `skills.auto_write_risk` | `ECHO_AGENT_SKILLS__AUTO_WRITE_RISK` | _LiteralGenericAlias | 'low' |
| `compression.enabled` | `ECHO_AGENT_COMPRESSION__ENABLED` | type | True |
| `compression.trigger_ratio` | `ECHO_AGENT_COMPRESSION__TRIGGER_RATIO` | type | 0.7 |
| `compression.tail_budget_ratio` | `ECHO_AGENT_COMPRESSION__TAIL_BUDGET_RATIO` | type | 0.4 |
| `compression.head_protect_count` | `ECHO_AGENT_COMPRESSION__HEAD_PROTECT_COUNT` | type | 3 |
| `compression.summary_target_ratio` | `ECHO_AGENT_COMPRESSION__SUMMARY_TARGET_RATIO` | type | 0.2 |
| `compression.summary_min_tokens` | `ECHO_AGENT_COMPRESSION__SUMMARY_MIN_TOKENS` | type | 2000 |
| `compression.summary_max_tokens` | `ECHO_AGENT_COMPRESSION__SUMMARY_MAX_TOKENS` | type | 12000 |
| `compression.summary_model` | `ECHO_AGENT_COMPRESSION__SUMMARY_MODEL` | type | '' |
| `compression.summary_cooldown_seconds` | `ECHO_AGENT_COMPRESSION__SUMMARY_COOLDOWN_SECONDS` | type | 600 |
| `compression.tool_pruning_enabled` | `ECHO_AGENT_COMPRESSION__TOOL_PRUNING_ENABLED` | type | True |
| `compression.tool_pruning_tail_budget_ratio` | `ECHO_AGENT_COMPRESSION__TOOL_PRUNING_TAIL_BUDGET_RATIO` | type | 0.3 |
| `compression.max_compression_count` | `ECHO_AGENT_COMPRESSION__MAX_COMPRESSION_COUNT` | type | 10 |
| `gateway.enabled` | `ECHO_AGENT_GATEWAY__ENABLED` | type | False |
| `gateway.host` | `ECHO_AGENT_GATEWAY__HOST` | type | '127.0.0.1' |
| `gateway.port` | `ECHO_AGENT_GATEWAY__PORT` | type | 58123 |
| `gateway.api_prefix` | `ECHO_AGENT_GATEWAY__API_PREFIX` | type | '/api/v1' |
| `gateway.ws_path` | `ECHO_AGENT_GATEWAY__WS_PATH` | type | '/ws' |
| `gateway.ws_heartbeat_seconds` | `ECHO_AGENT_GATEWAY__WS_HEARTBEAT_SECONDS` | type | 30.0 |
| `gateway.session_policy.mode` | `ECHO_AGENT_GATEWAY__SESSION_POLICY__MODE` | _LiteralGenericAlias | 'idle' |
| `gateway.session_policy.daily_reset_hour` | `ECHO_AGENT_GATEWAY__SESSION_POLICY__DAILY_RESET_HOUR` | type | 4 |
| `gateway.session_policy.idle_timeout_minutes` | `ECHO_AGENT_GATEWAY__SESSION_POLICY__IDLE_TIMEOUT_MINUTES` | type | 1440 |
| `gateway.auth.mode` | `ECHO_AGENT_GATEWAY__AUTH__MODE` | _LiteralGenericAlias | 'allowlist' |
| `gateway.auth.allowed_users` | `ECHO_AGENT_GATEWAY__AUTH__ALLOWED_USERS` | GenericAlias | PydanticUndefined |
| `gateway.auth.admin_users` | `ECHO_AGENT_GATEWAY__AUTH__ADMIN_USERS` | GenericAlias | PydanticUndefined |
| `gateway.auth.api_tokens` | `ECHO_AGENT_GATEWAY__AUTH__API_TOKENS` | GenericAlias | PydanticUndefined |
| `gateway.auth.admin_tokens` | `ECHO_AGENT_GATEWAY__AUTH__ADMIN_TOKENS` | GenericAlias | PydanticUndefined |
| `gateway.auth.allowed_origins` | `ECHO_AGENT_GATEWAY__AUTH__ALLOWED_ORIGINS` | GenericAlias | PydanticUndefined |
| `gateway.auth.token_header` | `ECHO_AGENT_GATEWAY__AUTH__TOKEN_HEADER` | type | 'X-Echo-Agent-Token' |
| `gateway.auth.pairing_ttl_seconds` | `ECHO_AGENT_GATEWAY__AUTH__PAIRING_TTL_SECONDS` | type | 300 |
| `gateway.auth.allowed_hosts` | `ECHO_AGENT_GATEWAY__AUTH__ALLOWED_HOSTS` | GenericAlias | PydanticUndefined |
| `gateway.platforms` | `ECHO_AGENT_GATEWAY__PLATFORMS` | GenericAlias | PydanticUndefined |
| `gateway.media_cache_dir` | `ECHO_AGENT_GATEWAY__MEDIA_CACHE_DIR` | type | 'data/media_cache' |
| `gateway.media_cache_max_mb` | `ECHO_AGENT_GATEWAY__MEDIA_CACHE_MAX_MB` | type | 500 |
| `gateway.media_max_file_mb` | `ECHO_AGENT_GATEWAY__MEDIA_MAX_FILE_MB` | type | 25 |
| `gateway.media_max_urls_per_message` | `ECHO_AGENT_GATEWAY__MEDIA_MAX_URLS_PER_MESSAGE` | type | 10 |
| `gateway.media_download_concurrency` | `ECHO_AGENT_GATEWAY__MEDIA_DOWNLOAD_CONCURRENCY` | type | 4 |
| `gateway.media_allow_private_addresses` | `ECHO_AGENT_GATEWAY__MEDIA_ALLOW_PRIVATE_ADDRESSES` | type | False |
| `gateway.hooks_dir` | `ECHO_AGENT_GATEWAY__HOOKS_DIR` | type | '' |
| `planning.enabled` | `ECHO_AGENT_PLANNING__ENABLED` | type | True |
| `planning.default_strategy` | `ECHO_AGENT_PLANNING__DEFAULT_STRATEGY` | type | 'auto' |
| `planning.max_tree_depth` | `ECHO_AGENT_PLANNING__MAX_TREE_DEPTH` | type | 5 |
| `planning.max_branches` | `ECHO_AGENT_PLANNING__MAX_BRANCHES` | type | 3 |
| `planning.reflection_enabled` | `ECHO_AGENT_PLANNING__REFLECTION_ENABLED` | type | True |
| `a2a.enabled` | `ECHO_AGENT_A2A__ENABLED` | type | True |
| `a2a.agent_name` | `ECHO_AGENT_A2A__AGENT_NAME` | type | 'echo-agent' |
| `a2a.agent_description` | `ECHO_AGENT_A2A__AGENT_DESCRIPTION` | type | 'A modular AI agent framework' |
| `a2a.capabilities` | `ECHO_AGENT_A2A__CAPABILITIES` | GenericAlias | PydanticUndefined |
| `a2a.task_ttl_seconds` | `ECHO_AGENT_A2A__TASK_TTL_SECONDS` | type | 3600.0 |
| `a2a.max_tasks` | `ECHO_AGENT_A2A__MAX_TASKS` | type | 1000 |
| `a2a.active_task_ttl_seconds` | `ECHO_AGENT_A2A__ACTIVE_TASK_TTL_SECONDS` | type | 86400.0 |
| `evaluation.dataset_path` | `ECHO_AGENT_EVALUATION__DATASET_PATH` | type | 'data/eval' |
| `evaluation.timeout_per_case` | `ECHO_AGENT_EVALUATION__TIMEOUT_PER_CASE` | type | 120 |
| `bus.max_queue_size` | `ECHO_AGENT_BUS__MAX_QUEUE_SIZE` | type | 1000 |
| `bus.max_concurrency` | `ECHO_AGENT_BUS__MAX_CONCURRENCY` | type | 50 |
| `rate_limit.session_rpm` | `ECHO_AGENT_RATE_LIMIT__SESSION_RPM` | type | 20 |
| `rate_limit.session_burst` | `ECHO_AGENT_RATE_LIMIT__SESSION_BURST` | type | 5 |
| `circuit_breaker.failure_threshold` | `ECHO_AGENT_CIRCUIT_BREAKER__FAILURE_THRESHOLD` | type | 5 |
| `circuit_breaker.recovery_seconds` | `ECHO_AGENT_CIRCUIT_BREAKER__RECOVERY_SECONDS` | type | 60.0 |
| `circuit_breaker.half_open_max` | `ECHO_AGENT_CIRCUIT_BREAKER__HALF_OPEN_MAX` | type | 2 |
| `plugins.enabled` | `ECHO_AGENT_PLUGINS__ENABLED` | type | True |
| `plugins.allow` | `ECHO_AGENT_PLUGINS__ALLOW` | GenericAlias | PydanticUndefined |
| `plugins.deny` | `ECHO_AGENT_PLUGINS__DENY` | GenericAlias | PydanticUndefined |
| `plugins.extra_dirs` | `ECHO_AGENT_PLUGINS__EXTRA_DIRS` | GenericAlias | PydanticUndefined |
| `plugins.config` | `ECHO_AGENT_PLUGINS__CONFIG` | GenericAlias | PydanticUndefined |
| `plugins.trusted_plugins` | `ECHO_AGENT_PLUGINS__TRUSTED_PLUGINS` | GenericAlias | PydanticUndefined |
| `plugins.permission_mode` | `ECHO_AGENT_PLUGINS__PERMISSION_MODE` | _LiteralGenericAlias | 'compat' |
| `ui.locale` | `ECHO_AGENT_UI__LOCALE` | _LiteralGenericAlias | 'auto' |
| `agent.max_iterations` | `ECHO_AGENT_AGENT__MAX_ITERATIONS` | type | 40 |
| `agent.tool_concurrency.enabled` | `ECHO_AGENT_AGENT__TOOL_CONCURRENCY__ENABLED` | type | True |
| `agent.tool_concurrency.max_concurrent` | `ECHO_AGENT_AGENT__TOOL_CONCURRENCY__MAX_CONCURRENT` | type | 4 |
| `agent.heartbeat.enabled` | `ECHO_AGENT_AGENT__HEARTBEAT__ENABLED` | type | True |
| `agent.heartbeat.first_delay_sec` | `ECHO_AGENT_AGENT__HEARTBEAT__FIRST_DELAY_SEC` | type | 30 |
| `agent.heartbeat.min_interval_sec` | `ECHO_AGENT_AGENT__HEARTBEAT__MIN_INTERVAL_SEC` | type | 60 |
| `agent.heartbeat.verbosity` | `ECHO_AGENT_AGENT__HEARTBEAT__VERBOSITY` | _LiteralGenericAlias | 'key_milestones' |
| `agent.heartbeat.template` | `ECHO_AGENT_AGENT__HEARTBEAT__TEMPLATE` | type | '⏳ {activity}（已用时 {elapsed}）' |
| `agent.inspection.enabled` | `ECHO_AGENT_AGENT__INSPECTION__ENABLED` | type | False |
| `agent.inspection.tick_interval_sec` | `ECHO_AGENT_AGENT__INSPECTION__TICK_INTERVAL_SEC` | type | 300 |
| `agent.inspection.inspect_file` | `ECHO_AGENT_AGENT__INSPECTION__INSPECT_FILE` | type | 'INSPECT.md' |
| `agent.inspection.max_items_per_tick` | `ECHO_AGENT_AGENT__INSPECTION__MAX_ITEMS_PER_TICK` | type | 5 |
| `agent.inspection.deliver_channel` | `ECHO_AGENT_AGENT__INSPECTION__DELIVER_CHANNEL` | type | '' |
| `agent.inspection.deliver_chat_id` | `ECHO_AGENT_AGENT__INSPECTION__DELIVER_CHAT_ID` | type | '' |
| `evolution.enabled` | `ECHO_AGENT_EVOLUTION__ENABLED` | type | False |
| `evolution.trigger_mode` | `ECHO_AGENT_EVOLUTION__TRIGGER_MODE` | _LiteralGenericAlias | 'manual' |
| `evolution.threshold_trajectories` | `ECHO_AGENT_EVOLUTION__THRESHOLD_TRAJECTORIES` | type | 50 |
| `evolution.cron_expression` | `ECHO_AGENT_EVOLUTION__CRON_EXPRESSION` | type | '0 4 * * *' |
| `evolution.max_candidates_per_run` | `ECHO_AGENT_EVOLUTION__MAX_CANDIDATES_PER_RUN` | type | 3 |
| `evolution.max_trajectories_per_run` | `ECHO_AGENT_EVOLUTION__MAX_TRAJECTORIES_PER_RUN` | type | 200 |
| `evolution.eval_dataset_path` | `ECHO_AGENT_EVOLUTION__EVAL_DATASET_PATH` | type | 'data/eval/baseline.yaml' |
| `evolution.regression_threshold` | `ECHO_AGENT_EVOLUTION__REGRESSION_THRESHOLD` | type | 0.05 |
| `evolution.require_strict_improvement` | `ECHO_AGENT_EVOLUTION__REQUIRE_STRICT_IMPROVEMENT` | type | True |
| `evolution.min_eval_cases` | `ECHO_AGENT_EVOLUTION__MIN_EVAL_CASES` | type | 3 |
| `evolution.record_trajectories` | `ECHO_AGENT_EVOLUTION__RECORD_TRAJECTORIES` | type | True |
| `evolution.trajectory_retention_days` | `ECHO_AGENT_EVOLUTION__TRAJECTORY_RETENTION_DAYS` | type | 30 |
| `evolution.evolver_model` | `ECHO_AGENT_EVOLUTION__EVOLVER_MODEL` | type | '' |
| `evolution.skill_size_limit_bytes` | `ECHO_AGENT_EVOLUTION__SKILL_SIZE_LIMIT_BYTES` | type | 50000 |
| `evolution.redact_args` | `ECHO_AGENT_EVOLUTION__REDACT_ARGS` | type | True |
| `evolution.eval_parallel` | `ECHO_AGENT_EVOLUTION__EVAL_PARALLEL` | type | 2 |
| `evolution.eval_timeout_seconds` | `ECHO_AGENT_EVOLUTION__EVAL_TIMEOUT_SECONDS` | type | 60 |
| `evolution.cooldown_seconds_after_promote` | `ECHO_AGENT_EVOLUTION__COOLDOWN_SECONDS_AFTER_PROMOTE` | type | 86400 |
| `evolution.auto_promote` | `ECHO_AGENT_EVOLUTION__AUTO_PROMOTE` | type | True |
| `evolution.candidate_review_required` | `ECHO_AGENT_EVOLUTION__CANDIDATE_REVIEW_REQUIRED` | type | False |
| `cost.enabled` | `ECHO_AGENT_COST__ENABLED` | type | False |
| `cost.daily_budget_usd` | `ECHO_AGENT_COST__DAILY_BUDGET_USD` | type | 0.0 |
| `cost.soft_threshold_ratio` | `ECHO_AGENT_COST__SOFT_THRESHOLD_RATIO` | type | 0.8 |
| `cost.pricing_overrides` | `ECHO_AGENT_COST__PRICING_OVERRIDES` | type | PydanticUndefined |
| `workspace` | `ECHO_AGENT_WORKSPACE` | type | '~/.echo-agent' |

## Shell-Specific Syntax

### Bash / Zsh (Linux, macOS, WSL2)

```bash
# Single variable
export ECHO_AGENT_GATEWAY__PORT=4000

# Multiple variables in .env file
cat >> ~/.bashrc << 'EOF'
export ECHO_AGENT_MODELS__DEFAULT__API_KEY="sk-ant-..."
export ECHO_AGENT_GATEWAY__AUTH__MODE="allowlist"
export ECHO_AGENT_GATEWAY__AUTH__API_TOKENS="token1,token2"
EOF
source ~/.bashrc
```

### PowerShell (Windows)

```powershell
# Session variable
$env:ECHO_AGENT_GATEWAY__PORT = "4000"

# Persistent (user-level)
[System.Environment]::SetEnvironmentVariable(
    "ECHO_AGENT_MODELS__DEFAULT__API_KEY",
    "sk-ant-...",
    "User"
)
```

### Windows CMD

```batch
:: Session variable
set ECHO_AGENT_GATEWAY__PORT=4000

:: Persistent
setx ECHO_AGENT_MODELS__DEFAULT__API_KEY "sk-ant-..."
```

!!! warning "Windows path separators"
    On native Windows, use backslashes in `ECHO_AGENT_STORAGE__BASE_DIR`. In WSL2, use forward slashes.

---

## Docker / Container Usage

### Docker Run

```bash
docker run -d \
  -e ECHO_AGENT_GATEWAY__HOST=0.0.0.0 \
  -e ECHO_AGENT_GATEWAY__PORT=3000 \
  -e ECHO_AGENT_GATEWAY__AUTH__MODE=allowlist \
  -e ECHO_AGENT_GATEWAY__AUTH__API_TOKENS="mytoken123" \
  -e ECHO_AGENT_MODELS__DEFAULT__API_KEY="sk-ant-..." \
  -e ECHO_AGENT_CHANNELS__TELEGRAM__ENABLED=true \
  -e ECHO_AGENT_CHANNELS__TELEGRAM__TOKEN="123456:ABC..." \
  -v echo-agent-data:/root/.echo-agent/data \
  -p 3000:3000 \
  echo-agent:latest
```

### Docker Compose

```yaml
services:
  echo-agent:
    image: echo-agent:latest
    environment:
      ECHO_AGENT_GATEWAY__HOST: "0.0.0.0"
      ECHO_AGENT_GATEWAY__PORT: "3000"
      ECHO_AGENT_GATEWAY__AUTH__MODE: "allowlist"
      ECHO_AGENT_GATEWAY__AUTH__API_TOKENS: "mytoken123"
      ECHO_AGENT_MODELS__DEFAULT__PROVIDER: "anthropic"
      ECHO_AGENT_MODELS__DEFAULT__API_KEY: "${ANTHROPIC_API_KEY}"
      ECHO_AGENT_OBSERVABILITY__LOG_LEVEL: "info"
      ECHO_AGENT_OBSERVABILITY__LOG_FORMAT: "json"
    env_file:
      - .env
    volumes:
      - agent-data:/root/.echo-agent/data
    ports:
      - "3000:3000"

volumes:
  agent-data:
```

### Using `.env` Files

```bash
# .env file (do NOT commit to version control)
ECHO_AGENT_MODELS__DEFAULT__API_KEY=sk-ant-api03-...
ECHO_AGENT_CHANNELS__TELEGRAM__TOKEN=123456789:ABCdef...
ECHO_AGENT_CHANNELS__DISCORD__TOKEN=MTIzNDU2...
ECHO_AGENT_GATEWAY__AUTH__API_TOKENS=prod-token-abc,prod-token-def
```

!!! danger "Never commit secrets"
    Add `.env` to `.gitignore`. Use a secrets manager (Vault, AWS Secrets Manager, etc.) for production deployments.

---

## Debugging

### Verify Effective Configuration

```bash
# Show resolved config (env vars applied)
echo-agent config dump

# Explain where a specific value comes from
echo-agent config explain gateway.auth.mode
# Output: gateway.auth.mode = "allowlist" (from: environment variable ECHO_AGENT_GATEWAY__AUTH__MODE)
```

### Common Issues

| Symptom | Likely Cause | Fix |
|---------|-------------|-----|
| Env var ignored | Wrong nesting separator | Use `__` (double underscore) |
| Boolean not working | Unexpected string value | Use `true`/`false`, `1`/`0` |
| List has one item | Forgot comma separation | `"a,b,c"` or JSON `'["a","b"]'` |
| Variable not found | Typo in section name | Run `echo-agent config explain <path>` |
| Override not applied | CLI flag takes precedence | Remove conflicting CLI flags |

!!! tip "List all active env vars"
    ```bash
    env | grep ECHO_AGENT_ | sort
    ```
