# Configuration reference

This file ships with the CLI and is extracted to `~/.echo-agent/docs/user-guide/26-config-reference.md` on launch. It is the complete field list for `config.toml`, `managed_config.toml`, and `requirements.toml`. For conceptual guidance see [05-configuration.md](05-configuration.md).

## How to configure

Three files configure EchoAgent Build, and they are written by different people.

| File | Who writes it | Where it lives | Use it to |
| --- | --- | --- | --- |
| `config.toml` | The developer | `~/.echo-agent/config.toml`, and `.echo-agent/config.toml` in a project | Set personal defaults. Anything here can be changed by the person using the machine. |
| `managed_config.toml` | You, through the console or a deployment tool | `/etc/echoagent/managed_config.toml` | Ship a starting point to a fleet. A developer's own file overrides it. |
| `requirements.toml` | You, signed | `/etc/echoagent/requirements.toml`, or macOS device management | Set values a developer cannot change. Keys marked `pin` below hold against every other file, the environment, and the command line. |

Choose `managed_config.toml` for defaults you want people to be able to adjust, and `requirements.toml` for the ones you do not.

EchoAgent Build also reads these layers, later rows winning except where a requirements pin or the Managed column says otherwise.

1. Compiled defaults.
2. `/etc/echoagent/managed_config.toml`, then `$ECHO_AGENT_HOME/managed_config.toml` (fleet defaults; console-synced).
3. `$ECHO_AGENT_HOME/config.toml` (your settings; `/settings` writes here). Default `$ECHO_AGENT_HOME` is `~/.echo-agent`.
4. Project `.echo-agent/config.toml`: only `[mcp_servers]`, `[plugins]`, `[permission]`, and `[mcp] max_output_bytes`.
5. `ECHO_AGENT_CONFIG` (inline JSON) or `ECHO_AGENT_CONFIG_PATH` (JSON or TOML file). Allowlisted keys only.
6. `$ECHO_AGENT_HOME/requirements.toml`, then `/etc/echoagent/requirements.toml`, then macOS MDM `ai.x.echo-agent`. Admin layer. Keys marked `pin` in the table cannot be overridden; keys marked `yes` are also valid in this file.
7. `ECHO_AGENT_*` environment variables.
8. CLI flags such as `--model`, `--sandbox`, `--yolo`.

Run `echoagent inspect` or `echoagent inspect --json` to see which files and values won.

## config.toml

User-level configuration lives in `$ECHO_AGENT_HOME/config.toml` (default `~/.echo-agent/config.toml`; Windows `%USERPROFILE%\.echo-agent\config.toml`). Project-scoped overrides live in `.echo-agent/config.toml` and only contribute `[mcp_servers]`, `[plugins]`, `[permission]`, and `[mcp] max_output_bytes`.

**Requirements** marks whether the same key can be set in `requirements.toml`: `pin` cannot be overridden (including env and CLI where the resolver honors the pin); `yes` is accepted in that file; `—` is not read from `requirements.toml`. **Managed** marks whether a fleet `managed_config.toml` value stands (`fleet`) or the user's file wins (`user`).

### `agent`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `agent.definition` | `string (path)` | `yes` | `user` | Path to an agent definition markdown file with YAML frontmatter. |
| `agent.name` | `string` | `yes` | `user` | Built-in or discovered agent definition name. Also ECHO_AGENT_AGENT and `--agent-profile`. |
| `agent.system_prompt_label` | `string` | `yes` | `user` | Global system-prompt identity; per-model override wins. |

### `announcements`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `announcements` | `array of tables` | `—` | `user` | Remote announcement payloads consumed at load. Not a user-authored table. |

### `auth`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `auth` | `table` | `yes` | `user` | Alias of `[echo_agent_com_config]`; every `echo_agent_com_config.*` key also works as `auth.*`. |
| `auth.auth_provider_command` | `string` | `yes` | `user` | External auth binary; stdout is the token. Also ECHO_AGENT_AUTH_PROVIDER_COMMAND; also valid as `echo_agent_com_config.auth_provider_command`. |
| `auth.auth_provider_label` | `string` | `yes` | `user` | Login button label for an external auth provider. Also ECHO_AGENT_AUTH_PROVIDER_LABEL; also valid as `echo_agent_com_config.auth_provider_label`. |
| `auth.auth_token_ttl` | `number` | `yes` | `user` | Token TTL in seconds for providers that return a bare token. Also ECHO_AGENT_AUTH_TOKEN_TTL; also valid as `echo_agent_com_config.auth_token_ttl`. |
| `auth.disable_api_key_auth` | `boolean` | `pin` | `user` | Refuse API-key auth so only the deployment IdP can log in. Also ECHO_AGENT_DISABLE_API_KEY_AUTH; also valid as `echo_agent_com_config.disable_api_key_auth`. |
| `auth.force_login_team_uuid` | `string / string[]` | `pin` | `user` | Require login to this team UUID, or any of an array; empty array fails closed. Also ECHO_AGENT_FORCE_LOGIN_TEAM_ID; also valid as `echo_agent_com_config.force_login_team_uuid`. |
| `auth.echo_agent_ws_origin` | `string` | `yes` | `user` | Websocket origin for a configured EchoAgent relay. Also ECHO_AGENT_WS_ORIGIN; also valid as `echo_agent_com_config.echo_agent_ws_origin`. |
| `auth.echo_agent_ws_url` | `string` | `yes` | `user` | Relay websocket URL. Also ECHO_AGENT_WS_URL; also valid as `echo_agent_com_config.echo_agent_ws_url`. |
| `auth.oauth2` | `table` | `yes` | `user` | OAuth2 provider used when enterprise OIDC is unset; also valid as `echo_agent_com_config.oauth2`. |
| `auth.oauth2.client_id` | `string` | `yes` | `user` | OAuth2 client id. Also ECHO_AGENT_OAUTH2_CLIENT_ID; also valid as `echo_agent_com_config.oauth2.client_id`. |
| `auth.oauth2.issuer` | `string` | `yes` | `user` | OAuth2 issuer URL. Also ECHO_AGENT_OAUTH2_ISSUER; also valid as `echo_agent_com_config.oauth2.issuer`. |
| `auth.oauth2.principal_id` | `string` | `yes` | `user` | Required principal id when `principal_type` is set. Also ECHO_AGENT_OAUTH2_PRINCIPAL_ID; also valid as `echo_agent_com_config.oauth2.principal_id`. |
| `auth.oauth2.principal_type` | `string` | `yes` | `user` | Token principal type, such as Team. Also ECHO_AGENT_OAUTH2_PRINCIPAL_TYPE; also valid as `echo_agent_com_config.oauth2.principal_type`. |
| `auth.oauth2.referrer` | `string` | `yes` | `user` | Referrer for OAuth usage attribution. Also ECHO_AGENT_OAUTH2_REFERRER; also valid as `echo_agent_com_config.oauth2.referrer`. |
| `auth.oauth2.scopes` | `string[]` | `yes` | `user` | OAuth2 scopes. Also ECHO_AGENT_OAUTH2_SCOPES; also valid as `echo_agent_com_config.oauth2.scopes`. |
| `auth.oidc` | `table` | `yes` | `user` | Customer OIDC identity-provider settings; also valid as `echo_agent_com_config.oidc`. |
| `auth.oidc.audience` | `string` | `yes` | `user` | Optional OIDC audience. Also ECHO_AGENT_OIDC_AUDIENCE; also valid as `echo_agent_com_config.oidc.audience`. |
| `auth.oidc.client_id` | `string` | `yes` | `user` | OIDC client id. Also ECHO_AGENT_OIDC_CLIENT_ID; also valid as `echo_agent_com_config.oidc.client_id`. |
| `auth.oidc.issuer` | `string` | `yes` | `user` | OIDC issuer URL. Also ECHO_AGENT_OIDC_ISSUER; also valid as `echo_agent_com_config.oidc.issuer`. |
| `auth.oidc.scopes` | `string[]` | `yes` | `user` | OIDC scopes. Also ECHO_AGENT_OIDC_SCOPES; also valid as `echo_agent_com_config.oidc.scopes`. |
| `auth.preferred_method` | `api_key / oidc` | `yes` | `user` | Pin automatic auth to one method with no fallthrough; also valid as `echo_agent_com_config.preferred_method`. |
| `auth.token_header` | `string` | `yes` | `user` | Header name that carries the CLI auth token; default `echo-agent-cli`; also valid as `echo_agent_com_config.token_header`. |

