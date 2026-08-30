"""echo-agent cli — thin WebSocket client that attaches to a running
gateway on loopback and opens its own (cli:) session. Builds no agent."""

from __future__ import annotations

import asyncio
from dataclasses import dataclass
from typing import Any

import aiohttp

from echo_agent.cli.i18n import t
from echo_agent.cli.runtime_probe import GatewayState, is_wsl, probe_gateway


class NoGatewayError(Exception):
    """Raised when no local gateway can be reached."""


class MissingTUIDependencyError(Exception):
    """Raised when the optional TUI dependency (textual) is not installed.

    Kept distinct from NoGatewayError so run_cli_attach can surface the
    install hint directly instead of misdirecting the user to gateway/port
    troubleshooting (the gateway may be perfectly healthy)."""


# The TUI uses the theme API (textual.theme, added in 0.86) and native
# content markup with theme variables like [$primary] (added in 2.0). Below
# this floor the app either fails to import or raises MarkupError at render
# time; check up front so the user gets an actionable version error instead
# of a cryptic ModuleNotFoundError deep in the widget tree.
_MIN_TEXTUAL = (8, 2)


def _require_textual() -> None:
    try:
        import textual
    except ImportError as e:
        raise MissingTUIDependencyError(t("attach.ui.missing_textual")) from e

    raw = getattr(textual, "__version__", "0")
    # Parse leading numeric components only (e.g. "8.2.8" -> (8, 2)); ignore
    # any pre-release/build suffix so "9.0.0rc1" still compares correctly.
    parts: list[int] = []
    for token in raw.split(".")[:2]:
        digits = ""
        for ch in token:
            if ch.isdigit():
                digits += ch
            else:
                break
        parts.append(int(digits) if digits else 0)
    version = tuple(parts)

    if version < _MIN_TEXTUAL:
        need = ".".join(str(n) for n in _MIN_TEXTUAL)
        raise MissingTUIDependencyError(
            t("attach.ui.old_textual", installed=raw, required=need)
        )


def build_ws_url(host: str, port: int, ws_path: str) -> str:
    if not ws_path.startswith("/"):
        ws_path = "/" + ws_path
    return f"ws://{host}:{port}{ws_path}"


async def connect_ws(session: aiohttp.ClientSession, url: str) -> aiohttp.ClientWebSocketResponse:
    try:
        ws_timeout_cls = getattr(aiohttp, "ClientWSTimeout", None)
        ws_timeout = ws_timeout_cls(ws_close=10) if ws_timeout_cls else 10
        return await session.ws_connect(url, heartbeat=30, timeout=ws_timeout)
    except (aiohttp.ClientError, OSError, asyncio.TimeoutError) as e:
        raise NoGatewayError(t("attach.ui.cannot_connect", url=url)) from e


def diagnose_no_gateway(url: str, config_path: str | None, workspace: str | None) -> str:
    """Turn a bare connection failure into the one action that applies here.

    Driven by the runtime probe rather than a hardcoded guess: the old version
    always suggested `systemctl is-active echo-agent`, which on WSL2 without
    systemd only prints "System has not been booted with systemd" — a dead end,
    and exactly the environment this failure was reported from.

    Never raises: this runs on the failure path of `echo-agent cli`, where a
    traceback would replace the guidance the user is stuck without."""
    head = t("attach.diagnose.no_gateway", url=url)
    try:
        return _diagnose(head, config_path, workspace)
    except Exception:  # noqa: BLE001 - guidance must survive a broken sub-probe
        return f"{head}\n{t('attach.diagnose.probe_failed')}"


