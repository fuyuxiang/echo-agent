# Configuration Guide

This page covers the **mechanics** of configuration: where files live, how they are loaded, how overrides work, and how validation behaves. For the meaning, type and default of each individual field, see the [configuration reference](configuration.md) — that page is generated from the schema by `echo_agent.config.docgen` and is therefore always in step with the code.

!!! note "How the two pages divide the work"
    This page explains rules; it does not duplicate the field list. A hand-maintained field table drifts as the code evolves, so what follows describes each section's purpose and entry points, and defers to the generated page and `echo_agent/config/schema.py` for specifics.

## Load order

`load_config()` merges four sources in order, each overriding the last:

| Order | Source | Notes |
|-------|--------|-------|
| 1 | `echo_agent/config/default.yaml` | Packaged defaults, shipped with the release |
| 2 | User configuration file | See the lookup rules below |
| 3 | `ECHO_AGENT_` environment variables | See the [environment variable reference](environment-variables.md) |
| 4 | Explicit overrides from the caller | For programmatic use |

The merge is a **deep merge**: only matching leaf fields are replaced, and sibling fields keep their existing values. A user configuration therefore only needs to contain what it changes.

## Configuration file location

When no path is given via `--config`, these filenames are tried in order and the first one that exists is used:

1. `echo-agent.yaml`
2. `echo-agent.yml`
3. `config.yaml`
4. `config.yml`

## Top-level sections

The configuration tree has 33 sections plus one scalar field, `workspace`. Grouped by purpose:

| Group | Sections |
|-------|----------|
| Models and reasoning | `models`, `agent`, `planning`, `compression` |
| Tools and execution | `tools`, `execution`, `permissions`, `security` |
| Channels and gateway | `channels`, `gateway`, `bus`, `a2a` |
| Memory and knowledge | `memory`, `knowledge`, `session`, `spill` |
| Tasks and skills | `scheduler`, `skills`, `evolution`, `multi_agent` |
| Runtime and operations | `runtime`, `storage`, `checkpoint`, `observability` |
| Cost and stability | `cost`, `rate_limit`, `circuit_breaker` |
| Other | `credentials`, `plugins`, `ui`, `validation`, `evaluation`, `media_understanding`, `workspace` |

### Frequently adjusted sections

Only the most commonly changed entry points are listed, with their real defaults.

**models** — models and providers. `default_model` and `fallback_model` select models; `providers` is a list of providers; `routes` splits traffic by task type; `model_windows` overrides context window sizes.

```yaml
models:
  default_model: claude-sonnet-4-5
  providers:
    - name: anthropic          # the provider discriminator, not "type"
      # api_key may be omitted: discovered from ANTHROPIC_API_KEY when empty
      models: [claude-sonnet-4-5]
```

!!! warning "There is no models.primary"
    Providers are described by the `providers` list, whose discriminator is `name` rather than `type`; models are chosen with `default_model` / `fallback_model`. A structure like `models.primary.provider` does not raise an error — pydantic treats it as an unknown key and **silently ignores it**, leaving you with an empty model configuration. See the [model configuration guide](../guides/models/index.md).

**tools** — tool admission. `profile` picks the tier (default `full`); `allow` / `also_allow` / `deny` give finer control; `restrict_to_workspace` limits file operations. Individual tool switches are nested sections, such as `tools.exec`, `tools.browser` and `tools.web`.

**security** — a single field, `profile`, accepting `personal_cli` (default), `daemon` or `public_gateway`.

**permissions** — approval and elevation, containing `approval`, `elevated` and `admin_users`. The approval mode lives at `permissions.approval.mode` and accepts `manual`, `smart` (default) or `off`.

```yaml
tools:
  profile: coding
security:
  profile: daemon
permissions:
  approval:
    mode: smart
```

For the full admission and approval evaluation order, see the [security profile matrix](security-profile-matrix.md).

**gateway** — the HTTP/WebSocket gateway. `enabled` defaults to `false`; `host` to `127.0.0.1`; `port` to `58123`; `api_prefix` to `/api/v1`; `ws_path` to `/ws`. Authentication lives under `gateway.auth`, session behaviour under `gateway.session_policy`.

```yaml
gateway:
  enabled: true
  host: 127.0.0.1      # 0.0.0.0 exposes it beyond localhost; enable auth first
  port: 58123
```

**execution** — execution backends. `default_executor` defaults to `sandbox`; `network_policy` defaults to `deny`, and must be `allow` or `restricted` for outbound access.

**observability** — logging and tracing. `log_level` defaults to `INFO`; `trace_enabled` and `otel_enabled` are on by default; nothing is exported while `otel_endpoint` is empty.

**cost** — cost controls. `enabled` defaults to `false`; `daily_budget_usd` to `0.0` (no limit); `soft_threshold_ratio` to `0.8`.

**rate_limit** — throttling. `session_rpm` defaults to `20`, `session_burst` to `5`.

**circuit_breaker** — `failure_threshold` defaults to `5`, `recovery_seconds` to `60.0`, `half_open_max` to `2`.

**checkpoint** — workspace snapshots. `enabled` defaults to `true`; `max_snapshots_per_workspace` to `20`. Snapshots deliberately **exclude** the database, session, memory and log directories: a file-level snapshot of a live SQLite file is a torn read.

**memory** — the memory system, with 40-plus fields covering tiering, embeddings, reranking and contradiction detection. It is enabled by default with a local embedding model and rarely needs tuning; see [memory system](../concepts/memory-system.md).

### Field naming

Configuration accepts both snake_case and camelCase: `allow_from` and `allowFrom` are equivalent. This documentation uses snake_case consistently, while `echo-agent config dump` emits camelCase. Mixing them does not affect parsing.

## Environment variable interpolation

!!! warning "The configuration file does not interpolate variables"
    A `${VAR}` in a configuration value is **not** expanded — it is kept verbatim as a literal string. Writing `api_key: "${ANTHROPIC_API_KEY}"` stores that exact text as the API key.

There are two correct approaches. First, keep credentials out of the file entirely — provider API keys are discovered from conventional environment variables:

```bash
export ANTHROPIC_API_KEY=sk-ant-...
```

Second, override the option through an environment variable (scalar fields only; list types are not supported):

```bash
export ECHO_AGENT_MODELS__DEFAULT_MODEL=claude-sonnet-4-5
```

See the [environment variable reference](environment-variables.md) for the discovery table and override rules.

## Validation

Configuration is validated by pydantic, and its behaviour splits into two cases that matter for troubleshooting:

- **Invalid type or value** — startup fails immediately with an error. For example writing `standard` for `security.profile` (only three values are legal), or a non-numeric `gateway.port`.
- **Unknown field** — silently ignored, with no error and no warning. A misspelled field name or an invented structure therefore presents as "my setting had no effect" rather than as a failure.

Check a configuration before starting:

```bash
echo-agent config validate
```

The `config` subcommand has four actions:

| Command | Purpose |
|---------|---------|
| `echo-agent config validate` | Validate the configuration |
| `echo-agent config dump` | Print the merged, effective configuration (credentials redacted); accepts `--format json` |
| `echo-agent config explain <key>` | Explain one option, addressed by dotted path |
| `echo-agent config gen-docs` | Regenerate the configuration reference page |

When a setting appears not to take effect, suspect the second case first: run `echo-agent config dump` to see the merged value, or `echo-agent config explain gateway.port` to confirm the path is spelled correctly.

## Related pages

- [Configuration reference](configuration.md) — per-option reference generated from the schema
- [Environment variable reference](environment-variables.md) — override rules and credential variables
- [Security profile matrix](security-profile-matrix.md) — tool admission and approval