### `auth_provider`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `auth_provider.<name>` | `table` | `yes` | `user` | Named credential helper used by `[model.<id>] auth_provider`. |

### `auto_mode`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `auto_mode.enabled` | `boolean` | `yes` | `user` | Enable Auto permission mode. |

### `campaigns`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `campaigns` | `array of tables` | `yes` | `user` | Named campaign patches applied below requirements. The deployment publishes these. |

### `cli`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `cli.auto_update` | `boolean` | `pin` | `user` | Check a configured standalone runtime release channel on launch. Disabled by default in the embedded desktop runtime. Also ECHO_AGENT_DISABLE_AUTOUPDATER to suppress. |
| `cli.channel` | `stable / alpha` | `pin` | `user` | Release channel preference. |
| `cli.installer` | `string` | `—` | `user` | Which installer last set up this CLI, used to pick the update path. |
| `cli.maximum_version` | `string` | `pin` | `user` | Highest CLI version that still runs without a hard block. Also ECHO_AGENT_MAXIMUM_VERSION. |
| `cli.minimum_version` | `string` | `pin` | `user` | Lowest CLI version that still runs without a hard block. Also ECHO_AGENT_MINIMUM_VERSION. |
| `cli.npm_registry` | `string` | `yes` | `user` | npm registry used by the auto-updater. |
| `cli.required_maximum_version` | `string` | `pin` | `user` | Hard maximum CLI version. Also ECHO_AGENT_REQUIRED_MAXIMUM_VERSION. |
| `cli.required_minimum_version` | `string` | `pin` | `user` | Hard minimum CLI version. Also ECHO_AGENT_REQUIRED_MINIMUM_VERSION. |
| `cli.session_picker_grouped` | `boolean` | `yes` | `user` | Group sessions by repo in the picker and CLI listings. |
| `cli.session_registry` | `boolean` | `yes` | `user` | Participate in the cross-process session registry. |
| `cli.show_tips` | `boolean` | `pin` | `user` | Startup tips. |
| `cli.use_leader` | `boolean` | `pin` | `user` | Use the leader process for config reload and MCP watches. |
| `cli.worktree_type` | `string` | `yes` | `user` | Worktree implementation preference. |

### `compat`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `compat.claude.agents` | `boolean` | `yes` | `user` | Scan CLAUDE.md. Also ECHO_AGENT_CLAUDE_AGENTS_ENABLED. |
| `compat.claude.hooks` | `boolean` | `yes` | `user` | Scan Claude hooks. Also ECHO_AGENT_CLAUDE_HOOKS_ENABLED. |
| `compat.claude.mcps` | `boolean` | `yes` | `user` | Scan Claude MCP config. Also ECHO_AGENT_CLAUDE_MCPS_ENABLED. |
| `compat.claude.rules` | `boolean` | `yes` | `user` | Scan Claude rules. Also ECHO_AGENT_CLAUDE_RULES_ENABLED. |
| `compat.claude.skills` | `boolean` | `yes` | `user` | Scan Claude skills. Also ECHO_AGENT_CLAUDE_SKILLS_ENABLED. |
| `compat.codex.hooks` | `boolean` | `yes` | `user` | Scan Codex hooks when present. |
| `compat.codex.skills` | `boolean` | `yes` | `user` | Scan Codex skills directories when present. |
| `compat.cursor.agents` | `boolean` | `yes` | `user` | Scan agent definitions from Cursor compat sources. Also ECHO_AGENT_CURSOR_AGENTS_ENABLED. |
| `compat.cursor.hooks` | `boolean` | `yes` | `user` | Scan Cursor hooks. Also ECHO_AGENT_CURSOR_HOOKS_ENABLED. |
| `compat.cursor.mcps` | `boolean` | `yes` | `user` | Scan Cursor mcp.json. Also ECHO_AGENT_CURSOR_MCPS_ENABLED. |
| `compat.cursor.rules` | `boolean` | `yes` | `user` | Scan `.cursor/rules/`. Also ECHO_AGENT_CURSOR_RULES_ENABLED. |
| `compat.cursor.skills` | `boolean` | `yes` | `user` | Scan Cursor skills directories. Also ECHO_AGENT_CURSOR_SKILLS_ENABLED. |

### `dashboard`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `dashboard.enabled` | `boolean` | `yes` | `user` | Show the agent dashboard. |
| `dashboard.grouping` | `state / directory` | `yes` | `user` | How dashboard rows group. |

### `default_auto_mode`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `default_auto_mode` | `boolean` | `yes` | `user` | Start sessions in auto permission mode when no per-session override is set. |

### `diagnostics`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `diagnostics.crash_handler` | `boolean` | `yes` | `user` | Write a panic report under `$ECHO_AGENT_HOME/crash/`. Also ECHO_AGENT_CRASH_HANDLER. |

### `disable_web_search`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `disable_web_search` | `boolean` | `yes` | `user` | Drop the web_search tool for this process. Also `--disable-web-search`. |

### `disabled_mcp_servers`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `disabled_mcp_servers` | `string[]` | `yes` | `user` | MCP server names to skip without deleting their `[mcp_servers]` blocks. |

### `disabled_mcp_tools`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `disabled_mcp_tools` | `map<string, string[]>` | `yes` | `user` | Per-server MCP tool deny lists keyed by server name. |

### `doom_loop_recovery`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `doom_loop_recovery.enabled` | `boolean` | `yes` | `user` | Resample confident tool-call loops; set false to disable. |