def _diagnose(head: str, config_path: str | None, workspace: str | None) -> str:
    rt = probe_gateway(config_path=config_path, workspace=workspace)

    if rt.state is GatewayState.DISABLED:
        return f"{head}\n{t('startup.disabled')}\n{t('startup.disabled_fix')}"

    if rt.state is GatewayState.NO_SERVICE_MANAGER:
        lines = [head, t("startup.no_manager")]
        if is_wsl():
            lines.append(t("startup.no_manager_wsl"))
        lines.append(t("startup.no_manager_tmux"))
        return "\n".join(lines)

    if rt.state is GatewayState.SERVICE_INSTALLED_STOPPED:
        if rt.service_running:
            # The unit is active but nothing is listening: the process forked and
            # then died in bootstrap (bad API key, port already taken). Another
            # `start` would not help — the log is the only useful next step.
            return f"{head}\n{t('attach.diagnose.service_crashed')}"
        return f"{head}\n{t('attach.diagnose.service_stopped')}"

    if rt.state is GatewayState.NOT_INSTALLED:
        return f"{head}\n{t('attach.diagnose.not_installed')}"

    # RUNNING: the probe says the port is up, yet this connection failed. Most
    # likely a token/path mismatch rather than a missing process.
    return f"{head}\n{t('attach.diagnose.listener_mismatch', host=rt.probe_host, port=rt.effective_port)}"


class AuthError(Exception):
    """WS auth handshake rejected by the server."""


async def authenticate(ws, *, platform: str, user_id: str, session_key: str, token: str) -> str:
    await ws.send_json(
        {
            "type": "auth",
            "platform": platform,
            "user_id": user_id,
            "session_key": session_key,
            "token": token,
        }
    )
    msg = await asyncio.wait_for(ws.receive_json(), timeout=10)
    if msg.get("type") != "auth_ok":
        raise AuthError(msg.get("error") or "auth failed")
    return msg.get("session_key", session_key)


async def fetch_last_assistant_reply(
    session: aiohttp.ClientSession,
    *,
    host: str,
    port: int,
    api_prefix: str,
    session_key: str,
    token: str,
) -> str:
    """Fetch the most recent assistant reply for a session over the history API.

    Used after a reconnect to recover a final reply that the live WS push
    dropped while the socket was down (the gateway does not replay missed
    outbound). Best-effort: any failure returns "" and the caller shows nothing
    extra rather than surfacing a spurious error. Returns the last assistant
    message's text, or "" if none / on failure."""
    prefix = api_prefix if api_prefix.startswith("/") else "/" + api_prefix
    url = f"http://{host}:{port}{prefix.rstrip('/')}/sessions/{session_key}/history?limit=8"
    headers = {"Authorization": f"Bearer {token}"} if token else {}
    try:
        async with session.get(url, headers=headers) as resp:
            if resp.status != 200:
                return ""
            body = await resp.json()
    except (aiohttp.ClientError, OSError, ValueError):
        return ""
    messages = body.get("messages") or []
    for msg in reversed(messages):
        if msg.get("role") == "assistant":
            content = msg.get("content")
            if isinstance(content, str) and content.strip():
                return content
            # Block-style content: concatenate text blocks.
            if isinstance(content, list):
                parts = [b.get("text", "") for b in content if isinstance(b, dict) and b.get("type") == "text"]
                joined = "".join(parts).strip()
                if joined:
                    return joined
            return ""
    return ""


async def fetch_turn_status(
    session: aiohttp.ClientSession,
    *,
    host: str,
    port: int,
    api_prefix: str,
    session_key: str,
    token: str,
    event_id: str = "",
) -> dict:
    """Fetch one durable server-side turn record; empty dict on any failure."""
    from urllib.parse import quote

    prefix = api_prefix if api_prefix.startswith("/") else "/" + api_prefix
    if event_id:
        suffix = f"/turns/{quote(event_id, safe='')}"
    else:
        suffix = f"/sessions/{quote(session_key, safe='')}/turns?limit=1"
    url = f"http://{host}:{port}{prefix.rstrip('/')}{suffix}"
    headers = {"Authorization": f"Bearer {token}"} if token else {}
    try:
        async with session.get(url, headers=headers) as resp:
            if resp.status != 200:
                return {}
            body = await resp.json()
    except (aiohttp.ClientError, OSError, ValueError):
        return {}
    if event_id:
        turn = body.get("turn")
        return turn if isinstance(turn, dict) else {}
    turns = body.get("turns") or []
    return turns[0] if turns and isinstance(turns[0], dict) else {}


