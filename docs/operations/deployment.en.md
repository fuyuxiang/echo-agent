# Deployment

**Scope: standalone Python Runtime.** For Desktop operations, see the [Desktop guide](../desktop/index.md).

The standalone `echo-agent-runtime/` project includes complete Dashboard sources, bundled skills and `deploy/` templates. Pure sources require a frontend build; official Runtime archives and Python packages include prebuilt assets. See the [Runtime README](https://github.com/fuyuxiang/echo-agent/blob/master/echo-agent-runtime/README.en.md).

Echo Agent can run in the foreground or as a registered Gateway service. The default workspace is `~/.echo-agent`; use `echo-agent config dump` to inspect the effective paths.

## Local process

```bash
pip install "echo-agent[all]"
echo-agent setup
echo-agent run
```

`echo-agent run` starts the full agent and starts the Gateway only when `gateway.enabled: true`. Use `echo-agent gateway` to run the Gateway separately in the foreground. For a managed background process, run `echo-agent gateway install` followed by `echo-agent gateway start`. See [background service](background-service.en.md).

The default Gateway address is `127.0.0.1:58123`, with a health check at `http://127.0.0.1:58123/api/v1/health`. Use `--config` and `--workspace` on supported commands for a separate instance. See the [filesystem layout](../reference/filesystem-layout.en.md) for configuration search and workspace paths.

## Docker example

The repository has no prebuilt Docker image. Save this example as a `Dockerfile`. Inside a container, the Gateway must bind to `0.0.0.0` for port forwarding; a non-loopback bind requires at least one API token or startup is refused.

```dockerfile
FROM python:3.11-slim
RUN pip install "echo-agent[all]"
EXPOSE 58123
ENTRYPOINT ["echo-agent", "gateway", "--workspace", "/data/echo-agent", "--config", "/data/echo-agent/echo-agent.yaml"]
```

Save the following as `./echo-agent.yaml`. Replace the token with a random value and restrict file access:

```yaml
workspace: /data/echo-agent
gateway:
  host: "0.0.0.0"
  port: 58123
  auth:
    mode: allowlist
    api_tokens: ["replace-with-a-random-token"]
    allowed_users: ["cli:local"]  # Default CLI user; add other callers explicitly
    allowed_hosts: ["localhost", "127.0.0.1"]
```

```yaml
services:
  echo-agent:
    build: .
    ports:
      - "127.0.0.1:58123:58123"
    volumes:
      - echo-agent-data:/data/echo-agent
      - ./echo-agent.yaml:/data/echo-agent/echo-agent.yaml:ro
    restart: unless-stopped

volumes:
  echo-agent-data:
```

Start with `docker compose up -d` and inspect output with `docker compose logs -f echo-agent`. The published host port is loopback-only. For public access, add a host reverse proxy and list its domain in `gateway.auth.allowed_hosts`. Tool commands in this setup execute inside the container; mount project files explicitly when needed.

## Reverse proxy

For HTTPS or domain access, follow the [Gateway reverse proxy](../integrations/gateway/reverse-proxy.en.md) examples. Preserve the original Host and authentication headers and support WebSocket upgrades. The default health path is `/api/v1/health`.

## Multiple instances

Use a separate configuration, workspace, listen port, and service identity for each instance. Do not have multiple Agent processes write the same SQLite database or memory files. This is not a shared-database load-balancing configuration.

```bash
echo-agent gateway --config /srv/agent-a/echo-agent.yaml --workspace /srv/agent-a
echo-agent gateway --config /srv/agent-b/echo-agent.yaml --workspace /srv/agent-b
```

Set different `gateway.port` values in the two files, and plan credentials, backups, and process management separately.

## Related documentation

- [Gateway authentication](../integrations/gateway/authentication.en.md)
- [Backup and restore](backup-restore.en.md)
- [Security hardening](security-hardening.en.md)

## Headless server deployment and first tool task

An installed production wheel includes the Dashboard, fallback playground, bundled skills, and default config. Runtime needs Python 3.11+ and the selected provider extra; it needs no Node/pnpm, Rust/Tauri, source web directory, or root client directory. Source builders first build web with Node 24 LTS and the pinned pnpm, then run `python -m build`. Do not use a missing-Dashboard escape hatch for distributions.

Create a separate venv and workspace; install `'/absolute/path/echo_agent-0.3.8-py3-none-any.whl[openai]'`, run `pip check`, and create a mode-0600 config with absolute workspace/config paths. Configure an explicit model, `api_key_env: OPENAI_API_KEY`, loopback Gateway, separate random API/admin tokens, and allowlist `api:operator`. The [Chinese deployment walkthrough](deployment.md) contains the complete YAML and reproducible commands. A self-hosted keyless OpenAI-compatible endpoint must explicitly set its `api_base`, including its required API prefix.

Start `echo-agent gateway --config /absolute/config.yaml --workspace /absolute/workspace` in the foreground before installing the [background service](background-service.en.md). `/api/v1/health` measures service response; `/api/v1/ready` additionally checks local runtime, storage, configured provider, and required integrations. Readiness sends no remote model probe and does not verify credentials, quotas, or upstream availability. Submit a real tool task to prove that path.

Use `POST /api/v1/message` with a Bearer API token, JSON `platform: api`, `user_id/chat_id: operator`, `wait: true`, and a stable `Idempotency-Key`. Ask the model to actually call list_dir and inspect its tool record plus final status. Replaying the same key/request queries the original result; changing its content is a conflict. Unconfigured models produce an incomplete/error outcome instead of completed.

The authenticated `/playground` discovers custom HTTP/WS paths from `/meta`. Dashboard ordinary config saves report saved/running differences across refresh and require a restart; explicit channel lifecycle actions support their documented local activation. Use supported interactive WS or server CLI attach commands for tool approval and clarification. Tool approvals and board task review are different operations; neither requires a desktop application. Never auto-approve to make an unattended task look successful.

For persistent output use artifact_create/append/finalize/deliver and their IDs/validation results. HTTP/WS without attachment transport can receive numbered text chunks; synchronous HTTP returns tool deliveries in `reply.tool_deliveries` and the final answer in `reply.text`. Business completion, artifact persistence, send acceptance, and remote acknowledgement remain distinct facts.

Send SIGTERM and verify shutdown before switching to systemd. Give the service its own protected model environment: a service does not inherit an SSH shell's environment. Duplicate Runtime/port conflicts exit nonzero; service-control failures propagate instead of reporting success. Service and browser workflows were validated in an isolated macOS environment. Linux package and process checks run in CI. The Docker configuration is a deployment example without a corresponding complete container acceptance record. See the [review evidence](../reviews/server-review-and-remediation.md).

## Validate skill execution and web access

`deploy/echo-agent.yaml.example` uses `security.profile: daemon`, `tools.profile: coding`,
and explicit `also_allow: [web_fetch, skill_run]`. Coding alone excludes web and execution tools.
These additions retain execution and dependency-install approval and do not enable a general shell.
A nonempty `tools.allow` is exclusive: add required names there; `also_allow` cannot extend it.
Explicit `deny` takes priority. `tools.web.enabled` controls construction, and
`execution.network_policy: deny` still blocks native web tools.

Run `echo-agent status --json` for the policy reasons for `skill_run`, `web_fetch`, and `web_search`.
Native search still requires a Brave/Tavily/SerpAPI key or a SearXNG base URL; this change does not add
a native keyless provider. Approved web-search and deep-research scripts share text search through
360 (Chinese first), Bing RSS, then DDGS. News and images use DDGS only. External endpoints can time out,
challenge clients, or change markup; verify original links and article content before using results.

An empty user `data/skills` does not mean bundled skills are absent. Read the actual Linked files using
`skill_view(name="web-search", file_path="scripts/web_search.py")` and execute using `skill_run`.
Paths are relative to the skill root, which can be outside the workspace. Viewing a skill never installs
packages. Current registered tools and structured results take precedence over old conversation or memory
claims about the environment; do not clear user memory to correct a diagnosis.

### Upgrade model caches

Both Embedder and Reranker expand `~` once at initialization and pin relative paths to the initial cwd.
An empty path retains library defaults. Under systemd, home expansion follows the service user;
prefer an absolute writable cache path. Upgrades do not move or delete existing models automatically.

For a cache under a literal `~/.echo-agent/models/fastembed` directory inside the workspace, stop the
specific instance, record the old path, and back up its configuration. Set the correct absolute path.
Only if the destination is absent, move the whole cache there, preserving its contents and permissions.
If it already exists, stop and compare manually; never overwrite it or redownload the entire model.
Restart and check model-loading logs. To roll back, stop the instance, move the directory back to the
recorded location, and restore the configuration backup. The server described in the handoff already
completed migration and needs no further move.

### Package acceptance

Build with the full Dashboard. CI validates editable base, direct wheel `[skills]`, and sdist-rebuilt
wheel `[all]` installs outside the checkout. Each uses an empty user skill directory, reads the three
bundled skills, executes `print(6 * 7)`, and checks policy restrictions. Run
`scripts/validate-installed-skills.py` with the installed Python and `-I`.
`scripts/validate-server-process.py` also drives Gateway tools through a local controlled model,
saving results, model tool records, and SQLite quick_check evidence. Deterministic tests need no public
network or production credentials.