### `endpoints`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `endpoints.cli_chat_proxy_base_url` | `string` | `pin` | `user` | Session-service API base URL. |
| `endpoints.deployment_key` | `string` | `pin` | `user` | Management key for enterprise deployments. Also ECHO_AGENT_DEPLOYMENT_KEY. |
| `endpoints.feedback_base_url` | `string` | `yes` | `user` | Where feedback submissions go. Also ECHO_AGENT_FEEDBACK_BASE_URL. |
| `endpoints.managed_config_url` | `string` | `yes` | `user` | Override managed config endpoint. Also ECHO_AGENT_MANAGED_CONFIG_URL. |
| `endpoints.models_base_url` | `string` | `pin` | `user` | Custom inference base URL. Also ECHO_AGENT_MODELS_BASE_URL. |
| `endpoints.models_list_url` | `string` | `pin` | `user` | Override model-list URL. Also ECHO_AGENT_MODELS_LIST_URL. Alias `models_endpoint`. |
| `endpoints.trace_upload_bucket` | `string` | `yes` | `user` | Direct gs:// or s3:// bucket for traces; bypasses the proxy. Also ECHO_AGENT_TRACE_UPLOAD_BUCKET. |
| `endpoints.trace_upload_credentials` | `string` | `yes` | `user` | Inline GCS service-account JSON or AWS credentials for that bucket; wins over `trace_upload_credentials_file` and has no environment variable. |
| `endpoints.trace_upload_credentials_file` | `string (path)` | `yes` | `user` | Path to a GCS service-account JSON or AWS credentials file for that bucket. Also ECHO_AGENT_TRACE_UPLOAD_CREDENTIALS_FILE. |
| `endpoints.trace_upload_endpoint_url` | `string` | `yes` | `user` | Custom S3-compatible endpoint for s3:// bucket uploads. Also ECHO_AGENT_TRACE_UPLOAD_ENDPOINT_URL. |
| `endpoints.trace_upload_region` | `string` | `yes` | `user` | AWS region for s3:// bucket uploads; default us-east-1. Also ECHO_AGENT_TRACE_UPLOAD_REGION. |
| `endpoints.trace_upload_url` | `string` | `pin` | `user` | Proxy destination for traces when no direct bucket is set. Also ECHO_AGENT_TRACE_UPLOAD_URL. |
| `endpoints.echo_agent_api_base_url` | `string` | `pin` | `user` | Public EchoAgent API base. Also ECHO_AGENT_API_BASE_URL. |

### `features`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `features.ask_user_question` | `boolean` | `pin` | `user` | Enable or disable `ask_user_question`. Default true. Also `ECHO_AGENT_ASK_USER_QUESTION`. |
| `features.auto_wake` | `boolean` | `pin` | `user` | Enable or disable `auto_wake`. Default true. Also `ECHO_AGENT_AUTO_WAKE`. |
| `features.backend_tools` | `boolean` | `pin` | `user` | Enable or disable `backend_tools`. Default true. Also `ECHO_AGENT_BACKEND_SEARCH`. |
| `features.campaigns` | `boolean` | `yes` | `user` | Enable remote campaign patches. `ECHO_AGENT_CAMPAIGNS=0` still disables even when requirements set this true. |
| `features.cancel_rewind` | `boolean` | `pin` | `user` | Enable or disable `cancel_rewind`. Default true. Also `ECHO_AGENT_CANCEL_REWIND`. |
| `features.codebase_indexing` | `boolean / string[]` | `pin` | `user` | Codebase graph indexing; true indexes git repos, or pass include/exclude globs. |
| `features.compaction_detail` | `none / minimal / balanced / verbose` | `yes` | `user` | Verbatim detail level for `segments` compaction. Also ECHO_AGENT_COMPACTION_DETAIL. |
| `features.compaction_mode` | `summary / transcript / segments` | `yes` | `user` | Compaction strategy. Also ECHO_AGENT_COMPACTION_MODE. |
| `features.compaction_tool_choice` | `string` | `yes` | `user` | Tool-choice hint used during compaction. |
| `features.compaction_verbatim_input` | `boolean` | `pin` | `user` | Enable or disable `compaction_verbatim_input`. Default true. Also `ECHO_AGENT_COMPACTION_VERBATIM_INPUT`. |
| `features.feedback` | `boolean` | `pin` | `user` | Enable or disable `feedback`. Default true. Also `ECHO_AGENT_FEEDBACK_ENABLED`. |
| `features.feedback_trace_card` | `boolean` | `pin` | `user` | Show a trace-upload consent question after `/feedback`. Default false. Also `ECHO_AGENT_FEEDBACK_TRACE_CARD`. |
| `features.image_edit_model_override` | `string` | `yes` | `user` | Imagine model id for image_edit. |
| `features.image_gen` | `boolean` | `pin` | `user` | Enable image_gen / `/imagine`. |
| `features.image_gen_model_override` | `string` | `yes` | `user` | Imagine model id for image_gen. Empty defers to the remotely configured default. |
| `features.lsp_tools` | `boolean` | `pin` | `user` | Enable or disable `lsp_tools`. Default false. Also `ECHO_AGENT_LSP_TOOLS`. |
| `features.managed_config` | `boolean` | `yes` | `user` | Fetch managed_config.toml and requirements.toml from the deployment. |
| `features.mcp_auto_restart` | `boolean` | `yes` | `user` | Auto-restart stdio MCP servers after transport failure. Also ECHO_AGENT_MCP_AUTO_RESTART. |
| `features.mcp_liveness_watchers` | `boolean` | `yes` | `user` | Poll MCP transports and push server_status updates. Emergency kill switch when false. |
| `features.mcp_push_server_status` | `boolean` | `yes` | `user` | Pager subscribes to MCP server_status push. Process env ECHO_AGENT_MCP_PUSH_SERVER_STATUS wins at launch. |
| `features.mcp_recursive_config_watch` | `boolean` | `yes` | `user` | Watch `<cwd>/` and `<cwd>/.echo-agent/` for project MCP config edits. Name is a misnomer; watches are non-recursive. |
| `features.non_git_warning` | `boolean` | `yes` | `user` | Show a blocking warning when EchoAgent starts outside a Git repository. |
| `features.remember_mode` | `boolean` | `—` | `—` | Remember the last permission mode across sessions. Read from user `config.toml` only. |
| `features.remote_fetch` | `boolean` | `pin` | `fleet` | Pin remote model-catalog and asset fetch. Managed wins over the user file when both set. |
| `features.session_recap` | `boolean` | `pin` | `user` | Enable or disable `session_recap`. Default true. Also `ECHO_AGENT_SESSION_RECAP`. |
| `features.session_search` | `boolean` | `pin` | `user` | Enable or disable `session_search`. Default true. Also `ECHO_AGENT_SESSION_SEARCH`. |
| `features.subagent_worktree_snapshot` | `boolean` | `pin` | `user` | Enable or disable `subagent_worktree_snapshot`. Default false. Also `ECHO_AGENT_SUBAGENT_WORKTREE_SNAPSHOT`. |
| `features.support_permission` | `boolean` | `yes` | `user` | Allow the agent to ask permission for tool executions. |
| `features.telemetry` | `boolean / session_metrics / off` | `pin` | `user` | Product telemetry mode. Enterprise default is off. |
| `features.title_refresh` | `boolean` | `pin` | `user` | Early-session auto-title refresh. Pin this in requirements to beat ECHO_AGENT_TITLE_REFRESH. |
| `features.turn_summary` | `boolean` | `pin` | `user` | Enable or disable `turn_summary`. Default true. Also `ECHO_AGENT_TURN_SUMMARY`. |
| `features.two_pass_compaction` | `boolean` | `pin` | `user` | Enable or disable `two_pass_compaction`. Default false. Also `ECHO_AGENT_TWO_PASS_COMPACTION`. |
| `features.video_gen` | `boolean` | `pin` | `user` | Enable video tools / `/imagine-video`. |
| `features.voice_mode` | `boolean` | `pin` | `user` | Enable or disable `voice_mode`. Default true. Also `ECHO_AGENT_VOICE_MODE`. |
| `features.web_fetch` | `boolean` | `pin` | `user` | Enable or disable `web_fetch`. Default false. Also `ECHO_AGENT_WEB_FETCH`. |
| `features.write_file` | `boolean` | `pin` | `user` | Enable or disable `write_file`. Default true. Also `ECHO_AGENT_WRITE_FILE`. |
| `features.zdr_access_enabled` | `boolean` | `pin` | `user` | Advertise ZDR-incompatible tools when the team is on Zero Data Retention. Also `ECHO_AGENT_ZDR_ACCESS_ENABLED`. |

