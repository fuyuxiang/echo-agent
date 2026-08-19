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

---

## Type Coercion

Environment variables are always strings. Echo Agent coerces them to the expected type:

| Target Type | Env Value | Result |
|-------------|-----------|--------|
| `bool` | `true`, `1`, `yes`, `on` | `True` |
| `bool` | `false`, `0`, `no`, `off` | `False` |
| `int` | `"3000"` | `3000` |
| `float` | `"0.5"` | `0.5` |
| `list` | `"item1,item2,item3"` | `["item1", "item2", "item3"]` |
| `list` | `'["item1","item2"]'` | `["item1", "item2"]` (JSON) |
| `dict` | `'{"key": "value"}'` | `{"key": "value"}` (JSON) |
| `str` | `"hello"` | `"hello"` |

!!! warning "List separator"
    Comma-separated lists do not support values containing commas. Use JSON array syntax for complex list values.

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
| `ECHO_AGENT_GATEWAY__HOST` | str | `127.0.0.1` | Gateway bind address |
| `ECHO_AGENT_GATEWAY__PORT` | int | `3000` | Gateway listen port |
| `ECHO_AGENT_GATEWAY__AUTH__MODE` | str | `pairing` | Auth mode: `open`, `allowlist`, `pairing` |
| `ECHO_AGENT_GATEWAY__AUTH__API_TOKENS` | list | `[]` | Comma-separated API tokens |
| `ECHO_AGENT_GATEWAY__AUTH__ADMIN_TOKENS` | list | `[]` | Comma-separated admin tokens |
| `ECHO_AGENT_GATEWAY__AUTH__ALLOWED_ORIGINS` | list | `[]` | Allowed CORS origins |
| `ECHO_AGENT_GATEWAY__AUTH__ALLOWED_HOSTS` | list | `[]` | Allowed Host header values |
| `ECHO_AGENT_GATEWAY__AUTH__TOKEN_HEADER` | str | `X-Echo-Token` | Custom token header name |
| `ECHO_AGENT_GATEWAY__AUTH__PAIRING_TTL_SECONDS` | int | `300` | Pairing code validity duration |
| `ECHO_AGENT_GATEWAY__MAX_SESSIONS_PER_TOKEN` | int | `5` | Concurrent session limit per token |
| `ECHO_AGENT_GATEWAY__MAX_MESSAGE_SIZE` | int | `1048576` | Maximum WebSocket message size (bytes) |

### Models

| Variable | Type | Default | Description |
|----------|------|---------|-------------|
| `ECHO_AGENT_MODELS__DEFAULT__PROVIDER` | str | `anthropic` | LLM provider name |
| `ECHO_AGENT_MODELS__DEFAULT__MODEL` | str | `claude-sonnet-4-20250514` | Model identifier |
| `ECHO_AGENT_MODELS__DEFAULT__API_KEY` | str | — | Provider API key |
| `ECHO_AGENT_MODELS__DEFAULT__BASE_URL` | str | — | Custom API base URL |
| `ECHO_AGENT_MODELS__DEFAULT__MAX_TOKENS` | int | `4096` | Max output tokens |
| `ECHO_AGENT_MODELS__DEFAULT__TEMPERATURE` | float | `0.7` | Sampling temperature |
| `ECHO_AGENT_MODELS__PLANNING__PROVIDER` | str | — | Provider for planning model |
| `ECHO_AGENT_MODELS__PLANNING__MODEL` | str | — | Dedicated planning model |

### Channels

| Variable | Type | Default | Description |
|----------|------|---------|-------------|
| `ECHO_AGENT_CHANNELS__TELEGRAM__ENABLED` | bool | `false` | Enable Telegram channel |
| `ECHO_AGENT_CHANNELS__TELEGRAM__TOKEN` | str | — | Telegram bot API token |
| `ECHO_AGENT_CHANNELS__TELEGRAM__ALLOW_FROM` | list | `[]` | Allowed user IDs |
| `ECHO_AGENT_CHANNELS__DISCORD__ENABLED` | bool | `false` | Enable Discord channel |
| `ECHO_AGENT_CHANNELS__DISCORD__TOKEN` | str | — | Discord bot token |
| `ECHO_AGENT_CHANNELS__SLACK__ENABLED` | bool | `false` | Enable Slack channel |
| `ECHO_AGENT_CHANNELS__SLACK__BOT_TOKEN` | str | — | Slack bot token (xoxb-) |
| `ECHO_AGENT_CHANNELS__SLACK__APP_TOKEN` | str | — | Slack app token (xapp-) |
| `ECHO_AGENT_CHANNELS__WEBHOOK__ENABLED` | bool | `false` | Enable webhook channel |
| `ECHO_AGENT_CHANNELS__WEBHOOK__SECRET` | str | — | Webhook signature secret |
| `ECHO_AGENT_CHANNELS__CLI__ENABLED` | bool | `true` | Enable CLI channel |
| `ECHO_AGENT_CHANNELS__CRON__ENABLED` | bool | `false` | Enable cron channel |

### Storage

| Variable | Type | Default | Description |
|----------|------|---------|-------------|
| `ECHO_AGENT_STORAGE__BASE_DIR` | str | `~/.echo-agent` | Root data directory |
| `ECHO_AGENT_STORAGE__SQLITE__JOURNAL_MODE` | str | `wal` | SQLite journal mode |
| `ECHO_AGENT_SPILL__MAX_SIZE_MB` | int | `500` | Maximum spill directory size |
| `ECHO_AGENT_SPILL__TTL_HOURS` | int | `24` | Spill file time-to-live |
| `ECHO_AGENT_CHECKPOINT__AUTO_SAVE` | bool | `true` | Enable automatic checkpoints |
| `ECHO_AGENT_CHECKPOINT__INTERVAL_MINUTES` | int | `30` | Auto-checkpoint interval |

### Observability

| Variable | Type | Default | Description |
|----------|------|---------|-------------|
| `ECHO_AGENT_OBSERVABILITY__LOG_LEVEL` | str | `info` | Log level: `debug`, `info`, `warning`, `error` |
| `ECHO_AGENT_OBSERVABILITY__LOG_FORMAT` | str | `text` | Log format: `text`, `json` |
| `ECHO_AGENT_OBSERVABILITY__OTEL_ENDPOINT` | str | — | OpenTelemetry collector endpoint |
| `ECHO_AGENT_OBSERVABILITY__OTEL_ENABLED` | bool | `false` | Enable OpenTelemetry export |
| `ECHO_AGENT_OBSERVABILITY__METRICS_PORT` | int | — | Prometheus metrics port |

### Cost

| Variable | Type | Default | Description |
|----------|------|---------|-------------|
| `ECHO_AGENT_COST__DAILY_LIMIT` | float | — | Daily spending cap (USD) |
| `ECHO_AGENT_COST__MONTHLY_LIMIT` | float | — | Monthly spending cap (USD) |
| `ECHO_AGENT_COST__ALERT_THRESHOLD` | float | `0.8` | Alert at this fraction of limit |

---

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
