"""Pure text generators for service files (plist / systemd unit).

Kept as side-effect-free functions so unit tests can assert on the exact
rendered payload without touching the filesystem or subprocess.
"""

from __future__ import annotations

import shlex
from xml.sax.saxutils import escape

from echo_agent.cli.service.base import STOP_TIMEOUT_SECONDS

LAUNCHD_LABEL = "com.echo-agent.gateway"

# systemd start-rate limit. 10 attempts at RestartSec=5 spans ~50s of real
# restarts, so a 300s window trips only on a failure that keeps recurring —
# i.e. a permanent one — while leaving plenty of headroom for transient
# start failures to recover. launchd has no equivalent burst cap; its
# ThrottleInterval only paces retries.
START_LIMIT_INTERVAL_SECONDS = 300
START_LIMIT_BURST = 10


def render_launchd_plist(argv: list[str], workdir: str, log_path: str) -> str:
    args_xml = "\n".join(f"        <string>{escape(a)}</string>" for a in argv)
    # KeepAlive + RunAtLoad: restart on crash and start at login.
    # ThrottleInterval prevents a crash-looping gateway from spinning hot.
    # ExitTimeOut must leave room for AppRuntime.stop()'s graceful drain.
    return f"""<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LAUNCHD_LABEL}</string>
    <key>ProgramArguments</key>
    <array>
{args_xml}
    </array>
    <key>WorkingDirectory</key>
    <string>{escape(workdir)}</string>
    <key>EnvironmentVariables</key>
    <dict>
        <key>_ECHO_AGENT_SUPERVISED</key>
        <string>1</string>
    </dict>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>ThrottleInterval</key>
    <integer>10</integer>
    <key>ExitTimeOut</key>
    <integer>{STOP_TIMEOUT_SECONDS}</integer>
    <key>StandardOutPath</key>
    <string>{escape(log_path)}</string>
    <key>StandardErrorPath</key>
    <string>{escape(log_path)}</string>
</dict>
</plist>
"""


def render_systemd_unit(
    argv: list[str],
    workdir: str,
    user: str | None = None,
) -> str:
    """Render a systemd unit. ``user`` is only set for system-scope units;
    user-scope units run as the invoking user implicitly."""
    # shlex.quote each token: systemd splits ExecStart on unquoted whitespace,
    # so a Python interpreter or workspace path containing spaces would be torn
    # into separate arguments without this.
    exec_start = " ".join(shlex.quote(a) for a in argv)
    user_line = f"User={user}\n" if user else ""
    wanted_by = "multi-user.target" if user else "default.target"
    return f"""[Unit]
Description=Echo Agent gateway — resident agent service
After=network-online.target
Wants=network-online.target
# Give up after {START_LIMIT_BURST} failed starts in {START_LIMIT_INTERVAL_SECONDS}s and enter `failed`.
# Restart=always alone turns a PERMANENT failure (bad config — e.g. binding
# 0.0.0.0 with no API token, which the gateway refuses by design) into an
# endless 5s respawn loop that buries the one real error under thousands of
# identical tracebacks. Failing loudly instead makes `systemctl status` answer
# "why is it down?" immediately. Transient failures (network not up, port still
# draining) recover well inside this budget.
StartLimitIntervalSec={START_LIMIT_INTERVAL_SECONDS}
StartLimitBurst={START_LIMIT_BURST}

[Service]
Type=simple
{user_line}ExecStart={exec_start}
WorkingDirectory={workdir}
Environment=_ECHO_AGENT_SUPERVISED=1
Restart=always
RestartSec=5
KillSignal=SIGTERM
TimeoutStopSec={STOP_TIMEOUT_SECONDS}
SuccessExitStatus=0 143

[Install]
WantedBy={wanted_by}
"""