### `feedback`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `feedback.user.command` | `string` | `yes` | `user` | Shell command that prints name and email JSON for feedback submissions. |
| `feedback.user.email` | `string[]` | `yes` | `user` | Sources for the feedback author email (`git_email` or a literal). |
| `feedback.user.name` | `string[]` | `yes` | `user` | Sources for the feedback author name (`os_user` or a literal). |

### `goal`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `goal.enabled` | `boolean` | `yes` | `user` | Enable `/goal`. |

### `echo_agent_com_config`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `echo_agent_com_config` | `table` | `yes` | `user` | EchoAgent relay and OAuth/OIDC settings. `[auth]` is an alias. |
| `echo_agent_com_config.auth_provider_command` | `string` | `yes` | `user` | External auth binary; stdout is the token. Also ECHO_AGENT_AUTH_PROVIDER_COMMAND. |
| `echo_agent_com_config.auth_provider_label` | `string` | `yes` | `user` | Login button label for an external auth provider. Also ECHO_AGENT_AUTH_PROVIDER_LABEL. |
| `echo_agent_com_config.auth_token_ttl` | `number` | `yes` | `user` | Token TTL in seconds for providers that return a bare token. Also ECHO_AGENT_AUTH_TOKEN_TTL. |
| `echo_agent_com_config.disable_api_key_auth` | `boolean` | `pin` | `user` | Refuse API-key auth so only the deployment IdP can log in. Also ECHO_AGENT_DISABLE_API_KEY_AUTH. |
| `echo_agent_com_config.force_login_team_uuid` | `string / string[]` | `pin` | `user` | Require login to this team UUID, or any of an array; empty array fails closed. Also ECHO_AGENT_FORCE_LOGIN_TEAM_ID. |
| `echo_agent_com_config.echo_agent_ws_origin` | `string` | `yes` | `user` | Websocket origin for a configured EchoAgent relay. Also ECHO_AGENT_WS_ORIGIN. |
| `echo_agent_com_config.echo_agent_ws_url` | `string` | `yes` | `user` | Relay websocket URL. Also ECHO_AGENT_WS_URL. |
| `echo_agent_com_config.oauth2` | `table` | `yes` | `user` | OAuth2 provider used when enterprise OIDC is unset. |
| `echo_agent_com_config.oauth2.client_id` | `string` | `yes` | `user` | OAuth2 client id. Also ECHO_AGENT_OAUTH2_CLIENT_ID. |
| `echo_agent_com_config.oauth2.issuer` | `string` | `yes` | `user` | OAuth2 issuer URL. Also ECHO_AGENT_OAUTH2_ISSUER. |
| `echo_agent_com_config.oauth2.principal_id` | `string` | `yes` | `user` | Required principal id when `principal_type` is set. Also ECHO_AGENT_OAUTH2_PRINCIPAL_ID. |
| `echo_agent_com_config.oauth2.principal_type` | `string` | `yes` | `user` | Token principal type, such as Team. Also ECHO_AGENT_OAUTH2_PRINCIPAL_TYPE. |
| `echo_agent_com_config.oauth2.referrer` | `string` | `yes` | `user` | Referrer for OAuth usage attribution. Also ECHO_AGENT_OAUTH2_REFERRER. |
| `echo_agent_com_config.oauth2.scopes` | `string[]` | `yes` | `user` | OAuth2 scopes. Also ECHO_AGENT_OAUTH2_SCOPES. |
| `echo_agent_com_config.oidc` | `table` | `yes` | `user` | Customer OIDC identity-provider settings. |
| `echo_agent_com_config.oidc.audience` | `string` | `yes` | `user` | Optional OIDC audience. Also ECHO_AGENT_OIDC_AUDIENCE. |
| `echo_agent_com_config.oidc.client_id` | `string` | `yes` | `user` | OIDC client id. Also ECHO_AGENT_OIDC_CLIENT_ID. |
| `echo_agent_com_config.oidc.issuer` | `string` | `yes` | `user` | OIDC issuer URL. Also ECHO_AGENT_OIDC_ISSUER. |
| `echo_agent_com_config.oidc.scopes` | `string[]` | `yes` | `user` | OIDC scopes. Also ECHO_AGENT_OIDC_SCOPES. |
| `echo_agent_com_config.preferred_method` | `api_key / oidc` | `yes` | `user` | Pin automatic auth to one method with no fallthrough. |
| `echo_agent_com_config.token_header` | `string` | `yes` | `user` | Header name that carries the CLI auth token; default `echo-agent-cli`. |

### `harness`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `harness.block_for_upload` | `boolean` | `yes` | `user` | Block turn end until the workspace snapshot upload finishes. |
| `harness.disable_workspace_teleport` | `boolean` | `pin` | `user` | Kill switch for per-turn workspace snapshots. |

### `hints`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `hints.fork_worktree_mode` | `ask / always / never` | `yes` | `user` | Whether `/fork` offers a worktree. |
| `hints.new_session_worktree_mode` | `ask / always / never` | `yes` | `user` | Whether `/new` offers a worktree. |

### `hooks`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `hooks.<event>` | `array of tables` | `yes` | `user` | Matcher groups for a lifecycle event such as PreToolUse or Stop. See Hooks. |
| `hooks.<event>[].hooks[].command` | `string` | `yes` | `user` | Command to run for this hook. `$VAR` is not expanded at load. |
| `hooks.<event>[].hooks[].type` | `command` | `yes` | `user` | Hook handler type. Command hooks are supported. |
| `hooks.<event>[].matcher` | `string` | `yes` | `user` | Tool-name matcher for this hook group. |

