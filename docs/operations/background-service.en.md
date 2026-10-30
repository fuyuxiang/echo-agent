# Background Service

**Scope: standalone Python Runtime.** For Desktop operations, see the [Desktop guide](../desktop/index.md).

`echo-agent gateway install` creates a systemd user service on Linux or a launchd LaunchAgent on macOS. The service manager runs the foreground Gateway process and handles restarts and logs. `echo-agent gateway` also runs directly in the foreground.

## Install and manage

```bash
echo-agent gateway install
echo-agent gateway start
echo-agent gateway status
echo-agent gateway logs
echo-agent gateway logs --follow
echo-agent gateway restart
echo-agent gateway stop
echo-agent gateway uninstall
```

For a custom workspace or configuration, specify paths when installing:

```bash
echo-agent gateway install --workspace /srv/echo-agent --config /srv/echo-agent/echo-agent.yaml
echo-agent gateway start
```

Installation embeds resolved workspace and configuration paths in the service definition. Reinstall and restart the service after changing those paths or the Python installation.

## Linux: systemd

The default unit path is `~/.config/systemd/user/echo-agent.service`. The generated unit uses an absolute Python interpreter path, `-m echo_agent gateway`, a working directory, and `_ECHO_AGENT_SUPERVISED=1`. It sets `Restart=always` with a five-second delay, a limit of ten failed starts within 300 seconds, and a 60-second stop timeout.

```bash
systemctl --user status echo-agent
journalctl --user -u echo-agent -n 100
journalctl --user -u echo-agent -f
```

To keep the user service running after logout, enable lingering separately according to your system policy; installation does not do this automatically:

```bash
sudo loginctl enable-linger "$(whoami)"
```

Linux also supports `echo-agent gateway install --system` for a system-level service, which requires the appropriate permissions.

## macOS: launchd

The default plist path is `~/Library/LaunchAgents/com.echo-agent.gateway.plist`. It sets `RunAtLoad`, `KeepAlive`, a working directory, and `_ECHO_AGENT_SUPERVISED=1`. Both output streams go to `~/.echo-agent/logs/gateway.log`; `echo-agent gateway logs` reads it. The plist's `ExitTimeOut` is 60 seconds.

## Environment and shutdown

A service does not automatically inherit temporary environment variables exported in an interactive shell. Supply model keys and configuration overrides through the service manager's environment configuration, then restart it. Keep real keys out of repository files.

The stop timeout is the service manager's maximum exit window; it does not guarantee that an active Agent turn finishes within 60 seconds. `echo-agent gateway status` reports service state, not the number of active tasks. Stop the process before taking a full database backup; see [backup and restore](backup-restore.en.md).

In a container, run `echo-agent gateway` as the foreground entrypoint instead of installing a systemd or launchd service. See [deployment](deployment.en.md).
