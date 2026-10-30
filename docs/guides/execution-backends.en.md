# Execution Backends

**Scope: standalone Python Runtime.** For Desktop operations, see the [Desktop guide](../desktop/index.md).

Command and code tools (`exec`, `execute_code`, `process`) do not run inside the agent process. They are handed to an **executor**, which determines the isolation strength and where the work runs. The fields on this page come from `ExecutionConfig` in `echo_agent/config/schema.py`, and the behaviour from `echo_agent/agent/executors/factory.py`.

## The four executors

`execution.default_executor` selects the backend when `tools.exec.host` is `auto`. Its schema default is `sandbox`, but the packaged defaults set it to `local`. Execution tools currently use the separate `tools.exec.host: sandbox` default:

| Value | Isolation | Runs on | Suited to |
|-------|-----------|---------|-----------|
| `local` | none beyond the process | this machine, inside the workspace | fully trusted local development |
| `sandbox` | a separate working directory, without OS isolation | this machine, under `sandbox_root` | trusted commands needing a temporary workspace copy |
| `container` | a container | the local container runtime | strong isolation or a pinned environment |
| `remote` | SSH | a remote host | when the compute or environment lives elsewhere |

The executor instance is reused for the lifetime of the `AgentLoop`, so sandbox and container setup costs are paid once rather than on every tool call.

```yaml
execution:
  default_executor: sandbox
  network_policy: deny
```

!!! note "There is no execution.backend"
    The field is `default_executor`, and its four values are a flat enum. There is no `execution.backend`, and no per-backend subsections such as `execution.shell`, `execution.container` or `execution.process`. Writing that structure raises no error but is silently ignored as an unknown key.

## Common options

`ExecutionConfig` has exactly these fields:

| Field | Default | Purpose |
|-------|---------|---------|
| `default_executor` | `local` (effective; schema: `sandbox`) | Backend when `tools.exec.host` is `auto` |
| `network_policy` | `allow` (effective; schema: `deny`) | Outbound policy: `allow` / `deny` / `restricted` |
| `sandbox_root` | `/tmp/echo-agent-sandbox` | Root directory for the `sandbox` executor |
| `container_image` | `''` | Image used by the `container` executor |
| `remote_host` | `''` | Target host for the `remote` executor |
| `remote_user` | `root` | SSH user |
| `remote_key_path` | `''` | SSH private key path |
| `remote_strict_host_key` | `accept-new` | Host key checking: `no` / `accept-new` / `yes` |
| `remote_connect_timeout` | `10` | SSH connect timeout in seconds |
| `max_background_tasks` | `64` | Concurrency ceiling for background tasks |

`network_policy` is passed to the executor. The packaged defaults set it to `allow`. Setting it to `deny` prevents outbound tools such as `web_fetch` and `web_search` from registering. Command checks are policy checks, not OS network isolation; see the [security profile matrix](../reference/security-profile-matrix.md).

## local

Executes directly in the workspace with no added isolation. Use it only when you fully trust the workload and need access to the local environment.

```yaml
execution:
  default_executor: local
```

## sandbox

Work runs in a directory under `sandbox_root` after copying workspace contents there. The process still runs on the host and can access host files permitted to its user through absolute paths. This is not a security boundary for untrusted code.

```yaml
execution:
  default_executor: sandbox
  sandbox_root: /tmp/echo-agent-sandbox
```

## container

Executes inside a container, giving the strongest isolation and a pinned environment. A container runtime must be installed and running, and `container_image` must be set explicitly — it defaults to empty.

```yaml
execution:
  default_executor: container
  container_image: python:3.12-slim
  network_policy: deny
```

Resource limits and volume mounts are decided on the container runtime side; there are no configuration fields for them here.

## remote

Executes on a remote host over SSH.