### `managed_mcps`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `managed_mcps.enabled` | `boolean` | `pin` | `user` | Fetch managed MCP configs at startup. Also ECHO_AGENT_MANAGED_MCPS_ENABLED. |
| `managed_mcps.gateway_tools_enabled` | `boolean` | `yes` | `user` | Expose managed MCP gateway tools. Also ECHO_AGENT_MANAGED_MCP_GATEWAY_TOOLS_ENABLED. |

### `marketplace`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `marketplace.sources` | `array of tables` | `yes` | `user` | `[[marketplace.sources]]` plugin marketplace repos. |

### `mcp`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `mcp.max_output_bytes` | `number` | `yes` | `user` | Cap MCP tool output size in bytes. Project files may set this. |

### `mcp_servers`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `mcp_servers.<name>.args` | `string[]` | `yes` | `user` | `[mcp_servers.<name>]` `args` on a stdio or HTTP MCP server. |
| `mcp_servers.<name>.bearer_token_env_var` | `string` | `yes` | `user` | `[mcp_servers.<name>]` `bearer_token_env_var` on a stdio or HTTP MCP server. |
| `mcp_servers.<name>.command` | `string` | `yes` | `user` | `[mcp_servers.<name>]` `command` on a stdio or HTTP MCP server. |
| `mcp_servers.<name>.cwd` | `string` | `yes` | `user` | `[mcp_servers.<name>]` `cwd` on a stdio or HTTP MCP server. |
| `mcp_servers.<name>.enabled` | `boolean` | `yes` | `user` | `[mcp_servers.<name>]` `enabled` on a stdio or HTTP MCP server. |
| `mcp_servers.<name>.env` | `table` | `yes` | `user` | `[mcp_servers.<name>]` `env` on a stdio or HTTP MCP server. |
| `mcp_servers.<name>.expose_image_base64` | `boolean` | `yes` | `user` | `[mcp_servers.<name>]` `expose_image_base64` on a stdio or HTTP MCP server. |
| `mcp_servers.<name>.headers` | `table` | `yes` | `user` | `[mcp_servers.<name>]` `headers` on a stdio or HTTP MCP server. |
| `mcp_servers.<name>.oauth` | `table` | `yes` | `user` | `[mcp_servers.<name>]` `oauth` on a stdio or HTTP MCP server. |
| `mcp_servers.<name>.oauth_client_id` | `string` | `yes` | `user` | `[mcp_servers.<name>]` `oauth_client_id` on a stdio or HTTP MCP server. |
| `mcp_servers.<name>.oauth_client_secret_env_var` | `string` | `yes` | `user` | `[mcp_servers.<name>]` `oauth_client_secret_env_var` on a stdio or HTTP MCP server. |
| `mcp_servers.<name>.oauth_scopes` | `string[]` | `yes` | `user` | `[mcp_servers.<name>]` `oauth_scopes` on a stdio or HTTP MCP server. |
| `mcp_servers.<name>.setup` | `table` | `yes` | `user` | `[mcp_servers.<name>]` `setup` on a stdio or HTTP MCP server. |
| `mcp_servers.<name>.startup_timeout_sec` | `number` | `yes` | `user` | `[mcp_servers.<name>]` `startup_timeout_sec` on a stdio or HTTP MCP server. |
| `mcp_servers.<name>.tool_timeout_sec` | `number` | `yes` | `user` | `[mcp_servers.<name>]` `tool_timeout_sec` on a stdio or HTTP MCP server. |
| `mcp_servers.<name>.tool_timeouts` | `table` | `yes` | `user` | `[mcp_servers.<name>]` `tool_timeouts` on a stdio or HTTP MCP server. |
| `mcp_servers.<name>.type` | `string` | `yes` | `user` | `[mcp_servers.<name>]` `type` on a stdio or HTTP MCP server. |
| `mcp_servers.<name>.url` | `string` | `yes` | `user` | `[mcp_servers.<name>]` `url` on a stdio or HTTP MCP server. |

### `memory`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `memory.enabled` | `boolean` | `pin` | `user` | Cross-session memory master switch. Also ECHO_AGENT_MEMORY. |

### `model`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `model.<id>` | `table` | `yes` | `user` | Per-model override or BYOK definition. Prefer `env_key` over inline `api_key`. |
| `model.<id>.agent_type` | `string` | `yes` | `user` | Agent definition type associated with this model. |
| `model.<id>.api_backend` | `chat_completions / responses / messages` | `yes` | `user` | Wire protocol for this model. |
| `model.<id>.api_base_url` | `string` | `yes` | `user` | Alternate API base used with ECHO_AGENT_API_KEY resolution. |
| `model.<id>.api_key` | `string` | `yes` | `user` | Inline API key. Prefer `env_key`. Not a secret to put in a shared repo. |
| `model.<id>.auth_provider` | `string` | `yes` | `user` | Name of a `[auth_provider.<name>]` helper that mints this model's bearer token. |
| `model.<id>.auto_compact_threshold_percent` | `integer` | `yes` | `user` | Per-model auto-compact threshold (0-100). |
| `model.<id>.base_url` | `string` | `yes` | `user` | Provider endpoint base URL. |
| `model.<id>.compaction_at_tokens` | `number / table` | `yes` | `user` | Token threshold that triggers compaction for this model. |
| `model.<id>.compactions_remaining` | `string / table` | `yes` | `user` | How compaction leftover context is sent. Alias `send_compactions_remaining`. |
| `model.<id>.context_window` | `number` | `yes` | `user` | Context window tokens; drives auto-compact timing. |
| `model.<id>.description` | `string` | `yes` | `user` | Optional description shown in the picker. |
| `model.<id>.env_http_headers` | `map<string,string>` | `yes` | `user` | HTTP headers populated from environment variables when set. |
| `model.<id>.env_key` | `string / string[]` | `yes` | `user` | Environment variable name(s) holding the provider API key. |
| `model.<id>.extra_headers` | `map<string,string>` | `yes` | `user` | Per-request headers for this model. |
| `model.<id>.hidden` | `boolean` | `yes` | `user` | Hide this model from the picker. Still usable via `-m`. |
| `model.<id>.inference_idle_timeout_secs` | `number` | `yes` | `user` | Idle timeout for streaming inference on this model. |
| `model.<id>.max_completion_tokens` | `number` | `yes` | `user` | Per-model max completion tokens. |
| `model.<id>.max_retries` | `number` | `yes` | `user` | Inference retries for this model. |
| `model.<id>.model` | `string` | `yes` | `user` | Model id sent to the API. |
| `model.<id>.model_family` | `string` | `yes` | `user` | Family id used for compaction and capability grouping. |
| `model.<id>.model_provider` | `string` | `yes` | `user` | Named `[model_providers.<name>]` provider id for this model. |
| `model.<id>.name` | `string` | `yes` | `user` | Label shown in the model picker. |
| `model.<id>.query_params` | `map<string,string>` | `yes` | `user` | Extra query parameters on this model's requests. |
| `model.<id>.reasoning_effort` | `string` | `yes` | `user` | Deprecated per-model effort; prefer `reasoning_efforts`. |
| `model.<id>.reasoning_efforts` | `array of tables` | `yes` | `user` | Allowed reasoning-effort values for this model. |
| `model.<id>.show_model_fingerprint` | `boolean` | `yes` | `user` | Show the provider model fingerprint in the UI when present. |
| `model.<id>.stream_tool_calls` | `boolean` | `yes` | `user` | Per-model tool-call streaming request shape. |
| `model.<id>.supported_in_api` | `boolean` | `yes` | `user` | Whether this catalog entry is offered as a public API model. |
| `model.<id>.supports_backend_search` | `boolean` | `yes` | `user` | Whether the endpoint supports EchoAgent-hosted server-side search tools. |
| `model.<id>.supports_reasoning_effort` | `boolean` | `yes` | `user` | Deprecated; prefer `reasoning_efforts`. |
| `model.<id>.system_prompt_label` | `string` | `yes` | `user` | Per-model system-prompt identity label. |
| `model.<id>.temperature` | `number` | `yes` | `user` | Per-model sampling temperature. |
| `model.<id>.top_p` | `number` | `yes` | `user` | Per-model top_p. |
| `model.<id>.use_concise` | `boolean` | `yes` | `user` | Use the concise tool-description pack for this model. |

