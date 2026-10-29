# Deployment templates

These templates depend only on the `echo-agent-runtime/` source tree. See the [Runtime README](../README.en.md) for direct Python deployment. Use `echo-agent gateway install` for systemd/launchd service generation.

## Source installations and scheduled jobs

The deployment example explicitly enables the scheduler and allows `cronjob`,
`web_fetch`, and `skill_run`. The `coding` profile includes `cronjob`; approval
and per-job unattended authorization remain required for privileged work.
Public gateway restrictions, explicit denies, and exclusive allowlists still apply.
After creation, verify the job ID, next run, authorization, and final delivery state.

Run `echo-agent setup doctor --json` as the service user and check
`df -h /actual/workspace`. On Linux, compare the disk and partition sizes with
`lsblk`: expanding a cloud disk does not automatically expand its partition and
filesystem. Root-reserved blocks are not available to an ordinary service user.
Doctor warns below 512 MiB available; below 64 MiB it fails and `/api/v1/ready`
reports not ready.

Checkpoints filter runtime paths, their own store, and oversized files before
writing Git blobs, and skip snapshots when free space is insufficient. Weixin
state writes replace files atomically, preserving the previous complete state
on write failure. Text/media sends accept server `message_id` receipts while
still rejecting explicit error codes.

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