async def run_client(
    *,
    host: str,
    port: int,
    ws_path: str,
    user_id: str,
    token: str,
    save_dir=None,
    api_prefix: str = "/api/v1",
    renderer: str = "tui",
    initial_status: dict[str, Any] | None = None,
) -> int:
    from echo_agent.cli.tui.bridge import WSBridge

    if renderer == "tui":
        _require_textual()
        from echo_agent.cli.tui.app import EchoTUI

        app_factory = EchoTUI
    elif renderer == "inline":
        from echo_agent.cli.inline.app import InlineApp

        app_factory = InlineApp
    else:
        raise ValueError(f"unknown cli renderer: {renderer}")

    url = build_ws_url(host, port, ws_path)
    timeout = aiohttp.ClientTimeout(connect=10, sock_connect=10, sock_read=None)
    async with aiohttp.ClientSession(timeout=timeout) as session:
        ws = await connect_ws(session, url)
        session_key = await authenticate(
            ws,
            platform="cli",
            user_id=user_id,
            session_key=f"cli:{user_id}",
            token=token,
        )

        # Mutable connection holder: the live ws is swapped in place on reconnect
        # so send/interrupt closures always target the current socket without
        # being rebound. pump_task is likewise replaced when the pump restarts.
        conn: dict = {"ws": ws, "pump_task": None}

        async def send_coro(text: str) -> None:
            # The gateway may have closed the socket without an error frame; a
            # send on a dead ws would raise. Swallow it and reflect the drop in
            # the status bar instead of crashing the input handler.
            try:
                await conn["ws"].send_json({"type": "message", "text": text})
            except (aiohttp.ClientError, ConnectionError, RuntimeError):
                app.notify_disconnected()

        async def interrupt_coro(target_event_id: str = "") -> None:
            # Control-only frame: asks the gateway to cooperatively stop the
            # running turn. target_event_id (captured from that turn's `accepted`
            # frame) scopes the stop so a delayed frame can't clip a later turn;
            # omitted when unknown → gateway stops whatever is running. Same
            # dead-socket guard as send_coro — an interrupt on a closed ws must
            # not crash the key handler.
            frame: dict = {"type": "interrupt"}
            if target_event_id:
                frame["event_id"] = target_event_id
            try:
                await conn["ws"].send_json(frame)
            except (aiohttp.ClientError, ConnectionError, RuntimeError):
                app.notify_disconnected()

        async def pump() -> None:
            # app.run_async() and this coroutine share one event loop, so
            # bridge.dispatch (which updates widgets) is called directly — no
            # call_from_thread needed.
            async for msg in conn["ws"]:
                if msg.type != aiohttp.WSMsgType.TEXT:
                    break
                try:
                    payload = msg.json()
                except Exception:
                    continue
                try:
                    bridge.dispatch(payload)
                except Exception:
                    continue
            # Loop exit means the socket closed / a non-TEXT frame arrived. No
            # error frame is sent on a clean gateway shutdown, so flip the
            # status bar to disconnected here rather than leaving it "●已连接".
            app.notify_disconnected()

        async def reconnect_coro() -> bool | dict[str, Any]:
            # Rebuild the socket and re-auth reusing the SAME session_key so the
            # gateway rebinds this cli session (server-side history/turn state is
            # keyed by it). Returns True on success. On failure the old (dead)
            # state is left as-is and the caller keeps the disconnected UI.
            try:
                new_ws = await connect_ws(session, url)
                await authenticate(
                    new_ws,
                    platform="cli",
                    user_id=user_id,
                    session_key=session_key,
                    token=token,
                )
            except (NoGatewayError, AuthError, aiohttp.ClientError, OSError):
                return False
            # Tear down the stale pump before swapping the socket so two pumps
            # never read the same holder.
            old = conn.get("pump_task")
            if old is not None:
                old.cancel()
                await asyncio.gather(old, return_exceptions=True)
            old_ws = conn["ws"]
            conn["ws"] = new_ws
            conn["pump_task"] = asyncio.create_task(pump())
            try:
                await old_ws.close()
            except Exception:
                # The replacement socket and its pump are already installed; a
                # stale socket's close failure must not undo a valid reconnect.
                pass
            # Recover from the authoritative turn ledger first. It carries both
            # terminal state and event correlation; old gateways fall back to the
            # text-only history endpoint below.
            latest: dict[str, Any] = {}
            try:
                latest = await fetch_turn_status(
                    session,
                    host=host,
                    port=port,
                    api_prefix=api_prefix,
                    session_key=session_key,
                    token=token,
                )
                if latest.get("response_text"):
                    app.replay_missed_reply(
                        str(latest["response_text"]),
                        str(latest.get("event_id", "")),
                    )
                elif not latest:
                    missed = await fetch_last_assistant_reply(
                        session,
                        host=host,
                        port=port,
                        api_prefix=api_prefix,
                        session_key=session_key,
                        token=token,
                    )
                    if missed:
                        app.replay_missed_reply(missed)
            except Exception:
                # Ledger/history replay is opportunistic after reconnect; the live
                # replacement socket remains authoritative when old servers lack it.
                pass
            return {"ok": True, "turn": latest}

        async def turn_status_coro(event_id: str = "") -> dict:
            return await fetch_turn_status(
                session,
                host=host,
                port=port,
                api_prefix=api_prefix,
                session_key=session_key,
                token=token,
                event_id=event_id,
            )

        app_kwargs = {
            "send_coro": send_coro,
            "session_key": session_key,
            "interrupt_coro": interrupt_coro,
            "reconnect_coro": reconnect_coro,
            "save_dir": save_dir,
        }
        # Keep third-party/test renderer factories source-compatible when there
        # is no telemetry to seed. Real renderers accept this optional mapping.
        if initial_status:
            app_kwargs["initial_status"] = dict(initial_status)
        app = app_factory(**app_kwargs)
        app._turn_status = turn_status_coro
        bridge = WSBridge(app)

        conn["pump_task"] = asyncio.create_task(pump())
        try:
            await app.run_async()
        finally:
            pt = conn.get("pump_task")
            if pt is not None:
                pt.cancel()
                await asyncio.gather(pt, return_exceptions=True)
            await conn["ws"].close()
    # Farewell line after the TUI tears down, so the terminal doesn't just snap
    # back to the shell prompt. Configurable via ECHO_BRAND_GOODBYE. Read via
    # the public accessor (with a getattr fallback) so we don't couple to the
    # App's private _brand field — a test double may not replicate it.
    goodbye = getattr(app, "goodbye_message", None)
    if goodbye:
        print(goodbye)
    return 0