### `model_providers`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `model_providers.<name>` | `table` | `yes` | `user` | Named custom model provider definition. |

### `models`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `models.agent_type` | `string` | `yes` | `user` | Fallback agent_type for models without a per-model override. |
| `models.allowed_models` | `string[]` | `pin` | `user` | Glob allowlist for the model picker, default, and `-m`. Empty means no restriction. |
| `models.default` | `string` | `pin` | `user` | Model used for new sessions. Also `ECHO_AGENT_DEFAULT_MODEL`, `--model`, `-m`. |
| `models.default_reasoning_effort` | `string` | `yes` | `user` | Default reasoning effort for the default model when the model supports it. |
| `models.disabled_models` | `string[]` | `yes` | `user` | Remove these model IDs from the catalog. Wins over `hidden_models`. |
| `models.extra_headers` | `map<string,string>` | `yes` | `user` | Request headers applied to every model; per-model keys win. |
| `models.hidden_models` | `string[]` | `yes` | `user` | Hide these model IDs from the picker; `-m` can still select them. |
| `models.image_description` | `string` | `yes` | `user` | Vision model used to transcribe user-supplied images. |
| `models.inference_idle_timeout_secs` | `number` | `yes` | `user` | Global idle timeout for streaming inference when a model leaves it unset. |
| `models.max_completion_tokens` | `number` | `yes` | `user` | Global max completion tokens default when a model leaves it unset. |
| `models.max_retries` | `number` | `yes` | `user` | Global inference retry default when a model leaves it unset. |
| `models.prompt_suggestion` | `string` | `yes` | `user` | Model pin for next-prompt ghost text. Unset falls through remote, then the client default. |
| `models.session_summary` | `string` | `yes` | `user` | Model used for session titles and summaries. |
| `models.stream_tool_calls` | `boolean` | `yes` | `user` | Global tool-call streaming request shape; some BYOK endpoints need false. |
| `models.temperature` | `number` | `yes` | `user` | Global sampling temperature default when a model leaves it unset. |
| `models.top_p` | `number` | `yes` | `user` | Global top_p default when a model leaves it unset. |
| `models.web_search` | `string` | `pin` | `user` | Model used by the client `web_search` tool. Also `ECHO_AGENT_WEB_SEARCH_MODEL`. |

### `path_not_found_hints`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `path_not_found_hints` | `boolean` | `yes` | `user` | Enrich path-not-found errors with CWD reminders and similar-name suggestions. |

### `paths`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `paths.extra_rule_dirs` | `string[]` | `yes` | `user` | More rule directories (each contains `*.md`). |
| `paths.extra_skill_dirs` | `string[]` | `yes` | `user` | More skill directories (each contains `<skill>/SKILL.md`). |

### `permission`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `permission.allow` | `string[]` | `yes` | `user` | Compact allow rules such as `Bash(git *)`. Deny beats ask beats allow. Project files may set this. |
| `permission.ask` | `string[]` | `yes` | `user` | Compact ask rules. Project files may set this. |
| `permission.deny` | `string[]` | `yes` | `user` | Compact deny rules. Project files may set this. |
| `permission.rules` | `array of tables` | `yes` | `user` | Verbose action/tool/pattern object rules. Project files may set this. |

### `plugins`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `plugins.disabled` | `string[]` | `yes` | `user` | Plugin IDs to discover but not load. Project files may set this. |
| `plugins.enabled` | `string[]` | `yes` | `user` | Plugin IDs to enable; needed for project plugins that default off. |
| `plugins.paths` | `string[]` | `yes` | `user` | Additional plugin directories. Project files may set this when the folder is trusted. |

### `privacy`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `privacy.privacy_banner_acked` | `string` | `—` | `—` | RFC 3339 UTC timestamp when the local privacy banner was dismissed. The pager reads user `config.toml` only. |

### `relay`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `relay.enabled` | `boolean` | `yes` | `user` | Enable session relay sync. |

### `sandbox`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `sandbox.auto_allow_bash` | `boolean` | `pin` | `user` | Skip bash permission prompts when a sandbox profile is active. Also ECHO_AGENT_SANDBOX_AUTO_ALLOW_BASH. |
| `sandbox.profile` | `off / workspace / read-only / strict / string` | `pin` | `user` | Filesystem sandbox profile. Also `--sandbox` and ECHO_AGENT_SANDBOX. |

### `session`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `session.auto_compact_threshold_percent` | `integer` | `yes` | `user` | Auto-compact when context usage reaches this percent (0–100). |
| `session.load_envrc` | `boolean` | `yes` | `user` | Inject `.envrc` variables into bash. |

### `shell_environment_policy`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `shell_environment_policy.exclude` | `string[]` | `yes` | `user` | Env names to drop from bash. Overlay-allowlisted. |
| `shell_environment_policy.ignore_default_excludes` | `boolean` | `yes` | `user` | Skip the built-in env denylist. Overlay-allowlisted. |
| `shell_environment_policy.include_only` | `string[]` | `yes` | `user` | If set, bash inherits only these env names. Overlay-allowlisted. |
| `shell_environment_policy.inherit` | `string` | `yes` | `user` | Which parent env names bash inherits. Overlay-allowlisted; cannot inject values. |
| `shell_environment_policy.set` | `map<string,string>` | `yes` | `user` | Inject env values into bash. Not overlay-allowlisted. |

### `skills`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `skills.disabled` | `string[]` | `yes` | `user` | Skill names to discover but not activate. |
| `skills.paths` | `string[]` | `yes` | `user` | Additional skill directories. |

### `storage`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `storage` | `table` | `yes` | `user` | Local session storage cleanup policy. |

