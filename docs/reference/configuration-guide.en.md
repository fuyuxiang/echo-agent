# Configuration Guide

This guide explains how Echo Agent loads, merges, and validates configuration. For the full field reference with every option listed, see [Configuration Reference](configuration.en.md).

## Loading Order

Configuration is assembled from multiple sources, each layer overriding the previous:

```
┌─────────────────────────────────────────────────────────┐
│ 1. Package defaults (built into the Python package)     │
├─────────────────────────────────────────────────────────┤
│ 2. User YAML file (-c flag or ~/.echo-agent/config.yml) │
├─────────────────────────────────────────────────────────┤
│ 3. Environment variables (ECHO_AGENT_ prefix)           │
├─────────────────────────────────────────────────────────┤
│ 4. CLI runtime overrides (--set key=value)              │
├─────────────────────────────────────────────────────────┤
│ 5. Profile defaults (security/tool profiles applied)    │
├─────────────────────────────────────────────────────────┤
│ 6. Pydantic validation (type coercion + constraints)    │
└─────────────────────────────────────────────────────────┘
```

Later layers override earlier ones. Environment variables override YAML values; CLI flags override environment variables.

!!! warning
    Profile defaults are applied **after** CLI overrides. If a security profile restricts a field, your explicit override may be clamped or rejected during validation.

---

## Config File Locations

Echo Agent searches for configuration in this order:

| Priority | Location | Notes |
|----------|----------|-------|
| 1 | `-c /path/to/config.yml` | Explicit CLI flag, highest priority |
| 2 | `.echo-agent/config.yml` | Workspace-local config |
| 3 | `~/.echo-agent/config.yml` | User-global config |
| 4 | Package defaults | Built-in fallback |

Supported file extensions: `.yml`, `.yaml`

```bash
# Use explicit config file
echo-agent run -c /etc/echo-agent/production.yml

# Workspace-local (auto-detected)
mkdir -p .echo-agent && cp config.yml .echo-agent/config.yml
```

!!! tip
    Workspace-local config (`.echo-agent/config.yml`) is ideal for project-specific tool permissions and model selections. Keep secrets in environment variables or the user-global config.

---

## Top-Level Structure

The configuration file is organized into these top-level sections:

```yaml
# Echo Agent config.yml — all sections are optional
security:       # Security profiles and access control
channels:       # Channel integrations (Telegram, Discord, Slack, etc.)
models:         # LLM provider configuration
tools:          # Tool profiles, permissions, and approval modes
execution:      # Execution engine settings
permissions:    # Fine-grained permission rules
credentials:    # API keys and secrets (prefer env vars)
session:        # Session management and timeouts
memory:         # Memory persistence settings
knowledge:      # Knowledge base / RAG configuration
multi_agent:    # Multi-agent coordination
scheduler:      # Background task scheduling
checkpoint:     # Checkpoint and recovery
validation:     # Input/output validation rules
media_understanding:  # Vision, audio, document parsing
runtime:        # Runtime behavior (concurrency, limits)
storage:        # Storage backend configuration
spill:          # Large content spill-to-disk settings
observability:  # Logging, tracing, metrics
skills:         # Skill registry and evolution
compression:    # Context compression settings
gateway:        # Gateway server configuration
planning:       # Planning and reasoning settings
a2a:            # Agent-to-Agent protocol
evaluation:     # Eval framework settings
bus:            # Internal event bus
rate_limit:     # Rate limiting rules
circuit_breaker:  # Circuit breaker configuration
plugins:        # Plugin system
ui:             # TUI and dashboard settings
agent:          # Core agent behavior
evolution:      # Skill evolution settings
cost:           # Cost tracking and budgets
workspace:      # Workspace paths and layout
```

---

## YAML Structure Examples

### Minimal Configuration

```yaml
models:
  default:
    provider: anthropic
    model: claude-sonnet-4-20250514
    api_key: ${ANTHROPIC_API_KEY}

channels:
  cli:
    enabled: true
```

### Multi-Channel with Gateway

```yaml
security:
  profile: daemon

gateway:
  enabled: true
  host: 127.0.0.1
  port: 3000
  auth:
    mode: allowlist
    api_tokens:
      - ${GATEWAY_TOKEN}

channels:
  cli:
    enabled: true
  telegram:
    enabled: true
    token: ${TELEGRAM_BOT_TOKEN}
    allow_from: [123456789]
  discord:
    enabled: true
    token: ${DISCORD_BOT_TOKEN}

models:
  default:
    provider: anthropic
    model: claude-sonnet-4-20250514
```

### Restricted Coding Assistant

```yaml
security:
  profile: standard

tools:
  profile: coding
  approval_mode: ask
  blocked:
    - cronjob
    - skill_install
    - process

execution:
  max_turns: 50
  timeout_seconds: 300

cost:
  daily_budget_usd: 10.0
  alert_threshold_pct: 80
```

---

## Profile System

