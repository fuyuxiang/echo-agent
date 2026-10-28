# Deployment templates

These templates depend only on the `echo-agent-runtime/` source tree. See the [Runtime README](../README.en.md) for direct Python deployment. Use `echo-agent gateway install` for systemd/launchd service generation.

## Container

From this directory:

```bash
cp echo-agent.yaml.example echo-agent.yaml
# Set the model and two random tokens; set gateway.host to 0.0.0.0.
# Grant container UID 10001 read access to the config, e.g. chown then chmod 600.
docker compose up --build -d
docker compose logs -f echo-agent
```

Supply `OPENAI_API_KEY` to the Compose process. The image installs `[openai]`; adapt the Dockerfile extra and external tools for other capabilities. The build stage compiles the Dashboard; the final image runs the installed Python Runtime without Node/pnpm, Rust or desktop sources.

The port binds to host loopback. Configuration is read-only and workspace data uses a separate volume. The image runs as UID 10001; bind-mounted paths need suitable permissions. Mount `/home/echoagent/.echo-agent/models` separately if model caches must persist. Back up the workspace, config and any configured external paths.

These deployment templates accompany the Runtime sources. Python source/package CI checks do not establish container runtime compatibility; validate on your target Docker platform.