### `subagents`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `subagents.enabled` | `boolean` | `pin` | `user` | Subagent / task tool master switch. Also ECHO_AGENT_SUBAGENTS. |
| `subagents.limit_behavior` | `queue / fail` | `yes` | `user` | What to do when the concurrent subagent cap is hit. |
| `subagents.max_concurrent` | `integer` | `yes` | `user` | Max concurrent subagents. |
| `subagents.max_depth` | `integer` | `yes` | `user` | Max nested subagent depth (clamped ≥1). |
| `subagents.models.<name>` | `string` | `yes` | `user` | Per-subagent model id override. |
| `subagents.toggle.<name>` | `boolean` | `yes` | `user` | Enable or disable an individual subagent type. Omitted agents default on. |

### `telemetry`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `telemetry.otel_enabled` | `boolean` | `yes` | `user` | External OTEL master switch. Also ECHO_AGENT_EXTERNAL_OTEL. |
| `telemetry.trace_upload` | `boolean` | `pin` | `user` | Upload session traces. Requirements pin beats user config. |

### `tools`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `tools.disable_zdr_incompatible_tools` | `boolean` | `yes` | `user` | Restrict tools that need EchoAgent-hosted output under ZDR. Also ECHO_AGENT_DISABLE_ZDR_INCOMPATIBLE_TOOLS. |
| `tools.media_gen.max_parallel_image_gen_calls` | `integer` | `yes` | `user` | Cap parallel image_gen/image_edit calls in one model step. Also ECHO_AGENT_MAX_PARALLEL_IMAGE_GEN_CALLS. |
| `tools.media_gen.max_parallel_video_gen_calls` | `integer` | `yes` | `user` | Cap parallel video_gen calls in one model step. Also ECHO_AGENT_MAX_PARALLEL_VIDEO_GEN_CALLS. |
| `tools.respect_gitignore` | `boolean` | `pin` | `user` | When true, search and read tools skip gitignored files. Also ECHO_AGENT_RESPECT_GITIGNORE. |
| `tools.zdr_video_output_s3` | `table` | `yes` | `user` | Team S3 bucket for ZDR video output. See ZDR Video Storage. |

### `toolset`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `toolset.ask_user_question.timeout_secs` | `number` | `yes` | `user` | Timeout for the ask_user_question tool. |
| `toolset.bash.auto_background_on_timeout` | `boolean` | `yes` | `user` | Background the command when the foreground timeout fires. |
| `toolset.bash.login_shell_capture` | `boolean` | `yes` | `user` | Capture the user's login shell environment for bash. Overlay-allowlisted. |
| `toolset.bash.max_timeout_secs` | `number` | `yes` | `user` | Cap on model-requested foreground timeouts. |
| `toolset.bash.output_byte_limit` | `number` | `yes` | `user` | Max captured bash output in bytes. |
| `toolset.bash.timeout_secs` | `number` | `yes` | `user` | Foreground bash command timeout in seconds. |
| `toolset.file_toolset` | `standard / hashline` | `yes` | `user` | File edit tool scheme. |
| `toolset.web_fetch.allowed_domains` | `string[]` | `yes` | `user` | Domain allowlist override for web_fetch. |
| `toolset.web_fetch.proxy_endpoint` | `string` | `yes` | `user` | Egress proxy URL for web_fetch. Also ECHO_AGENT_WEB_FETCH_PROXY. |
| `toolset.web_search.allowed_domains` | `string[]` | `yes` | `user` | Domain allowlist for client web_search. Overlay-allowlisted. |
| `toolset.web_search.excluded_domains` | `string[]` | `yes` | `user` | Domain denylist for client web_search. Overlay-allowlisted. |

