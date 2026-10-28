# Environment Variables

Echo Agent uses `ECHO_AGENT_` variables to override configuration fields. The mapping is implemented in `echo_agent/config/loader.py`. A few runtime variables are read directly by other modules.

## Configuration overrides

Remove the `ECHO_AGENT_` prefix, lowercase the remainder, and use a double underscore (`__`) for each configuration level. A single underscore stays within a field name. For example:

| Configuration path | Environment variable |
|---|---|
| `gateway.host` | `ECHO_AGENT_GATEWAY__HOST` |
| `gateway.port` | `ECHO_AGENT_GATEWAY__PORT` |
| `gateway.api_prefix` | `ECHO_AGENT_GATEWAY__API_PREFIX` |
| `permissions.approval.mode` | `ECHO_AGENT_PERMISSIONS__APPROVAL__MODE` |
| `execution.network_policy` | `ECHO_AGENT_EXECUTION__NETWORK_POLICY` |
| `models.default_model` | `ECHO_AGENT_MODELS__DEFAULT_MODEL` |

The packaged configuration sets `gateway.port` to `58123`, `execution.default_executor` to `local`, and `execution.network_policy` to `allow`. The latter two schema defaults are `sandbox` and `deny`; `tools.exec.host` separately defaults to `sandbox`. Inspect the effective values with `echo-agent config dump`.

An incorrectly structured name, such as `ECHO_AGENT_GATEWAY_PORT`, maps to an unknown top-level field and is ignored. See the generated [configuration reference](configuration.en.md) for valid paths.

Scalar values remain strings until Pydantic validates them. Write integers as digits and booleans as `true` or `false`. Strings such as a token with the literal value `false` remain strings. List and dictionary fields accept JSON:

```bash
export ECHO_AGENT_GATEWAY__PORT=9000
export ECHO_AGENT_PERMISSIONS__ELEVATED__ENABLED=true
export ECHO_AGENT_GATEWAY__AUTH__ADMIN_TOKENS='["ephemeral-token"]'
```

Nested dictionary entries, such as `tools.mcp_servers`, can also be addressed by key: `ECHO_AGENT_TOOLS__MCP_SERVERS__MYSRV__ARGS='["-m", "myserver"]'`. Avoid `__` within dictionary keys because it is the hierarchy separator. Validate overrides with `echo-agent config validate`.

## Loading precedence

`load_config()` merges, in order, the packaged `echo_agent/config/default.yaml`, a user configuration file, `ECHO_AGENT_` environment overrides, and caller-supplied explicit overrides. Later values take precedence. Camel case keys in YAML are normalized to snake case when read, so `networkPolicy` and `network_policy` address the same field. Candidate user filenames are `echo-agent.yaml`, `echo-agent.yml`, `config.yaml`, and `config.yml`.

## Provider credentials

Model provider API keys can be discovered through conventional variables:

| Provider | Variables |
|---|---|
| OpenAI | `OPENAI_API_KEY` |
| Anthropic | `ANTHROPIC_API_KEY` |
| Gemini / Google | `GOOGLE_API_KEY` or `GEMINI_API_KEY` |
| OpenRouter | `OPENROUTER_API_KEY` |
| Bedrock / AWS | Standard AWS SDK credential chain, including `AWS_ACCESS_KEY_ID`, `AWS_REGION`, and `AWS_PROFILE` |

To use a different variable name, configure `api_key_env` on a `models.providers` list entry:

```yaml
models:
  providers:
    - name: openai
      api_key_env: MY_HOST_INJECTED_KEY
```

An explicit `api_key` takes precedence over `api_key_env`, which takes precedence over provider-specific discovery. YAML does not interpolate environment placeholders in strings; use `api_key_env` instead of placing a placeholder in `api_key`.

## Direct runtime variables

These are read outside the configuration override mechanism:

| Variable | Purpose |
|---|---|
| `ECHO_AGENT_CREDENTIAL_KEY` | Default environment variable name for the credential encryption key; configurable through `credentials.encryption_key_env` |
| `ECHO_AGENT_DISABLE_LAZY_INSTALLS` | Disables on-demand dependency installation |
| `ECHO_AGENT_SETUP_HANDLES_SERVICE` | Marks service registration as handled by the setup workflow |

Other integrations may require their own variables, such as `FAL_KEY` for FAL.ai. See the relevant integration and skill documentation.

## Related pages

- [Configuration reference](configuration.en.md)
- [Security profile matrix](security-profile-matrix.en.md)
