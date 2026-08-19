# Security Profile Matrix

Echo Agent uses a layered security model with two independent profile axes: **security profiles** control system-level protections, while **tool profiles** control what the agent can do.

## Security Profiles (`security.profile`)

| Profile | Bind Address | Gateway Auth | Token Required | Origin Check | Pairing | Use Case |
|---------|-------------|--------------|----------------|--------------|---------|----------|
| `minimal` | `127.0.0.1` | Optional | No | No | Disabled | Local development, single-user |
| `standard` | `127.0.0.1` | Enabled | Yes | Yes | Optional | Personal daemon, daily use |
| `extended` | `0.0.0.0` | Enforced | Yes | Yes | Required | Shared server, exposed gateway |

### `minimal`

Designed for local development and experimentation. No authentication barriers—the agent trusts all local connections.

```yaml
security:
  profile: minimal
```

!!! warning "Not for production"
    The `minimal` profile disables most security checks. Never use it on a machine accessible from a network.

**Characteristics:**

- Gateway binds to localhost only
- No API token required
- No Origin/Host header validation
- Tool approvals default to `auto`
- Cron jobs auto-authorized
- No session isolation

### `standard`

The recommended profile for personal use. Balances convenience with protection.

```yaml
security:
  profile: standard
```

**Characteristics:**

- Gateway binds to localhost
- API token required for all requests
- Origin header validated against `allowed_origins`
- High-risk tools require explicit approval
- Cron jobs require one-time authorization
- Sessions isolated by channel

### `extended`

For deployments exposed to a network or shared among multiple users.

```yaml
security:
  profile: extended
```

**Characteristics:**

- Gateway may bind to all interfaces
- Both API and admin tokens required
- Origin and Host headers strictly validated
- Pairing-based authentication enforced for new clients
- All non-read tools require approval
- Cron jobs require per-execution authorization
- Full audit logging enabled
- Rate limiting enforced

!!! danger "Network exposure"
    When using `extended` with `0.0.0.0` binding, always deploy behind a reverse proxy with TLS termination. Echo Agent does not handle TLS natively.

## Tool Profiles (`tools.profile`)

Tool profiles control which categories of built-in tools the agent may access.

| Profile | Read | Search | Memory | Messaging | Media | Filesystem | Code Exec | Process | Cron | Skill Install |
|---------|------|--------|--------|-----------|-------|------------|-----------|---------|------|---------------|
| `minimal` | ✓ | ✓ | ✓ | — | — | — | — | — | — | — |
| `messaging` | ✓ | ✓ | ✓ | ✓ | ✓ | — | — | — | — | — |
| `coding` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | — | — | — |
| `full` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |

### Tool Categories

| Category | Tools | Risk Level |
|----------|-------|------------|
| MINIMAL_TOOLS | `clarify`, `knowledge`, `memory`, `read_spill`, `search`, `session_search`, `skills`, `todo`, `web`, `vision` | Read-only |
| MESSAGING_TOOLS | `message`, `notify`, `send_file`, `image_gen`, `image_gen_fal`, `tts` | Medium |
| CODING_TOOLS | `browser`, `code_exec`, `document`, `filesystem`, `patch`, `shell`, `skill_run`, `workflow` | High |
| HIGH_RISK_TOOLS | `process`, `cronjob`, `delegate`, `skill_install`, `task` | Critical |

### Approval Modes per Tool

Each tool can be set to one of three approval modes:

| Mode | Behavior |
|------|----------|
| `auto` | Tool executes without user confirmation |
| `ask` | User prompted before each execution |
| `deny` | Tool is completely disabled |

```yaml
tools:
  profile: coding
  overrides:
    shell:
      approval: ask
    process:
      approval: deny
```

## Combined Profile Matrix

The intersection of security and tool profiles determines the effective permissions:

| Security × Tools | `minimal` | `messaging` | `coding` | `full` |
|-----------------|-----------|-------------|----------|--------|
| **`minimal`** | All auto | All auto | All auto | All auto |
| **`standard`** | All auto | All auto | Coding: ask | High-risk: ask |
| **`extended`** | All auto | Messaging: ask | Coding: ask | High-risk: ask, Cron: deny |

!!! tip "Recommended combinations"
    - **Local dev:** `minimal` + `full` — no friction, maximum capability
    - **Personal daily use:** `standard` + `coding` — confirms destructive actions
    - **Shared server:** `extended` + `messaging` — tight controls, audit everything
    - **Public demo:** `extended` + `minimal` — read-only agent, no side effects

## Overriding Profile Defaults

Profiles set baseline permissions. You can override individual settings without changing the profile:

```yaml
security:
  profile: standard
  # Override: disable pairing even though standard supports it
  gateway_auth:
    pairing_ttl_seconds: 0

tools:
  profile: coding
  overrides:
    # Allow shell without prompting (you trust your scripts)
    shell:
      approval: auto
    # Block image generation entirely
    image_gen:
      approval: deny
```

!!! question "Maintainer confirmation needed"
    Can individual security overrides weaken a profile below its documented baseline (e.g., disabling token auth in `extended`)? Current implementation rejects downgrades—confirm this is intended behavior.

## Migrating Between Profiles

### Upgrading (e.g., `minimal` → `standard`)

1. Generate an API token:

    ```bash
    echo-agent gateway --gen-token
    ```

2. Update configuration:

    ```yaml
    security:
      profile: standard
    gateway:
      auth:
        api_tokens:
          - "ea_tok_..."
    ```

3. Restart the gateway:

    ```bash
    echo-agent gateway restart
    ```

4. Update clients with the new token.

### Downgrading (e.g., `extended` → `standard`)

!!! warning "Security implications"
    Downgrading removes protections. Review what each profile disables before proceeding.

1. Ensure no external clients depend on pairing auth.
2. Update configuration.
3. Restart the gateway.
4. Revoke any pairing tokens no longer needed:

    ```bash
    echo-agent gateway --revoke-pairings
    ```

## Configuration Reference

```yaml
security:
  profile: standard          # minimal | standard | extended

tools:
  profile: coding            # minimal | messaging | coding | full
  overrides:                 # per-tool approval overrides
    <tool_name>:
      approval: auto | ask | deny
```

| Environment Variable | Equivalent |
|---------------------|------------|
| `ECHO_AGENT_SECURITY__PROFILE` | `security.profile` |
| `ECHO_AGENT_TOOLS__PROFILE` | `tools.profile` |
| `ECHO_AGENT_TOOLS__OVERRIDES__SHELL__APPROVAL` | `tools.overrides.shell.approval` |