### Security Profiles

The `security.profile` field selects a preset that adjusts multiple security-related defaults:

| Profile | Use Case | Key Behaviors |
|---------|----------|---------------|
| `minimal` | Local development, single user | No auth required, all origins allowed, localhost only |
| `standard` | Daemon mode, trusted network | Token auth enabled, restricted origins, audit logging |
| `extended` | Public-facing gateway | Strict auth, rate limiting, IP allowlisting, full audit |

```yaml
security:
  profile: standard
```

!!! danger
    Never use `minimal` profile when the gateway is exposed beyond localhost. It disables authentication entirely.

### Tool Profiles

The `tools.profile` field controls which tool categories are available:

| Profile | Tools Included | Risk Level |
|---------|---------------|------------|
| `minimal` | Read-only tools (filesystem read, search, web, knowledge) | Low |
| `messaging` | Minimal + media tools (message, send_file, notify, tts, image_gen) | Low–Medium |
| `coding` | Messaging + write tools (filesystem write, patch, shell, code_exec) | Medium |
| `full` | All 30 built-in tools including high-risk (process, cronjob, skill_install) | High |

```yaml
tools:
  profile: coding
  approval_mode: auto    # auto | ask | deny
  blocked:               # explicitly deny specific tools
    - process
    - cronjob
```

!!! tip
    Combine profiles with `approval_mode: ask` to allow powerful tools while keeping a human in the loop. Tools in the `HIGH_RISK` category always prompt regardless of approval mode unless explicitly set to `auto`.

---

## Config Validation and Debugging

Echo Agent provides three CLI subcommands for working with configuration:

### config dump

Print the fully-resolved configuration after all layers are merged:

```bash
echo-agent config dump

# Output as YAML (default)
echo-agent config dump --format yaml

# Output as JSON
echo-agent config dump --format json

# Show only a specific section
echo-agent config dump --section models
```

### config explain

Show where each value came from (which layer set it):

```bash
echo-agent config explain

# Example output:
# models.default.model = "claude-sonnet-4-20250514"
#   └─ source: /home/user/.echo-agent/config.yml (line 4)
#
# gateway.port = 3000
#   └─ source: environment variable ECHO_AGENT_GATEWAY__PORT
#
# security.profile = "standard"
#   └─ source: CLI override (--set security.profile=standard)
```

### config validate

Check configuration for errors without starting the agent:

```bash
echo-agent config validate

# With explicit file
echo-agent config validate -c production.yml
```

Output:

```
✓ Configuration valid (38 sections, 0 errors, 2 warnings)

Warnings:
  - credentials.anthropic_api_key: using environment variable fallback
  - gateway.auth.mode: "open" is not recommended for non-localhost binds
```

---

## Common Patterns

### Environment Variable Interpolation

YAML values support `${VAR}` syntax for environment variable interpolation:

```yaml
models:
  default:
    api_key: ${ANTHROPIC_API_KEY}

channels:
  telegram:
    token: ${TELEGRAM_BOT_TOKEN}
```

!!! warning
    Missing environment variables cause a validation error at startup. Use `${VAR:-default}` syntax for optional values with defaults.

### Per-Channel Model Override

```yaml
models:
  default:
    provider: anthropic
    model: claude-sonnet-4-20250514
  expensive:
    provider: anthropic
    model: claude-opus-4-20250514

channels:
  telegram:
    enabled: true
    model: expensive    # use opus for Telegram conversations
```

### Cost Controls

```yaml
cost:
  daily_budget_usd: 25.0
  alert_threshold_pct: 75
  hard_limit: true       # stop processing when budget exhausted

rate_limit:
  requests_per_minute: 30
  tokens_per_minute: 100000
```

### Workspace-Specific Tool Permissions

```yaml
# .echo-agent/config.yml in a project repo
tools:
  profile: coding
  allowed:
    - filesystem
    - shell
    - code_exec
    - search
    - patch
  blocked:
    - cronjob
    - process

permissions:
  filesystem:
    writable_paths:
      - ./src
      - ./tests
    readable_paths:
      - .
  shell:
    allowed_commands:
      - npm
      - pytest
      - git
```

---

## Common Pitfalls

!!! danger "Don't commit secrets"
    Never put API keys directly in config files that are committed to version control. Use environment variables or a separate `credentials.yml` file that is `.gitignore`d.

!!! warning "Profile overrides"
    Setting `security.profile: extended` may override your explicit `gateway.auth.mode` setting. Use `config explain` to verify the final resolved value.

!!! warning "Nested key syntax"
    In YAML, nested keys use indentation. In environment variables, use double underscores: `ECHO_AGENT_GATEWAY__AUTH__MODE=pairing`. A single underscore is treated as part of the key name.

!!! question "Maintainer confirmation needed"
    Is there support for config file inheritance/composition (e.g., `_extends: base.yml`)? The loading order suggests only one YAML file is active at a time.