@dataclass(frozen=True)
class ConnectionInfo:
    """Everything `echo-agent cli` needs to attach, from ONE config read.

    resolve_defaults / _resolve_save_dir / _resolve_api_prefix each loaded the
    same file independently — four reads per invocation (the fourth being the
    failure diagnosis), and four DEBUG lines on stderr because the cli path
    never configures a log sink."""

    host: str = "127.0.0.1"
    port: int = 58123
    ws_path: str = "/ws"
    token: str = ""
    api_prefix: str = "/api/v1"
    save_dir: Any = None
    model: str = ""
    context_max: int = 0


def resolve_connection(config_path: str | None, workspace: str | None) -> ConnectionInfo:
    """Read the gateway config once and derive every connection default.

    Host is pinned to loopback — cli is local-only. When gateway.port is 0 the
    static config cannot tell us the real bound port, so fall back to the runtime
    endpoint file the gateway writes on bind; without that, attaching to a
    dynamic-port gateway would try 127.0.0.1:0 and always fail."""
    try:
        from echo_agent.config.loader import load_config, resolve_config_file

        cp = config_path
        if cp is None and workspace:
            cp = str(resolve_config_file(search_dir=workspace) or "") or None
        cfg = load_config(config_path=cp)
    except Exception:  # noqa: BLE001 - fall back to documented defaults
        return ConnectionInfo()

    try:
        gw = cfg.gateway
        token = gw.auth.api_tokens[0] if gw.auth.api_tokens else ""
        port = int(gw.port)
        ws_path = gw.ws_path
        if port == 0:
            ep = _runtime_endpoint(cfg, config_path, workspace)
            if ep and ep.get("port"):
                port = int(ep["port"])
                ws_path = ep.get("ws_path") or ws_path
        # getattr, not attribute access: api_prefix is the one optional field
        # here, and a config object without it must not cost us the host/port we
        # already resolved — that would silently attach to the wrong port.
        api_prefix = getattr(gw, "api_prefix", None) or "/api/v1"
    except Exception:  # noqa: BLE001 - a config we cannot read the gateway out of
        return ConnectionInfo()

    model = ""
    context_max = 0
    try:
        from echo_agent.models.model_windows import resolve_context_window

        model = str(getattr(cfg.models, "default_model", "") or "")
        if model:
            context_max = resolve_context_window(
                model,
                captured_windows=getattr(cfg.models, "model_windows", None),
                config_default=getattr(cfg.session, "context_window_tokens", 0),
            )
    except Exception:
        # This is optimistic first-paint telemetry. The first cost_update frame
        # remains authoritative and will replace it after actual model routing.
        model = ""
        context_max = 0

    save_dir = None
    try:
        from echo_agent.cli.workspace import resolve_effective_workspace

        save_dir = resolve_effective_workspace(cfg, config_path, workspace) / "transcripts"
    except Exception:  # noqa: BLE001 - TUI falls back to ./transcripts
        save_dir = None

    return ConnectionInfo(
        host="127.0.0.1",
        port=port,
        ws_path=ws_path,
        token=token,
        api_prefix=api_prefix,
        save_dir=save_dir,
        model=model,
        context_max=context_max,
    )


