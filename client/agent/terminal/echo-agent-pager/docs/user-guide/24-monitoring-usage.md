# Monitoring Usage (External OpenTelemetry)

> **Status: alpha.** The schema below is versioned (`echo_agent_code.schema.version = v1`);
> additive changes may occur without notice, renames/removals will bump the
> version and be called out in the changelog.

EchoAgent CLI can export usage **metrics** and **events** to your organization's
own OpenTelemetry collector, so platform teams can monitor adoption, token
consumption, tool-permission decisions, and errors across the fleet — without
any data flowing through EchoAgent.

## Related settings

These knobs are independent of each other (and of this guide's external OTEL stream):

| Setting | How to set it |
|---------|---------------|
| Telemetry master switch | `[features] telemetry` / `ECHO_AGENT_TELEMETRY_ENABLED` |
| Coding data, retention, and training | Settings — `/privacy` opens the row |
| Trace upload | `[telemetry] trace_upload` / `ECHO_AGENT_TELEMETRY_TRACE_UPLOAD` |
| External OpenTelemetry | `ECHO_AGENT_EXTERNAL_OTEL` / `[telemetry] otel_*` (this guide) |

See also [Authentication](02-authentication.md#related-settings) and
[Configuration](05-configuration.md#telemetry).

## External OTEL stream

The external stream is:

- **Off by default**, and requires a *double opt-in* (a master switch **and**
  an explicit exporter selection).
- **Content-free by default**: no prompts, no code, no file paths (extension
  only), no tool arguments, no bash commands, and MCP/skill/plugin names
  collapsed to categories. Optional content gates re-enable some of these.
- **Structurally separate** from EchoAgent-internal telemetry: its exporters carry
  only the headers you configure, never EchoAgent credentials.
- **Independent of EchoAgent data-retention opt-outs**: it works even when
  `telemetry` is disabled and for ZDR (zero-data-retention) teams. Those
  settings govern EchoAgent-side retention; the external stream is governed solely
  by your own OTEL configuration.

## Quick start

```bash
export ECHO_AGENT_EXTERNAL_OTEL=1                  # master switch
export OTEL_METRICS_EXPORTER=otlp
export OTEL_LOGS_EXPORTER=otlp
export OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf  # or grpc
export OTEL_EXPORTER_OTLP_ENDPOINT=https://collector.corp.example:4318
export OTEL_EXPORTER_OTLP_HEADERS="Authorization=Bearer <collector-token>"
echoagent
```

`ECHO_AGENT_EXTERNAL_OTEL=1` alone enables **nothing** — you must also select at
least one exporter. Conversely, the `OTEL_*` vars alone enable nothing
without the master switch.

## Environment variables

| Variable | Default | Meaning |
|---|---|---|
| `ECHO_AGENT_EXTERNAL_OTEL` | `0` | Master switch. Distinct from `ECHO_AGENT_TELEMETRY_ENABLED`, which controls EchoAgent-internal product analytics — the two govern opposite-pointing data flows. |
| `OTEL_METRICS_EXPORTER` | `none` | `otlp` \| `console` \| `none`. |
| `OTEL_LOGS_EXPORTER` | `none` | `otlp` \| `console` \| `none`. Gates the event stream. |
| `OTEL_EXPORTER_OTLP_PROTOCOL` | `http/protobuf` | `http/protobuf` \| `grpc`. Base protocol for both signals. |
| `OTEL_EXPORTER_OTLP_LOGS_PROTOCOL` / `..._METRICS_PROTOCOL` | — | Per-signal protocol overrides (same values as the base protocol). Unrecognized values disable the stream. |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | `http://localhost:4318` for HTTP, `http://localhost:4317` for gRPC | Base endpoint. For `http/protobuf`, `/v1/logs` and `/v1/metrics` are appended per the OTLP spec; for `grpc`, the collector endpoint is used as-is. Path appending uses **that signal’s** protocol. |
| `OTEL_EXPORTER_OTLP_LOGS_ENDPOINT` / `..._METRICS_ENDPOINT` | — | Signal-specific overrides, used verbatim. For gRPC these should normally be collector endpoints without `/v1/...` paths. |
| `OTEL_EXPORTER_OTLP_HEADERS` (+ signal-specific variants) | — | Collector auth (`k=v,k2=v2`). The **only** headers the external exporters send, and the only supported collector-auth mechanism (no config-file headers key — tokens never live on disk). |
| `OTEL_EXPORTER_OTLP_CERTIFICATE` (+ signal-specific variants) | — | Path to a PEM bundle with additional trusted CA certificate(s) for verifying the collector — for collectors behind a private/corporate CA. Additive to the default trust roots (system store and embedded Mozilla roots). Also settable via `[telemetry] otel_certificate`. |
| `OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE` / `OTEL_EXPORTER_OTLP_CLIENT_KEY` (+ signal-specific `…_LOGS_…` / `…_METRICS_…` variants) | — | PEM **paths** for mTLS client identity. Both cert and key must be set (base or same signal); half-config is ignored with a warning. Unencrypted PEM keys only. Also settable via `[telemetry] otel_client_certificate` / `otel_client_key`. |
| `OTEL_EXPORTER_OTLP_TIMEOUT` | `10000` (ms) | Export timeout. |
| `OTEL_METRIC_EXPORT_INTERVAL` | `60000` (ms) | Metric export interval. |
| `OTEL_BLRP_SCHEDULE_DELAY` (or alias `OTEL_LOGS_EXPORT_INTERVAL`) | `5000` (ms) | Log batch interval. |
| `OTEL_EXPORTER_OTLP_METRICS_TEMPORALITY_PREFERENCE` | `delta` | `delta` \| `cumulative`. |
| `OTEL_METRICS_INCLUDE_SESSION_ID` | `1` | Attach `session.id` to metrics (cardinality opt-out). |
| `OTEL_METRICS_INCLUDE_VERSION` | `0` | Attach `app.version` to metrics. |
| `OTEL_LOG_USER_PROMPTS` | `0` | Content gate: prompt text on `echo_agent_code.user_prompt` (60 KB cap, secret-scrubbed). |
| `OTEL_LOG_TOOL_DETAILS` | `0` | Content gate: tool parameters (4 KB cap), full file paths, verbatim MCP/skill/plugin names. Bash command text is **never** exported in v1, even with this gate. |

`OTEL_RESOURCE_ATTRIBUTES` is deliberately ignored: the resource is built
from a fixed, audited attribute set.

> **Migration note:** older releases could share `OTEL_EXPORTER_OTLP_*` with
> the product's own analytics pipeline. That behavior is deprecated: when
> `ECHO_AGENT_EXTERNAL_OTEL` is set, product analytics ignores those vars, and the
> CLI refuses to activate the external stream in any configuration where
> product analytics already consumed them — your collector only receives the
> external stream you opted into.

## Config file

Org defaults live under the existing `[telemetry]` table in `config.toml`
(env vars win). The keys are `otel_`-prefixed peers of the other
`[telemetry]` settings:

```toml
[telemetry]
otel_enabled = true
otel_metrics_exporter = "otlp"
otel_logs_exporter = "otlp"
otel_endpoint = "https://collector.corp.example:4318"
otel_protocol = "http/protobuf"  # or "grpc"
# Optional PEM *paths* for private-CA trust and mTLS (never PEM contents):
otel_certificate = "/etc/ssl/corp-ca.pem"
otel_client_certificate = "/etc/ssl/client.crt"
otel_client_key = "/etc/ssl/client.key"
otel_log_user_prompts = false   # admins can pin these via requirements
otel_log_tool_details = false
```

The config keys are `otel_*` under `[telemetry]`; the **env vars keep their
standard OTEL names** (`ECHO_AGENT_EXTERNAL_OTEL`, `OTEL_*`) for ecosystem
interop, so the two layers use deliberately different namespaces. The
`otel_protocol` config key maps to `OTEL_EXPORTER_OTLP_PROTOCOL`. Env vars
win over config file paths for CA and client identity.

There is deliberately no `headers` key: supply collector auth via
`OTEL_EXPORTER_OTLP_HEADERS` so tokens are never stored on disk. Certificate
and key config keys are **paths only** — never embed private key material
in TOML.

The external stream exports **logs and metrics only** (no customer-facing
traces exporter).

Managed deployments can additionally enable org-wide telemetry by distributing
the `[telemetry]` `otel_*` keys through `echoagent setup` managed config /
requirements pins, or force-disable it fleet-wide with the same local config
layers (`external_otel_disabled`, content-gate locks).

## Startup suppression (why nothing arrives for the first few seconds)

Because EchoAgent can force-disable this stream fleet-wide, the CLI holds emission
closed at startup until it knows whether that switch is set — it fetches the
fleet policy from `/v1/settings` and only then starts exporting. In a healthy
setup that is well under a second and invisible.

**The wait is bounded**, so a deployment that cannot reach EchoAgent still exports:

- If no fleet policy can apply at all — `[features] remote_fetch = false`, or
  `[endpoints] cli_chat_proxy_base_url` points somewhere other than EchoAgent — the
  stream starts immediately, governed by your local configuration.
- If the policy fetch fails or never completes (firewalled host, offline
  laptop), emission starts anyway once the attempt is exhausted, and in all
  cases no later than 30 seconds after startup.

A fleet policy that arrives afterwards still applies; it can only ever
*tighten* (disable the stream or force the content gates off), never enable
something your local configuration did not.

If your collector receives nothing at all, check the debug log
(`echoagent --debug`) for `external otel:` lines — they record whether the stream
resolved its configuration, and whether it is exporting or suppressed.

## Resource attributes

| Attribute | Value |
|---|---|
| `service.name` | `echo-agent-cli` |
| `service.version`, `client.version` | build/client versions |
| `app.entrypoint` | `cli` \| `headless` \| `agent` |
| `terminal.type` | terminal emulator brand |
| `echo_agent_code.schema.version` | `v1` |

Identity attributes (`user.id`, and `organization.id` / `team.id` /
`deployment.id` when known) are attached per metric data point and per event
once authentication completes. `prompt.id` (per-prompt UUID) appears on
events only, never metrics.

## Metrics (meter scope `com.echoagent.agent_code`)

| Metric | Unit | Attributes |
|---|---|---|
| `echo_agent_code.session.count` | `{session}` | base attrs only |
| `echo_agent_code.token.usage` | `{token}` | `type` = `input` \| `output` \| `reasoning` \| `cache_read`; `model` |
| `echo_agent_code.turn.count` | `{turn}` | `outcome` = `completed` \| `cancelled` \| `error`; `model` |
| `echo_agent_code.tool.decision` | `{decision}` | `tool_name`, `decision` = `allow` \| `deny` \| `cancelled` \| `followup`, `access_kind`, `permission_mode` |
| `echo_agent_code.tool.usage` | `{call}` | `tool_name`, `outcome` |
| `echo_agent_code.error.count` | `{error}` | `error_category`, `model` |
| `echo_agent_code.startup.total` | `ms` | `outcome` = `ok` \| `timeout` \| `error`; `auth_mode` |
| `echo_agent_code.startup.phase_duration` | `ms` | `phase`, `outcome`, `auth_mode` |
| `echo_agent_code.startup.timeout` | `{timeout}` | `stuck_in`, `auth_mode` |

`startup.total` measures process start to a usable session, recorded once per
process; `outcome` = `timeout` or `error` means startup ended without one.
`phase_duration` breaks the connect attempt down by step (`config_load`,
`managed_policy`, `bootstrap`, `model_catalog`, `worker_spawn`,
`leader_connect`, `acp_initialize`, `eager_auth`); filter on its `outcome`
(`ok` | `timeout` | `cancelled` | `error`) so truncated samples do not skew
`ok` percentiles. The later `app_init`
and `session_create` phases appear in the log timeline and the summary
strings, not in this metric. `stuck_in` on a timeout names the step that had
not finished. That is often not the step that took the longest, because a step
that runs without pausing finishes before the timeout is recorded. The error
message EchoAgent prints names the longest step instead, so the two can name
different steps for the same timeout. Use `phase_duration` to compare them.
`auth_mode` is `personal`, `team`, `deployment`, or `unknown`:
startup cost differs by kind, so split by it before comparing.

There is no `cost.usage` metric: join `echo_agent_code.token.usage` with your own
price sheet. `lines_of_code.count` and `active_time.total` are planned for a
later phase.

`tool_name` values: built-in tool names pass verbatim; MCP tools collapse to
`mcp_tool` and other non-built-in tools to `custom_tool` unless
`OTEL_LOG_TOOL_DETAILS=1`.

## Events (OTLP log records)

Every event carries `event.sequence`, `session.id`, `turn_number` (in-turn),
`prompt.id`, plus the identity attributes. Gate legend: **details** =
requires `OTEL_LOG_TOOL_DETAILS`, **prompts** = requires
`OTEL_LOG_USER_PROMPTS`; everything else always exports while the stream is
active.

| `event.name` | Attributes |
|---|---|
| `echo_agent_code.session_start` | `model`, `permission_mode`, `mcp_server_count`, `plugin_count`, `skill_count`, `hook_count`, `memory_enabled`, `is_git_repo`, `client_identifier` |
| `echo_agent_code.session_end` | `duration_secs`, `turn_count`, `tool_call_count`, `compaction_count`, `model` |
| `echo_agent_code.user_prompt` | `prompt_length`, `model`, `screen_mode?` (`fullscreen` \| `inline` \| `minimal` \| `headless` \| `other`); `prompt` (**prompts**) |
| `echo_agent_code.turn_completed` | `outcome`, `duration_ms`, `tool_call_count`, `model`, `error_category?`, `cancellation_category?` |
| `echo_agent_code.api_request` | `model`, `duration_ms`, `stop_reason?`, `input_tokens`, `output_tokens`, `reasoning_tokens`, `cache_read_tokens` |
| `echo_agent_code.api_error` | `error_category`, `model`, `status_code?`, `duration_ms?` |
| `echo_agent_code.tool_result` | `tool_name`, `outcome`, `success`, `duration_ms`, `file_extension`; `tool_parameters`, `file_path` (**details**) |
| `echo_agent_code.tool_decision` | `tool_name`, `decision`, `access_kind`, `permission_mode`, `source` |
| `echo_agent_code.mcp_server_connection` | `status`, `transport_type`, `duration_ms`, `tool_count?`, `error_type?`; `mcp_server.name` (**details**; collapsed to `mcp_server` otherwise) |
| `echo_agent_code.permission_mode_changed` | `to_mode`, `trigger` |
| `echo_agent_code.skill_activated` | `skill_source`, `trigger` = `slash_command` \| `skill_md_read` \| `skill_tool`; `skill.name` (**details**) |
| `echo_agent_code.plugin_loaded` | `install_kind?`, `success`, `error_category?`; `plugin_name` (**details**) |
| `echo_agent_code.compaction` | `duration_ms`, `tokens_before`, `tokens_after`, `model?` |
| `echo_agent_code.subagent` | `phase` = `launched` \| `completed`, `subagent_type?`, `outcome?`, `duration_ms?` |
| `echo_agent_code.auth` | `auth_method` |
| `echo_agent_code.internal_error` | `error_type` (class only — no message, no location) |
| `echo_agent_code.model_switched` | `from_model`, `to_model`, `success`, `error_code?` |

## Privacy model

Three independent fail-closed mechanisms guard the wire format:

1. A **typed schema**: attribute keys are a closed enum; nothing outside it
   can be attached.
2. **Emit-time redaction**: every string passes a secret-shape scrub and a
   home-directory scrub, with truncation (512→128 chars per value, 4 KB tool
   params, 60 KB prompt cap).
3. **Export-time validators**: any record carrying a non-schema key, a
   closed-gate key, or an unscrubbed secret shape is dropped before leaving
   the process; metric exports with out-of-schema attribute keys are dropped
   entirely.

Never exported: bash command text, error message bodies, prompt text
(without the gate), file paths (without the gate), `api_key.id`, machine
fingerprints, email addresses, subscription tier.

## Example collector config

```yaml
receivers:
  otlp:
    protocols:
      http:
        endpoint: 0.0.0.0:4318
      grpc:
        endpoint: 0.0.0.0:4317

processors:
  batch:

exporters:
  prometheus:
    endpoint: 0.0.0.0:9464

service:
  pipelines:
    metrics:
      receivers: [otlp]
      processors: [batch]
      exporters: [prometheus]
    logs:
      receivers: [otlp]
      processors: [batch]
      exporters: []   # point at your log backend (loki, elasticsearch, …)
```

Example queries (PromQL, with the Prometheus exporter above):

```promql
# Tokens by model and type across the org, 1h rate
sum by (model, type) (rate(echo_agent_code_token_usage_total[1h]))

# Sessions per team per day
sum by (team_id) (increase(echo_agent_code_session_count_total[1d]))

# Tool-permission denial ratio
sum(rate(echo_agent_code_tool_decision_total{decision="deny"}[1h]))
  / sum(rate(echo_agent_code_tool_decision_total[1h]))
```

## Debugging

Set `OTEL_LOGS_EXPORTER=console` / `OTEL_METRICS_EXPORTER=console` to print
redacted records to **stderr** (suppressed in `agent`/`headless` entrypoints
to keep captured logs clean). Export errors never surface in the TUI; check
the debug log.