```yaml
execution:
  default_executor: remote
  remote_host: 10.0.0.20
  remote_user: echo
  remote_key_path: ~/.ssh/echo_agent_ed25519
  remote_strict_host_key: "yes"   # must be quoted, or YAML parses it as a bool
  remote_connect_timeout: 10
```

The default `accept-new` accepts the host key on first connection. In production prefer `"yes"` with the host key pre-seeded in `known_hosts`, so the first connection cannot be intercepted.

!!! warning "Quote yes and no"
    This field is a string enum (`no` / `accept-new` / `yes`). Unquoted `yes` and `no` are parsed by YAML as booleans, which fails configuration validation. Write `"yes"` or `"no"`.

## Overriding the executor per tool

`tools.exec.host` defaults to `sandbox`. The resulting executor is shared by `exec`, `execute_code` and `process`; set it to `auto` to use `execution.default_executor`:

```yaml
execution:
  default_executor: sandbox

tools:
  exec:
    host: container        # all three execution tools share this container executor
```

The remaining `tools.exec` fields constrain the command itself:

| Field | Default | Purpose |
|-------|---------|---------|
| `enabled` | `true` | Whether the `exec` tool is available |
| `security` | `allowlist` | Command admission policy |
| `allowed_commands` | `[]` | Explicitly permitted commands |
| `blocked_commands` | `[]` | Explicitly denied commands |
| `safe_bins` | see below | Executables treated as safe |
| `ask` | `on_miss` | When to request approval |
| `max_output_chars` | `2000000` | Output truncation threshold |
| `host` | `sandbox` | Executor used by this tool |
| `env_allowlist` | `[]` | Ambient variable names passed explicitly to `exec`, `execute_code` and `process` |

`safe_bins` defaults to read-only utilities such as `awk`, `cat`, `date`, `echo`, `find`, `grep`, `head`, `ls` and `pwd`.

Constraints for `execute_code` live under `tools.code_exec`, with three fields: `enabled`, `allowed_languages` and `timeout_seconds`.

Execution tools inherit only basic variables such as `PATH`, `HOME`, locale and TLS certificate paths, plus proxy URLs without credentials. To make a project variable available, allow its name in the configuration; its value is read from the service process environment rather than stored in the configuration:

```yaml
tools:
  exec:
    env_allowlist: [DATABASE_URL, PYTHONPATH]
```

The list applies to `exec`, `execute_code` and background `process` commands. Commands can read every listed variable, so include only values they need. Credentials explicitly injected for an execution request continue to use the existing authorization path.

## Comparison

| Aspect | local | sandbox | container | remote |
|--------|:-----:|:-------:|:---------:|:------:|
| Isolation | none | working-directory separation only | depends on container configuration | depends on the host |
| Extra prerequisites | none | none | container runtime | SSH reachability + key |
| Startup cost | lowest | low | medium | medium |
| Can reach the local workspace | yes | has a workspace copy; original may be reachable by absolute path | depends on mounts | depends on the remote host |

## Security guidance

- Set `network_policy: deny` explicitly when outbound tools must be withheld; command checks do not create a network namespace.
- Do not switch to `local` for convenience: it has no isolation, so model-generated commands act directly on the workspace.
- Keep `tools.exec.security` at `allowlist` and enumerate what you need in `allowed_commands`, rather than permitting everything and subtracting with `blocked_commands` — denylists are easy to work around.
- With `remote`, set `remote_strict_host_key` to `"yes"`.
- `exec`, `execute_code` and `process` all belong to `HIGH_RISK_TOOLS` and are denied by default under the `daemon` and `public_gateway` shapes. To use them there, add them to `tools.also_allow` explicitly and keep a human in the loop via `permissions.approval`.

## Related pages

- [Security profile matrix](../reference/security-profile-matrix.md) — tool admission and approval
- [Built-in tool reference](../reference/tools.md) — parameters for `exec`, `execute_code` and `process`
- [Configuration reference](../reference/configuration.md) — per-option reference generated from the schema