def resolve_defaults(config_path: str | None, workspace: str | None) -> tuple[str, int, str, str]:
    """Backwards-compatible view of resolve_connection()."""
    info = resolve_connection(config_path, workspace)
    return info.host, info.port, info.ws_path, info.token


def _runtime_endpoint(cfg, config_path: str | None, workspace: str | None) -> dict | None:
    """Best-effort read of the gateway's runtime endpoint file. Resolves the
    effective workspace the same way the gateway does so both point at the same
    ``.echo-agent/gateway.json``. Returns None if unavailable."""
    try:
        from echo_agent.cli.workspace import (
            read_runtime_endpoint,
            resolve_effective_workspace,
        )

        ws = resolve_effective_workspace(cfg, config_path, workspace)
        return read_runtime_endpoint(ws)
    except Exception:
        return None


def run_cli_attach(
    *,
    host: str,
    port: int,
    ws_path: str,
    user_id: str,
    token: str,
    api_prefix: str = "/api/v1",
    save_dir: Any = None,
    config_path: str | None = None,
    workspace: str | None = None,
    renderer: str = "tui",
    initial_status: dict[str, Any] | None = None,
) -> int:
    # Setup persists ui.locale; attach is a separate process, so restore that
    # preference before importing/rendering either terminal frontend. Restore
    # it afterwards as well because embedders/tests may call this function in a
    # longer-lived process even though the console command normally exits.
    from echo_agent.cli.i18n import detect_locale, get_locale, set_locale

    previous_locale = get_locale()
    try:
        from echo_agent.config.loader import load_config, resolve_config_file

        resolved = config_path
        if resolved is None and workspace:
            resolved = str(resolve_config_file(search_dir=workspace) or "") or None
        locale_pref = load_config(config_path=resolved).ui.locale
        set_locale(detect_locale(None if locale_pref == "auto" else locale_pref))
    except Exception:
        # An unreadable optional preference must not prevent the client from
        # starting; it safely keeps the caller's current locale.
        pass
    try:
        try:
            return asyncio.run(
                run_client(
                    host=host,
                    port=port,
                    ws_path=ws_path,
                    user_id=user_id,
                    token=token,
                    save_dir=save_dir,
                    api_prefix=api_prefix,
                    renderer=renderer,
                    initial_status=initial_status,
                )
            )
        except MissingTUIDependencyError as e:
            # The gateway may be perfectly healthy — this is purely a missing
            # optional dependency, so surface the install hint directly rather
            # than the (misleading) gateway diagnosis.
            print(str(e))
            return 1
        except NoGatewayError:
            url = build_ws_url(host, port, ws_path)
            print(diagnose_no_gateway(url, config_path, workspace))
            return 1
        except AuthError as e:
            print(t("attach.auth_failed", error=e))
            return 1
        except KeyboardInterrupt:
            return 0
    finally:
        set_locale(previous_locale)
