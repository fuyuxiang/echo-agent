"""Pure text generators for service files (plist / systemd unit).

Kept as side-effect-free functions so unit tests can assert on the exact
rendered payload without touching the filesystem or subprocess.
"""

from __future__ import annotations

from xml.sax.saxutils import escape

from echo_agent.cli.service.base import STOP_TIMEOUT_SECONDS

LAUNCHD_LABEL = "com.echo-agent.gateway"


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
    exec_start = " ".join(argv)
    user_line = f"User={user}\n" if user else ""
    wanted_by = "multi-user.target" if user else "default.target"
    return f"""[Unit]
Description=Echo Agent gateway — resident agent service
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
{user_line}ExecStart={exec_start}
WorkingDirectory={workdir}
Restart=always
RestartSec=5
KillSignal=SIGTERM
TimeoutStopSec={STOP_TIMEOUT_SECONDS}
SuccessExitStatus=0 143

[Install]
WantedBy={wanted_by}
"""