### `ui`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `ui.approval_mode` | `string` | `yes` | `user` | Deprecated; use `ui.permission_mode`. |
| `ui.auto_dark_theme` | `string` | `yes` | `user` | Theme when `theme = auto` and the OS is dark. |
| `ui.auto_light_theme` | `string` | `yes` | `user` | Theme when `theme = auto` and the OS is light. |
| `ui.cancel_subagents_on_turn_cancel` | `ask / always_stop / always_continue` | `yes` | `user` | What to do with running subagents when cancelling a parent turn. |
| `ui.collapsed_edit_blocks` | `boolean` | `yes` | `user` | Show edits as one-line +N/-M summaries. Also ECHO_AGENT_COLLAPSED_EDIT_BLOCKS. |
| `ui.combine_queued_prompts` | `boolean` | `yes` | `user` | Merge consecutive plain follow-ups into one turn. |
| `ui.compact_mode` | `boolean` | `yes` | `user` | Denser message padding. Also `/compact-mode`. |
| `ui.confirm_before_rewind` | `boolean` | `yes` | `user` | Ask before rewinding conversation history. |
| `ui.contextual_hints.image_input` | `boolean` | `yes` | `user` | Clipboard image paste tip when the model accepts images. |
| `ui.contextual_hints.plan_mode` | `boolean` | `yes` | `user` | Suggest plan mode (Shift+Tab) for planning-style prompts. |
| `ui.contextual_hints.send_now` | `boolean` | `yes` | `user` | After queuing a mid-turn follow-up, Enter on an empty prompt sends now. |
| `ui.contextual_hints.small_screen` | `boolean` | `yes` | `user` | Suggest `/compact-mode` on short terminals. |
| `ui.contextual_hints.ssh_wrap` | `boolean` | `yes` | `user` | Recommend `echoagent wrap` when SSH lacks a clipboard sink. |
| `ui.contextual_hints.undo` | `boolean` | `yes` | `user` | Ctrl+Z restores a wiped prompt draft tip. |
| `ui.contextual_hints.word_select` | `boolean` | `yes` | `user` | After double-click with fold/nav selection, point at Word select in settings. |
| `ui.cursor_blink` | `boolean` | `yes` | `user` | Force blinking (true) or steady (false) block cursor. Unset inherits the terminal. |
| `ui.default_selected_permission` | `string` | `yes` | `user` | Preselected approval row on the first prompt of a session. Also ECHO_AGENT_DEFAULT_SELECTED_PERMISSION. |
| `ui.display_refresh.auto_cadence_enabled` | `boolean` | `yes` | `user` | Match stream/scroll cadence to display refresh rate. Also ECHO_AGENT_DISPLAY_REFRESH_AUTO_CADENCE. |
| `ui.follow_up_behavior` | `queue / steer` | `yes` | `user` | Mid-turn follow-up routing. |
| `ui.fork_secondary_model` | `string` | `yes` | `user` | Model for the secondary agent when forking. Defaults to the main default model. |
| `ui.group_tool_verbs` | `boolean` | `yes` | `user` | Fold consecutive read/search/list tool rows. Also ECHO_AGENT_GROUP_TOOL_VERBS. |
| `ui.hunk_tracker_mode` | `agent_only / all_dirty / off` | `yes` | `user` | File-change hunk tracking. Also ECHO_AGENT_HUNK_TRACKER and `--hunk-tracker-mode`. |
| `ui.invert_scroll` | `boolean` | `yes` | `user` | Reverse vertical scroll direction. Also ECHO_AGENT_INVERT_SCROLL. |
| `ui.keep_text_selection` | `flash / hold / word_select` | `yes` | `user` | In-app selection: brief flash, hold, or double-click word select. |
| `ui.max_thoughts_width` | `number` | `yes` | `user` | Column width for the thoughts panel (40–500). |
| `ui.mouse_reporting_toggle` | `boolean` | `yes` | `user` | Ctrl+R in scrollback toggles terminal mouse capture. Also ECHO_AGENT_MOUSE_REPORTING_TOGGLE. |
| `ui.page_flip_on_send` | `boolean` | `yes` | `user` | Snap the sent prompt to the top of the viewport. |
| `ui.permission_mode` | `default / ask / auto / always-approve` | `yes` | `user` | Default tool-permission behavior. Enterprise locks use requirements.toml. |
| `ui.prompt_suggestions` | `boolean` | `yes` | `user` | Next-prompt ghost text after each turn. Also ECHO_AGENT_PROMPT_SUGGESTIONS. |
| `ui.remember_tool_approvals` | `boolean` | `yes` | `user` | Show per-tool Always allow options. Also ECHO_AGENT_REMEMBER_TOOL_APPROVALS. |
| `ui.render_mermaid` | `auto / on / off` | `yes` | `user` | How mermaid fences render: clickable open row or raw source. |
| `ui.screen_mode` | `fullscreen / minimal` | `yes` | `user` | Default render mode for plain `echoagent`. Restart required. |
| `ui.scroll_lines` | `integer` | `yes` | `user` | Lines per scroll tick (1–10). Also ECHO_AGENT_SCROLL_LINES. |
| `ui.scroll_mode` | `auto / wheel / trackpad` | `yes` | `user` | Scroll input classification. Also ECHO_AGENT_SCROLL_MODE. |
| `ui.scroll_speed` | `integer` | `yes` | `user` | Mouse/trackpad scroll speed multiplier (1–100). Also ECHO_AGENT_SCROLL_SPEED. |
| `ui.show_thinking_blocks` | `boolean` | `yes` | `user` | Show thinking/reasoning blocks while streaming. Also ECHO_AGENT_SHOW_THINKING_BLOCKS. |
| `ui.show_timeline` | `boolean` | `yes` | `user` | Per-turn tick rail instead of the scrollbar. |
| `ui.show_timestamps` | `boolean` | `yes` | `user` | Clock time next to messages. Also `/timestamps`. |
| `ui.simple_mode` | `boolean` | `yes` | `user` | Readline prompt editing when true; experimental vim prompt keys when false. |
| `ui.status_line.command` | `string` | `yes` | `user` | Script for a `command` status line. Campaigns strip this path; a requirements layer still merges it. |
| `ui.status_line.type` | `disabled / command` | `yes` | `user` | Optional status-line row above the shortcuts bar. Off by default. See the status-line user guide. |
| `ui.theme` | `string` | `yes` | `user` | Color theme name, or `auto`/`system` to follow the OS. Also `/theme` and ECHO_AGENT_THEME. |
| `ui.ui_theme` | `string` | `yes` | `user` | Legacy alias for `ui.theme`. |
| `ui.vim_mode` | `boolean` | `yes` | `user` | Vim keys in the scrollback, not the prompt. Also `/vim-mode`. |
| `ui.voice_capture_mode` | `hold / toggle` | `yes` | `user` | Hold-to-talk or press-to-toggle voice capture. |
| `ui.voice_keybind_enabled` | `boolean` | `yes` | `user` | Enable Ctrl+Space / F8 for voice dictation. `/voice` still works when false. |
| `ui.voice_stt_language` | `string` | `yes` | `user` | Speech-to-text language code or `auto`. Overrides `[voice].language` for the session. |
| `ui.yolo` | `boolean` | `pin` | `user` | Always-approve tool calls. Requirements can pin false and block `--yolo`. |

### `version_overrides`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `version_overrides` | `array of tables` | `yes` | `user` | Per-CLI-version config patches applied before merge. See `[[version_overrides]]`. |

### `voice`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `voice.api_base` | `string` | `yes` | `user` | HTTPS API root for speech-to-text. Unset inherits `[endpoints].echo_agent_api_base_url`. |
| `voice.language` | `string` | `yes` | `user` | Preferred STT language catalog code or `auto`. |
| `voice.sample_rate` | `number` | `yes` | `user` | STT capture rate in Hz. |

### `workflows`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `workflows.enabled` | `boolean` | `yes` | `user` | Enable workflows. |

### `worktree`

| Key | Type / Values | Requirements | Managed | Details |
| --- | --- | --- | --- | --- |
| `worktree.auto_gc` | `table` | `yes` | `user` | Automatic worktree garbage collection policy. |

## managed_config.toml

`managed_config.toml` accepts every key in the tables above. It sets fleet defaults, so a developer's own `config.toml` overrides it. Use it for values you want people to be able to adjust, and `requirements.toml` for values they cannot.

One exception to that rule:

| Key | Behaviour |
| --- | --- |
| `features.remote_fetch` | The managed value wins over the developer's. |

EchoAgent Build reads `/etc/echoagent/managed_config.toml` first, then `$ECHO_AGENT_HOME/managed_config.toml`, which the console keeps in sync. Values in the second replace values in the first.

The **Managed** column on the tables above is the per-key answer: `fleet` means the fleet value stands, `user` means the user's file wins, `—` means this file is ignored.

## requirements.toml

`requirements.toml` is an admin-enforced file. Locations: `$ECHO_AGENT_HOME/requirements.toml` (signed cache) then `/etc/echoagent/requirements.toml`, then macOS MDM `ai.x.echo-agent`. The **Requirements** column on the `config.toml` tables lists every `config.toml` key this file accepts (`pin` or `yes`). Omitted keys stay unconstrained.

These keys exist only in `requirements.toml`:

| Key | Type / Values | Default | Details |
| --- | --- | --- | --- |
| `fail_closed` | `boolean` | `false` | Refuse to start when signed requirements or version_overrides cannot be applied; default false. |
| `features.image_edit` | `boolean` | — | Pin image_edit availability. Requirements only; a user-file entry is unrecognized and unset leaves the remotely configured default. |
| `ui.disable_bypass_permissions_mode` | `boolean` | — | Lock always-approve off. The lock is enforced only from a requirements layer; true in user or managed files is ignored. |

## What happens when a setting is refused

| Situation | What EchoAgent Build does |
| --- | --- |
| A developer sets a key you pinned | The pinned value applies. `echoagent inspect` lists the requirements file that contributed. |
| A developer sets a key you shipped in `managed_config.toml` | Their value applies, except `features.remote_fetch`. Pin the key instead if it must hold. |
| `requirements.toml` is missing or its signature does not verify | The pins do not apply, and EchoAgent Build starts without them. Set `fail_closed = true` to refuse to start instead. |
| A pinned key names a value this version does not recognise | The key is ignored and the rest of the file still applies. |

## Check what is in effect

Run `echoagent inspect` on the developer's machine. It lists every config file that contributed, including requirements and managed layers, so a policy that is not applying is visible in one command.
