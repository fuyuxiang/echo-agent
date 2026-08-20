# Security Profile Matrix

This page describes how Echo Agent decides which tools reach the model and which calls need approval. Every value below is taken from `echo_agent/config/schema.py` and `echo_agent/security/tool_policy.py`.

!!! warning "The two `profile` fields are not interchangeable"
    Configuration has two fields named `profile`. Their value sets are disjoint, and mixing them up silently leaves your intent unapplied:

    - `tools.profile` — `minimal` / `messaging` / `coding` / `full`, default `full`
    - `security.profile` — `personal_cli` / `daemon` / `public_gateway`, default `personal_cli`

    There are no `standard`, `extended` or `strict` profiles. An undefined value is rejected by configuration validation at startup.

## tools.profile: the tool allowlist

The four tiers are cumulative — each one contains every tool from the tier before it.

| Profile | Tools | Intended use |
|---------|-------|--------------|
| `minimal` | 14 | Read-only Q&A; no file writes, no media generation |
| `messaging` | 18 | Adds memory and media generation on top of `minimal` |
| `coding` | 24 | Adds file writes and orchestration on top of `messaging` |
| `full` | all | Allowlist is `*`, permitting every tool (default) |

For the exact tool list per tier, see the profile table in the [built-in tool reference](tools.md).

The `full` allowlist is the literal `*`, so newly added tools become available there automatically. The other three tiers are explicit sets, and new tools do not enter them on their own.

## security.profile: the deployment baseline

`security.profile` does not change the allowlist. It adds deny rules on top of it, either by tool name or by capability tag.

| Profile | Meaning | Additional denials |
|---------|---------|--------------------|
| `personal_cli` | Single user on their own machine (default) | none |
| `daemon` | Long-running background service | 4 tools + 4 capabilities |
| `public_gateway` | Gateway exposed beyond localhost | 11 tools + 8 capabilities |

### daemon

Denied tools: `exec`, `execute_code`, `process`, `skill_install`

Denied capabilities: `code.exec`, `process.exec`, `process.manage`, `skill.install`

### public_gateway

Denied tools are the 6 members of `HIGH_RISK_TOOLS` (`cronjob`, `exec`, `execute_code`, `process`, `skill_install`, `skill_manage`) plus 5 write-capable tools (`edit_file`, `knowledge_index`, `patch`, `workflow`, `write_file`), 11 in total.

Denied capabilities: `code.exec`, `fs.write`, `process.exec`, `process.manage`, `scheduler.write`, `skill.install`, `skill.write`, `workflow.write`

!!! danger "Before exposing the gateway"
    Setting `security.profile: public_gateway` is not sufficient on its own. Confirm that authentication is enabled and that the listen address and allowed origins are restricted. The gateway listens on `127.0.0.1` by default; listening beyond localhost is a change you must make deliberately. See [security hardening](../operations/security-hardening.md).

## Evaluation order

`is_tool_allowed()` evaluates layers in a fixed order and stops at the first denial:

1. **Explicit deny** — a tool named in `tools.deny` is rejected. This layer has the highest precedence and cannot be waived by any other setting.
2. **Allowlist** — if `tools.allow` is non-empty it becomes the only allowlist and the profile no longer participates; otherwise the `tools.profile` tier or `tools.also_allow` decides.
3. **Deployment denials** — the tool-name and capability rules added by `security.profile`. A tool also named in `tools.allow` or `tools.also_allow` is exempt from this layer.
4. **Network policy** — when `execution.network_policy` is `deny`, this rejects `web_fetch`, `web_search` and any tool carrying the `network.outbound` capability. Note that `deny` is the default.

A denied tool does not raise an error: it is simply absent from the model's tool list, and one INFO-level line is logged — `Tool policy skipped N tools`. When investigating "the agent never called my tool", check that log line first.

### Related options

| Option | Type | Default | Purpose |
|--------|------|---------|---------|
| `tools.deny` | list[str] | `[]` | Tool names to reject unconditionally |
| `tools.allow` | list[str] | `[]` | When non-empty, the sole allowlist, overriding the profile |
| `tools.also_allow` | list[str] | `[]` | Permits tools beyond the profile and exempts them from deployment denials |
| `tools.profile` | enum | `full` | Tier allowlist |
| `security.profile` | enum | `personal_cli` | Deployment baseline |
| `execution.network_policy` | `allow` / `deny` / `restricted` | `deny` | Outbound network policy |

## Approval

Passing admission does not mean a call runs unattended — it may still require approval. Approval settings live under `permissions.approval`, not under `security`: `SecurityConfig` has exactly one field, `profile`.

| Option | Type | Default | Purpose |
|--------|------|---------|---------|
| `mode` | `manual` / `smart` / `off` | `smart` | Approval mode |
| `default_policy` | `approve` / `deny` / `ask` | `approve` | Fallback when no specific rule matches |
| `require_approval` | list[str] | see below | Tools that require approval |
| `auto_approve` | list[str] | `[]` | Tools approved automatically |
| `auto_deny` | list[str] | `[]` | Tools denied automatically |
| `cli_auto_approve` | bool | `true` | Auto-approve on the CLI channel |
| `trusted_channels` | list[str] | `[]` | Channels exempt from approval |
| `unattended_policy` | `deny` / `allow_safe` | `deny` | Behaviour when nobody is present |
| `wait_timeout_seconds` | int | `300` | How long to wait for a human |
| `smart_model` | str | `""` | Model used by `smart` mode |

`require_approval` defaults to 9 entries: `cronjob`, `delegate_task`, `dep_install`, `exec`, `execute_code`, `process`, `skill_install`, `skill_manage`, `spawn_task`.

!!! note "Valid approval modes"
    `mode` accepts `manual`, `smart` and `off`. `auto`, `ask` and `deny` are not modes — of those, `approve`/`deny`/`ask` belong to `default_policy`.

### Elevation

`permissions.elevated` temporarily relaxes restrictions:

| Option | Type | Default | Purpose |
|--------|------|---------|---------|
| `enabled` | bool | `false` | Whether elevation is available |
| `allow_from` | dict | `{}` | Which sources may request it |

## Examples

This configuration keeps the `coding` tier, permits `exec`, and retains every other restriction of the daemon shape:

```yaml
tools:
  profile: coding
  also_allow:
    - exec          # also exempts exec from the daemon denial

security:
  profile: daemon

permissions:
  approval:
    mode: smart
    require_approval:
      - exec
```

To disable a tool outright, use `tools.deny` rather than removing it from an allowlist — `deny` is the first layer evaluated and cannot be bypassed by `also_allow` or elevation:

```yaml
tools:
  deny:
    - skill_install
```

## Related pages

- [Built-in tool reference](tools.md) — parameters and capability tags for all 36 tools
- [Configuration reference](configuration.md) — per-option reference generated from the schema
