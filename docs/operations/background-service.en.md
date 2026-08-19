# Background Service

Run Echo Agent as a persistent background service for 24/7 operation.

---

## Linux (systemd)

```bash
# Install the service
echo-agent gateway install

# Manage
echo-agent gateway start
echo-agent gateway stop
echo-agent gateway restart
echo-agent gateway status
echo-agent gateway logs
```

The install command creates a systemd user service (`~/.config/systemd/user/echo-agent.service`) with `lingering` enabled for persistence across logouts.

!!! tip
    Enable lingering: `loginctl enable-linger $USER`

## macOS (launchd)

```bash
echo-agent gateway install
echo-agent gateway start
```

Creates a LaunchAgent plist at `~/Library/LaunchAgents/com.echo-agent.gateway.plist`.

## Alternative: tmux/screen

```bash
tmux new-session -d -s echo-agent 'echo-agent gateway'
tmux attach -t echo-agent
```

## Alternative: Docker

!!! question "Maintainer Decision Required"
    Official Docker image is not yet provided. Community Dockerfile contributions welcome.

## Verifying the Service

```bash
echo-agent status
# Or check health endpoint:
curl http://127.0.0.1:58123/api/v1/health
```

## Uninstalling

```bash
echo-agent gateway stop
echo-agent gateway uninstall
```
