# Execution Backends

Command and code tools (`exec`, `execute_code`, `process`) do not run inside the agent process. They are handed to an **executor**, which determines the isolation strength and where the work runs. The fields on this page come from `ExecutionConfig` in `echo_agent/config/schema.py`, and the behaviour from `echo_agent/agent/executors/factory.py`.

## The four executors

`execution.default_executor` selects the executor. It accepts four values and defaults to `sandbox`:

| Value | Isolation | Runs on | Suited to |
|-------|-----------|---------|-----------|
| `local` | none beyond the process | this machine, inside the workspace | fully trusted local development |
| `sandbox` | a separate sandbox directory (default) | this machine, under `sandbox_root` | the default choice, balancing usability and isolation |
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
| `default_executor` | `sandbox` | Executor type |
| `network_policy` | `deny` | Outbound policy: `allow` / `deny` / `restricted` |
| `sandbox_root` | `/tmp/echo-agent-sandbox` | Root directory for the `sandbox` executor |
| `container_image` | `''` | Image used by the `container` executor |
| `remote_host` | `''` | Target host for the `remote` executor |
| `remote_user` | `root` | SSH user |
| `remote_key_path` | `''` | SSH private key path |
| `remote_strict_host_key` | `accept-new` | Host key checking: `no` / `accept-new` / `yes` |
| `remote_connect_timeout` | `10` | SSH connect timeout in seconds |
| `max_background_tasks` | `64` | Concurrency ceiling for background tasks |

`network_policy` is passed to every executor. Because it defaults to `deny`, `web_fetch`, `web_search` and any tool carrying the `network.outbound` capability are withheld from the model — see the [security profile matrix](../reference/security-profile-matrix.md).

## local

Executes directly in the workspace with no added isolation. Use it only when you fully trust the workload and need access to the local environment.

```yaml
execution:
  default_executor: local
```

## sandbox

The default. Work runs in its own directory under `sandbox_root`, separate from the workspace.

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

`tools.exec` has its own `host` field, which overrides `default_executor` for the `exec` tool alone:

```yaml
execution:
  default_executor: sandbox

tools:
  exec:
    host: container        # only exec runs in a container
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

`safe_bins` defaults to read-only utilities such as `awk`, `cat`, `date`, `echo`, `find`, `grep`, `head`, `ls` and `pwd`.

Constraints for `execute_code` live under `tools.code_exec`, with three fields: `enabled`, `allowed_languages` and `timeout_seconds`.

## Comparison

| Aspect | local | sandbox | container | remote |
|--------|:-----:|:-------:|:---------:|:------:|
| Isolation | none | medium | strong | depends on the host |
| Extra prerequisites | none | none | container runtime | SSH reachability + key |
| Startup cost | lowest | low | medium | medium |
| Can reach the local workspace | yes | no | no | no |

## Security guidance

- Keep `network_policy: deny`; open it only when outbound access is genuinely needed, and prefer `restricted` first.
- Do not switch to `local` for convenience: it has no isolation, so model-generated commands act directly on the workspace.
- Keep `tools.exec.security` at `allowlist` and enumerate what you need in `allowed_commands`, rather than permitting everything and subtracting with `blocked_commands` — denylists are easy to work around.
- With `remote`, set `remote_strict_host_key` to `"yes"`.
- `exec`, `execute_code` and `process` all belong to `HIGH_RISK_TOOLS` and are denied by default under the `daemon` and `public_gateway` shapes. To use them there, add them to `tools.also_allow` explicitly and keep a human in the loop via `permissions.approval`.

## Related pages

- [Security profile matrix](../reference/security-profile-matrix.md) — tool admission and approval
- [Built-in tool reference](../reference/tools.md) — parameters for `exec`, `execute_code` and `process`
- [Configuration reference](../reference/configuration.md) — per-option reference generated from the schema
