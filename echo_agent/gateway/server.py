"""GatewayServer — HTTP/WebSocket server orchestrating all gateway subsystems.

Provides a unified API layer above the channel system for:
- External message ingestion (HTTP POST, WebSocket)
- Session lifecycle management with reset policies
- Authentication and rate limiting
- Cross-platform delivery routing
- Progressive message editing
- Health monitoring
"""

from __future__ import annotations

import asyncio
import json
from pathlib import Path
from typing import Any

import aiohttp
from aiohttp import web
from loguru import logger

from echo_agent.bus.events import InboundEvent, OutboundEvent, ContentBlock, ContentType
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.base import SendResult
from echo_agent.channels.manager import ChannelManager
from echo_agent.channels.qqbot_media import detect_media_kind
from echo_agent.config.schema import GatewayConfig
from echo_agent.gateway.auth import GatewayAuth
from echo_agent.gateway.editor import ProgressiveEditor
from echo_agent.gateway.health import GatewayHealthProvider
from echo_agent.gateway.hooks import HookRegistry
from echo_agent.gateway.host_rules import is_loopback_bind, normalize_host_entries
from echo_agent.gateway.media import MediaCache
from echo_agent.gateway.rate_limiter import RateLimiter
from echo_agent.gateway.router import DeliveryRouter
from echo_agent.gateway.session_context import set_session_vars, clear_session_vars
from echo_agent.gateway.session_policy import SessionResetPolicy
from echo_agent.gateway import ws_common
from echo_agent.gateway.ws_dashboard import DashboardWebSocket
from echo_agent.gateway.ws_session import normalize_platform, resolve_client_session_key
from echo_agent.session.manager import SessionManager


class GatewayServer:
    _MEDIA_KIND_TO_CONTENT_TYPE = {
        "image": ContentType.IMAGE,
        "video": ContentType.VIDEO,
        "voice": ContentType.AUDIO,
        "file": ContentType.FILE,
    }

    def __init__(
        self,
        config: GatewayConfig,
        bus: MessageBus,
        channel_manager: ChannelManager,
        session_manager: SessionManager,
        workspace: Path,
        agent_loop: Any = None,
        a2a_config: Any = None,
    ):
        self._config = config
        self._bus = bus
        self.channel_manager = channel_manager
        self.session_manager = session_manager
        self._workspace = workspace
        self._agent_loop = agent_loop
        self._a2a_config = a2a_config

        self._app: web.Application | None = None
        self._runner: web.AppRunner | None = None
        self._site: web.TCPSite | None = None
        self._ws_clients: dict[str, web.WebSocketResponse] = {}
        self._pending_http: dict[str, asyncio.Future[dict[str, Any]]] = {}
        self._MAX_PENDING_HTTP = 500
        self._running = False
        self._actual_port: int | None = None

        data_dir = workspace / "data"
        # Pass the bind address to the auth so the Host-header check can derive
        # a sensible default allowlist (loopback addresses when bound locally,
        # none when bound to 0.0.0.0/::). For non-loopback binds, an empty
        # allowed_hosts configuration is a deployment mistake: anyone reaching
        # the gateway via DNS could claim to come from a non-existent domain.
        self.auth = GatewayAuth(config.auth, data_dir, bound_host=config.host)
        self._warn_host_allowlist_if_unset()
        self.media_cache = MediaCache(
            cache_dir=workspace / config.media_cache_dir,
            max_size_mb=config.media_cache_max_mb,
            max_file_mb=config.media_max_file_mb,
            concurrency=config.media_download_concurrency,
            allow_private=config.media_allow_private_addresses,
        )
        self.rate_limiter = RateLimiter()
        self.delivery_router = DeliveryRouter(bus)
        self.hooks = HookRegistry()
        self.editor = ProgressiveEditor(bus)
        self.session_policy = SessionResetPolicy(config.session_policy)
        self.health = GatewayHealthProvider(self)
        self._dashboard_ws = DashboardWebSocket(self)
        self._bus.subscribe_outbound_global(self._handle_outbound)

        for name, plat_cfg in config.platforms.items():
            if plat_cfg.rate_limit_rpm:
                self.rate_limiter.configure(name, plat_cfg.rate_limit_rpm)

        if config.hooks_dir:
            hooks_path = workspace / config.hooks_dir
            if hooks_path.is_dir():
                self.hooks.load_from_dir(hooks_path)

    def _normalize_platform(self, reported: str | None) -> str:
        """Fold a client-reported platform onto a gateway-known value.

        Anything explicitly configured under ``gateway.platforms`` counts as known
        on top of the built-in list: a deployment that registered its own platform
        (to give it a rate limit) must keep routing under that name rather than
        collapsing to ws. See ws_session.normalize_platform for why unknown values
        are folded rather than rejected.
        """
        known = getattr(self._config, "known_platforms", None)
        if not isinstance(known, list) or not known:
            # Missing key (older/stubbed config) or explicitly emptied: no folding.
            return normalize_platform(reported, None)
        return normalize_platform(reported, known + list(self._config.platforms or {}))

    async def _reset_session_if_needed(
        self, session_key: str, *, force: bool = False,
    ) -> tuple[Any, bool]:
        """Run the one authoritative reset path under the agent session lock."""
        session = await self.session_manager.get_or_create(session_key)
        if not force and not self.session_policy.should_reset(session):
            return session, False

        # Approval/clarification waits hold the turn lock. An explicit reset is
        # itself the operator's request to abandon that turn, so wake only those
        # waits before trying to acquire the lock. Automatic idle/daily resets
        # do not pre-empt a genuinely active turn.
        if force and self._agent_loop is not None:
            unblock = getattr(self._agent_loop, "unblock_session_for_reset", None)
            if callable(unblock):
                unblock(session_key)

        async def _clear_process_state() -> None:
            if self._agent_loop is None:
                return
            reset_state = getattr(self._agent_loop, "reset_session_state", None)
            if callable(reset_state):
                result = reset_state(session_key)
                if hasattr(result, "__await__"):
                    await result

        acquire = getattr(self.session_manager, "acquire", None)
        if acquire is None:
            # Compatibility for minimal embedders/test doubles. Production's
            # SessionManager always provides the lock.
            await self.session_policy.reset(session, self.session_manager)
            await _clear_process_state()
        else:
            lock = await acquire(session_key)
            async with lock:
                session = await self.session_manager.get_or_create(session_key)
                if not force and not self.session_policy.should_reset(session):
                    return session, False
                await self.session_policy.reset(session, self.session_manager)
                # History epoch and every process-local prompt cache change as
                # one critical section. Otherwise another accepted turn can
                # populate the new epoch in the gap and then have it erased by
                # this reset's late cleanup.
                await _clear_process_state()
        await self.hooks.emit("session_reset", session_key=session_key)
        return session, True

    async def _accept_turn(self, event: InboundEvent, session: Any) -> None:
        # Approval/clarification replies are control traffic for an already
        # running primary turn. Recording them as independent turns makes a
        # reconnect's "latest" lookup report the tiny /approve acknowledgement
        # as completed while the real task is still running.
        command = event.text.strip().split(maxsplit=1)[0].lower() if event.text.strip() else ""
        if event.is_control or command in {"/approve", "/deny", "/approvals", "/clarify"}:
            return
        turn_runs = getattr(self._agent_loop, "turn_runs", None)
        if turn_runs is None:
            return
        from echo_agent.session.context_epoch import conversation_context_key

        try:
            result = turn_runs.accept(
                event.event_id,
                event.session_key,
                context_key=conversation_context_key(event.session_key, session),
                metadata={"channel": event.channel, "chat_id": event.chat_id},
            )
            if hasattr(result, "__await__"):
                await result
        except Exception as e:
            # Status observability must never become a message-ingestion outage.
            logger.warning("Turn acceptance ledger write failed: {}", e)

    async def _reject_turn(self, event_id: str, reason: str) -> None:
        turn_runs = getattr(self._agent_loop, "turn_runs", None)
        if turn_runs is None:
            return
        try:
            result = turn_runs.mark_terminal(event_id, "failed", error=reason)
            if hasattr(result, "__await__"):
                await result
        except Exception as e:
            logger.warning("Turn rejection ledger write failed: {}", e)

    @property
    def is_running(self) -> bool:
        return self._running

    @property
    def dashboard_ws(self) -> DashboardWebSocket:
        """The dashboard WebSocket hub — exposes broadcast() so subsystems (e.g.
        TaskManager) can push real-time events to subscribed UI clients."""
        return self._dashboard_ws

    @property
    def actual_port(self) -> int:
        """Return the actual bound port (useful when configured port is 0)."""
        if self._actual_port is not None:
            return self._actual_port
        return self._config.port

    # ── Lifecycle ────────────────────────────────────────────────────────────

    async def start(self) -> None:
        self._check_bind_safety()
        self._app = web.Application()
        self._setup_routes()

        self._runner = web.AppRunner(self._app)
        await self._runner.setup()
        self._site = web.TCPSite(
            self._runner,
            self._config.host,
            self._config.port,
        )
        try:
            await self._site.start()
        except OSError as e:
            import errno
            if e.errno == errno.EADDRINUSE:
                raise RuntimeError(
                    f"网关端口 {self._config.host}:{self._config.port} 已被占用，"
                    "可能本机已有一个常驻 echo-agent 在运行。若要接入它请用 "
                    "`echo-agent cli`；若要另起实例请用 `--port` 指定其它端口。"
                ) from e
            raise

        actual_port = self._config.port
        if self._runner.addresses:
            actual_port = self._runner.addresses[0][1]
        self._actual_port = actual_port

        self._running = True

        # Persist the actually bound endpoint so attach / `service status` can
        # discover the real port even when gateway.port=0 (ephemeral) — until
        # now it only surfaced on stdout, unreadable to anything not parsing the
        # log. Best-effort: a write failure must not abort a healthy bind.
        import os as _os

        from echo_agent.cli.workspace import clear_runtime_endpoint, write_runtime_endpoint

        try:
            write_runtime_endpoint(
                self._workspace,
                host=self._config.host,
                port=actual_port,
                pid=_os.getpid(),
                ws_path=self._config.ws_path,
            )
            # atexit backstop: if the process exits without a clean stop()
            # (unhandled exception, os._exit), still drop the stale endpoint.
            import atexit

            atexit.register(clear_runtime_endpoint, self._workspace)
        except Exception as e:
            logger.warning("Failed to write gateway runtime endpoint: {}", e)

        await self.hooks.emit("gateway_start")
        logger.info(
            "Gateway listening on {}:{}",
            self._config.host, actual_port,
        )

    def _check_bind_safety(self) -> None:
        """Refuse to expose an unauthenticated gateway beyond localhost.

        With no token of any kind configured, ``authenticate_token`` accepts every
        request — fine on loopback, an open door on 0.0.0.0. Configure
        gateway.auth.apiTokens, or bind to 127.0.0.1.

        adminTokens alone counts as authenticated: an admin token is accepted
        everywhere an API token is (admin implies read), so such a deployment is
        not open — refusing to start would be a false alarm.

        The loopback verdict comes from ``host_rules.is_loopback_bind``, not a
        local string tuple. The tuple used to contain ``""``, which is a
        *wildcard* bind (aiohttp binds it to 0.0.0.0 and ::), so an empty host
        with no token passed this check and exposed an unauthenticated gateway
        to the network. It also rejected ``127.0.0.2``, which is loopback.
        """
        host = (self._config.host or "").strip()
        if is_loopback_bind(host) or self._tokens_configured():
            return
        raise RuntimeError(
            f"Gateway is configured to bind {host}:{self._config.port} without any "
            "API token. Set gateway.auth.apiTokens (or bind to 127.0.0.1) before "
            "exposing the gateway to the network."
        )

    async def stop(self) -> None:
        self._running = False
        await self.hooks.emit("gateway_stop")

        for future in self._pending_http.values():
            if not future.done():
                future.cancel()
        self._pending_http.clear()

        for ws_id, ws in list(self._ws_clients.items()):
            await ws.close(code=aiohttp.WSCloseCode.GOING_AWAY, message=b"shutdown")
        self._ws_clients.clear()

        # Dashboard sockets live in their own registry. Leaving them open meant
        # runner.cleanup() below waited on live handlers — an open browser tab
        # could stall shutdown for aiohttp's shutdown timeout.
        await self._dashboard_ws.close_all()

        if self._site:
            await self._site.stop()
        if self._runner:
            await self._runner.cleanup()

        # Remove the runtime-endpoint file so a later `service status` doesn't
        # report a stale port for a gateway that has since exited.
        from echo_agent.cli.workspace import clear_runtime_endpoint

        clear_runtime_endpoint(self._workspace)

        await self.media_cache.cleanup()
        logger.info("Gateway stopped")

    # ── Route setup ──────────────────────────────────────────────────────────

    def _setup_routes(self) -> None:
        """注册所有 HTTP 和 WebSocket 路由，包括 A2A 协议端点。"""
        prefix = self._config.api_prefix
        app = self._app
        assert app is not None

        app.router.add_get("/playground", self._handle_playground)
        app.router.add_get("/meta", self._handle_meta)
        app.router.add_post(f"{prefix}/message", self._handle_message)
        app.router.add_get(f"{prefix}/health", self._handle_health)
        app.router.add_delete(f"{prefix}/sessions/{{key}}", self._handle_reset_session)
        # Note: GET /sessions is registered by register_management_routes when
        # _agent_loop is available; fallback registered below for standalone mode.
        app.router.add_post(f"{prefix}/pair", self._handle_pair_generate)
        app.router.add_post(f"{prefix}/pair/verify", self._handle_pair_verify)
        app.router.add_get(f"{prefix}/stats", self._handle_stats)
        # Scope probe for the dashboard: lets the UI disable admin-only controls
        # instead of rendering buttons that are guaranteed to 403.
        app.router.add_get(f"{prefix}/capabilities", self._handle_capabilities)
        app.router.add_get(self._config.ws_path, self._handle_websocket)
        app.router.add_get("/ws/dashboard", self._dashboard_ws.handle)

        if self._a2a_config and self._a2a_config.enabled and self._agent_loop:
            from echo_agent.a2a.server import A2AServer
            from echo_agent.a2a.models import AgentCard
            from echo_agent import __version__
            card = AgentCard(
                name=self._a2a_config.agent_name,
                description=self._a2a_config.agent_description,
                url=f"http://{self._config.host}:{self._config.port}",
                version=__version__,
                capabilities=self._a2a_config.capabilities,
            )
            a2a = A2AServer(
                self._agent_loop,
                card,
                auth_fn=lambda req: self._require_api_token(req, action="a2a:rpc"),
                task_ttl_seconds=self._a2a_config.task_ttl_seconds,
                max_tasks=self._a2a_config.max_tasks,
                active_task_ttl_seconds=self._a2a_config.active_task_ttl_seconds,
            )
            a2a.register_routes(app)

        if self._agent_loop:
            from echo_agent.gateway.api import register_management_routes
            register_management_routes(app, prefix, self)
        else:
            app.router.add_get(f"{prefix}/sessions", self._handle_list_sessions)

        # Dashboard SPA catch-all — must be last so it doesn't shadow API routes
        app.router.add_get("/{path:.*}", self._handle_dashboard)

    # ── HTTP handlers ────────────────────────────────────────────────────────

    PLACEHOLDER_CONTINUE = "<!-- more -->"

    def _infer_media_content_type(self, *sources: str, mime_type: str = "") -> ContentType:
        """Classify cached media by extension/MIME, reusing the shared detector so
        the gateway agrees with the channel layer on what counts as image/video/audio."""
        for source in sources:
            kind = detect_media_kind(source, mime_type)
            if kind != "file":
                return self._MEDIA_KIND_TO_CONTENT_TYPE[kind]
        return ContentType.FILE

    def _request_token(self, request: web.Request) -> str:
        token = self.auth.token_from_headers(request.headers)
        if token:
            return token
        return request.query.get("token", "").strip()

    @staticmethod
    def _is_loopback_peer(request: web.Request) -> bool:
        """Whether the request arrives over a loopback socket.

        Derives the verdict from the real TCP peer (``transport.get_extra_info
        ('peername')``), NEVER from ``request.remote`` or forwarded headers such
        as ``X-Forwarded-For`` — those are client-controllable and would let a
        remote caller spoof local trust. Used to grant the loopback exemption in
        the user-authorization gate (see auth.py:is_authorized)."""
        import ipaddress

        transport = getattr(request, "transport", None)
        peername = transport.get_extra_info("peername") if transport else None
        if not peername:
            return False
        host = peername[0]
        try:
            return ipaddress.ip_address(host).is_loopback
        except ValueError:
            return False

    def _check_csrf(self, request: web.Request, *, action: str) -> web.Response | None:
        """Reject cross-site browser requests to mutating endpoints.

        Defends localhost deployments against CSRF-to-localhost / DNS-rebinding:
        a malicious web page cannot drive skills/knowledge just because
        the user's browser can reach 127.0.0.1. Non-browser clients are
        unaffected (they send no Origin/Sec-Fetch-Site).

        Uses the default-on ``is_cross_site_browser`` primitive — the same
        defense as POST /message and the WS handshake. These admin endpoints
        are high-risk, so an unauthenticated loopback deployment must not leave
        them exposed to CSRF-to-localhost."""
        origin = request.headers.get("Origin", "").strip()
        sec_fetch_site = request.headers.get("Sec-Fetch-Site", "").strip()
        host = request.headers.get("Host", "").strip()
        is_browser_request = bool(origin) or sec_fetch_site not in ("", "none")
        if not is_browser_request:
            return None
        # Both gates must pass independently — see reject_cross_site for why
        # same-origin alone is not enough (DNS rebinding makes Origin and Host
        # both attacker-controlled and consistent with each other).
        if self.auth.is_cross_site_browser(origin, sec_fetch_site, host):
            self.auth.audit(action, ok=False, reason=f"cross-site origin rejected: {origin or '?'}")
            return web.json_response({"error": "cross-site request forbidden"}, status=403)
        if not self.auth.is_host_allowed(host):
            self.auth.audit(action, ok=False, reason=f"untrusted host rejected: {host or '?'}")
            return web.json_response({"error": "cross-site request forbidden"}, status=403)
        return None

    def _tokens_configured(self) -> bool:
        """Whether this deployment authenticates at all.

        Must consider BOTH lists, and every "is there a token?" test goes through
        here. Keying such a test on ``api_tokens`` alone is what made a
        deployment with only ``admin_tokens`` serve read endpoints (and the WS
        handshake) unauthenticated, while making the bind-safety check refuse to
        start on 0.0.0.0 — an admin token passes the read guard, so the two lists
        are one hierarchy, not two independent switches."""
        return bool(self._config.auth.api_tokens or self._config.auth.admin_tokens)

    def _warn_host_allowlist_if_unset(self) -> None:
        """Warn when the Host allowlist is empty and the bind is not loopback.

        A non-loopback bind (``0.0.0.0`` / ``::`` / a LAN address) means
        strangers can reach the gateway. Without an explicit ``allowed_hosts``
        entry, ``is_host_allowed`` refuses every Host the server gets — DNS
        rebinding is closed but so is the operator's own browser.

        Scope, stated precisely because the previous wording ("every
        browser-shaped request") overstated it and invited operators to dismiss
        the warning as a false alarm once the dashboard loaded: the Host check
        runs in ``_check_csrf``, which only ``_require_admin_token`` calls. So
        login, the overview page and the other ``_require_api_token`` reads keep
        working, while every admin surface — sessions, config, memory writes,
        tasks, cron and knowledge — 403s.

        Wildcard entries do not count as configured: they cannot appear in a
        Host header, so ``allowed_hosts: [0.0.0.0]`` is an allowlist that
        matches nothing (see ``host_rules.normalize_host_entries``, which is
        what ``GatewayAuth`` itself applies).
        """
        if normalize_host_entries(self._config.auth.allowed_hosts):
            return
        bound = (self._config.host or "").strip()
        if is_loopback_bind(bound):
            return
        logger.warning(
            "Gateway bound to {} with no usable auth.allowed_hosts entry. Admin "
            "endpoints (sessions, config, memory writes, tasks, cron, knowledge) "
            "will reject every browser request for lacking a trusted "
            "Host; read-only pages and native clients still work. List the domain "
            "or address you browse to (e.g. 'echo.example.com') in "
            "gateway.auth.allowed_hosts — note that a wildcard such as '0.0.0.0' "
            "is not a usable entry.",
            bound or "(empty = all interfaces)",
        )

    def _require_api_token(self, request: web.Request, *, action: str) -> web.Response | None:
        """Guard for read/chat-level endpoints. Admin tokens also pass (admin
        scope implies read scope — see auth.authenticate_token)."""
        if not self._tokens_configured():
            return None
        token = self._request_token(request)
        if self.auth.authenticate_token(token):
            self.auth.audit(action, ok=True)
            return None
        self.auth.audit(action, ok=False, reason="invalid api token")
        return web.json_response({"error": "unauthorized"}, status=401)

    def _require_admin_token(self, request: web.Request, *, action: str) -> web.Response | None:
        """Guard for high-risk admin endpoints (skill import/install/delete and
        knowledge upload/delete). Enforces CSRF, then an admin-scoped
        token. The ``?token=`` query backdoor is NOT honoured here — admin
        tokens must travel in a header so they can't leak via referrer/logs or
        be triggered by a cross-site GET."""
        csrf = self._check_csrf(request, action=action)
        if csrf is not None:
            return csrf
        admin = self._config.auth.admin_tokens or self._config.auth.api_tokens
        if not admin:
            return None  # unauthenticated deployment (loopback, no tokens)
        token = self.auth.token_from_headers(request.headers)
        if self.auth.authenticate_admin_token(token):
            self.auth.audit(action, ok=True)
            return None
        self.auth.audit(action, ok=False, reason="invalid admin token")
        return web.json_response({"error": "admin authorization required"}, status=403)

    def _playground_path(self) -> Path:
        return Path(__file__).resolve().parent / "static" / "index.html"

    def _resolve_dashboard_dir(self) -> Path | None:
        """Locate the dashboard SPA build directory.

        Checks two candidates in order:
        1. Bundled in wheel: echo_agent/_bundled/dashboard/
        2. Development: web/dist/ relative to project root (../../web/dist from server.py)
        """
        candidates = [
            Path(__file__).resolve().parent.parent / "_bundled" / "dashboard",
            Path(__file__).resolve().parent.parent.parent / "web" / "dist",
        ]
        for p in candidates:
            if (p / "index.html").exists():
                return p
        return None

    async def _handle_dashboard(self, request: web.Request) -> web.Response:
        """Serve dashboard SPA with fallback to index.html for client-side routing."""
        dashboard_dir = self._resolve_dashboard_dir()
        if dashboard_dir is None:
            return await self._handle_playground(request)

        req_path = request.match_info.get("path", "")
        if req_path:
            file_path = dashboard_dir / req_path
            # Prevent path traversal
            try:
                file_path = file_path.resolve()
                if file_path.is_file() and str(file_path).startswith(str(dashboard_dir.resolve())):
                    return web.FileResponse(file_path)
            except (OSError, ValueError):
                pass
        # SPA fallback — serve index.html for all unmatched paths
        return web.FileResponse(dashboard_dir / "index.html")

    def _authenticate_and_check_rate_limit(
        self, platform: str, user_id: str, chat_id: str, *, trusted: bool = False,
    ) -> str | None:
        """统一的认证和限流检查。

        Args:
            trusted: 来自 loopback socket 的可信请求，跳过用户白名单闸门
                （仍受限流约束）。必须由真实 peer 推导，见 _is_loopback_peer。

        Returns:
            str: 错误信息（如果被拒绝）
            None: 检查通过
        """
        # trusted 只放宽用户白名单闸门（loopback 豁免），不接受 is_authorized 的
        # trusted 形参（Task 1 已移除）：正常授权或可信 loopback 二者其一即放行。
        if not (self.auth.is_authorized(platform, user_id) or trusted):
            self.auth.audit("message", platform=platform, user_id=user_id, ok=False, reason="user unauthorized")
            return "unauthorized"
        if not self.rate_limiter.acquire(platform, chat_id):
            return "rate limited"
        return None

    def _build_outbound_payload(self, event: OutboundEvent) -> dict[str, Any]:
        return {
            "type": "message",
            "event_id": event.event_id,
            "reply_to_id": event.reply_to_id,
            "channel": event.channel,
            "chat_id": event.chat_id,
            "text": event.text,
            "is_final": event.is_final,
            "message_kind": event.message_kind,
            "edit_message_id": event.edit_message_id,
            "metadata": event.metadata,
        }

    async def _handle_playground(self, request: web.Request) -> web.Response:
        path = self._playground_path()
        if path.exists():
            return web.FileResponse(path)
        return web.Response(text="Gateway playground not found.", status=404)

    async def _handle_message(self, request: web.Request) -> web.Response:
        """处理 HTTP 消息请求：认证 → 限流 → 会话管理 → 事件分发。"""
        guard = self._require_api_token(request, action="message")
        if guard is not None:
            return guard
        origin = request.headers.get("Origin", "").strip()
        sec_fetch_site = request.headers.get("Sec-Fetch-Site", "").strip()
        host = request.headers.get("Host", "").strip()
        if self.auth.is_cross_site_browser(origin, sec_fetch_site, host):
            self.auth.audit("message", ok=False, reason=f"cross-site origin rejected: {origin or '?'}")
            return web.json_response({"error": "cross-site request forbidden"}, status=403)
        try:
            body = await request.json()
        except json.JSONDecodeError:
            return web.json_response({"error": "invalid JSON"}, status=400)

        # Folded before anything keys off it: platform reaches channel names,
        # session keys, rate-limit buckets and audit records, and those must all
        # agree on one value. Default stays "api" for a body that omits it.
        platform = self._normalize_platform(body.get("platform") or "api")
        user_id = body.get("user_id", "")
        chat_id = body.get("chat_id", user_id)
        text = body.get("text", "")
        media_urls = body.get("media_urls", [])
        wait = bool(body.get("wait", False))
        is_group = bool(body.get("is_group", False))
        timeout_seconds = max(1, min(int(body.get("timeout_seconds", 180)), 600))

        if not text and not media_urls:
            return web.json_response({"error": "text or media_urls required"}, status=400)

        # Bound the fan-out before any network work. Each URL becomes a guarded
        # download (size-capped, SSRF-checked), but an unbounded *list* would
        # still let one request open arbitrarily many of them at once.
        if not isinstance(media_urls, list):
            return web.json_response({"error": "media_urls must be a list"}, status=400)
        max_urls = self._config.media_max_urls_per_message
        if len(media_urls) > max_urls:
            return web.json_response(
                {"error": f"too many media_urls (max {max_urls})"}, status=400,
            )

        rejection = self._authenticate_and_check_rate_limit(
            platform, user_id, chat_id, trusted=self._is_loopback_peer(request),
        )
        if rejection == "unauthorized":
            await self.hooks.emit("auth_failed", platform=platform, user_id=user_id)
            return web.json_response({"error": "unauthorized"}, status=403)
        if rejection == "rate limited":
            return web.json_response({"error": "rate limited"}, status=429)
        self.auth.audit("message", platform=platform, user_id=user_id, ok=True)

        # Gate B（身份收口，对齐 WS 握手）：仅靠 loopback 豁免放行的客户端
        # （normally_ok=False）拿不到 server 派生的 gateway:{platform}:{chat_id}
        # 兜底键，必须自带 cli: 前缀 key，否则被拒——否则本机裸调用者可自报
        # platform=wechat,user_id=victim 落到他人隔离的 gateway:wechat:victim。
        normally_ok = self.auth.is_authorized(platform, user_id)
        session_key, sk_err = resolve_client_session_key(
            body.get("session_key"),
            platform=platform,
            chat_id=chat_id,
            allow_fallback=normally_ok,
        )
        if sk_err:
            self.auth.audit(
                "message", platform=platform, user_id=user_id, ok=False, reason=sk_err,
            )
            return web.json_response({"error": "forbidden session_key"}, status=403)
        session, _ = await self._reset_session_if_needed(session_key)

        tokens = set_session_vars(
            platform=platform,
            chat_id=chat_id,
            user_id=user_id,
            session_key=session_key,
        )

        try:
            content_blocks = [ContentBlock(type=ContentType.TEXT, text=text)]
            if media_urls:
                paths = await asyncio.gather(
                    *(self.media_cache.download(url, platform) for url in media_urls),
                    return_exceptions=True,
                )
                for url, path in zip(media_urls, paths):
                    if isinstance(path, Exception):
                        logger.warning("Gateway media download failed for {}: {}", url, path)
                        continue
                    if not path:
                        continue
                    content_blocks.append(ContentBlock(
                        type=self._infer_media_content_type(str(path), url),
                        url=str(path),
                    ))

            event = InboundEvent(
                channel=f"gateway:{platform}",
                sender_id=user_id,
                chat_id=chat_id,
                content=content_blocks,
                session_key_override=session_key,
                is_group=is_group,
                metadata={
                    "gateway": True,
                    "platform": platform,
                    "user_id": user_id,
                },
            )
            future: asyncio.Future[dict[str, Any]] | None = None
            if wait:
                if len(self._pending_http) >= self._MAX_PENDING_HTTP:
                    return web.json_response({"error": "too many pending requests"}, status=503)
                future = asyncio.get_running_loop().create_future()
                self._pending_http[event.event_id] = future

            await self._accept_turn(event, session)
            accepted = await self._bus.publish_inbound(event)
            if not accepted:
                self._pending_http.pop(event.event_id, None)
                await self._reject_turn(event.event_id, "server overloaded")
                return web.json_response({"error": "server overloaded"}, status=503)
            await self.hooks.emit(
                "message_received",
                platform=platform, user_id=user_id, chat_id=chat_id,
            )

            if future:
                try:
                    payload = await asyncio.wait_for(future, timeout=timeout_seconds)
                except asyncio.TimeoutError:
                    return web.json_response(
                        {
                            "error": "timeout",
                            "event_id": event.event_id,
                            "session_key": session_key,
                        },
                        status=504,
                    )
                finally:
                    self._pending_http.pop(event.event_id, None)
                return web.json_response(
                    {
                        "status": "completed",
                        "event_id": event.event_id,
                        "session_key": session_key,
                        "reply": payload,
                    }
                )

            return web.json_response({
                "status": "accepted",
                "event_id": event.event_id,
                "session_key": session_key,
            })
        finally:
            clear_session_vars(tokens)

    async def _handle_health(self, request: web.Request) -> web.Response:
        status = await self.health.check()
        code = 200 if status["status"] != "unhealthy" else 503
        return web.json_response(status, status=code)

    async def _handle_list_sessions(self, request: web.Request) -> web.Response:
        guard = self._require_admin_token(request, action="list_sessions")
        if guard is not None:
            return guard
        list_sessions = getattr(self.session_manager, "list_sessions_async", None)
        sessions = await list_sessions() if list_sessions else self.session_manager.list_sessions()
        gateway_sessions = [s for s in sessions if s.get("key", "").startswith("gateway:")]
        return web.json_response({"sessions": gateway_sessions})

    async def _handle_reset_session(self, request: web.Request) -> web.Response:
        guard = self._require_admin_token(request, action="reset_session")
        if guard is not None:
            return guard
        key = request.match_info["key"]
        await self._reset_session_if_needed(key, force=True)
        return web.json_response({"status": "reset", "session_key": key})

    async def _handle_pair_generate(self, request: web.Request) -> web.Response:
        guard = self._require_api_token(request, action="pair_generate")
        if guard is not None:
            return guard
        try:
            body = await request.json()
        except json.JSONDecodeError:
            return web.json_response({"error": "invalid JSON"}, status=400)

        if not body.get("platform", ""):
            return web.json_response({"error": "platform required"}, status=400)
        # Folded with the same rule as /message and the WS handshake. The approved
        # -users store is keyed by platform (auth.py, {platform}_approved.json), so
        # pairing under the raw name while messages arrive under the folded one
        # would file the approval where is_authorized never looks — the client
        # would pair successfully and still be rejected.
        platform = self._normalize_platform(body.get("platform"))

        code = self.auth.generate_pairing_code(platform)
        return web.json_response({"code": code, "ttl_seconds": self._config.auth.pairing_ttl_seconds})

    async def _handle_pair_verify(self, request: web.Request) -> web.Response:
        try:
            body = await request.json()
        except json.JSONDecodeError:
            return web.json_response({"error": "invalid JSON"}, status=400)

        user_id = body.get("user_id", "")
        code = body.get("code", "")

        if not all([body.get("platform", ""), user_id, code]):
            return web.json_response({"error": "platform, user_id, code required"}, status=400)
        # Same fold as pair_generate, so verify looks up the code under the key it
        # was issued with.
        platform = self._normalize_platform(body.get("platform"))

        if self.auth.verify_pairing(platform, user_id, code):
            await self.hooks.emit("auth_success", platform=platform, user_id=user_id)
            return web.json_response({"status": "paired"})
        else:
            await self.hooks.emit("auth_failed", platform=platform, user_id=user_id)
            return web.json_response({"error": "invalid or expired code"}, status=403)

    async def _handle_meta(self, request: web.Request) -> web.Response:
        """Bootstrap metadata for the dashboard SPA.

        No authentication required — the frontend needs this before it has a
        token. Exposed at a fixed path (/meta) so the SPA can locate the real
        API prefix without hardcoding it.
        """
        from echo_agent import __version__
        return web.json_response({
            "api_prefix": self._config.api_prefix,
            "ws_path": "/ws/dashboard",
            "version": __version__,
            "auth_required": self._tokens_configured(),
        })

    async def _handle_capabilities(self, request: web.Request) -> web.Response:
        """What the *calling token* is allowed to do.

        The dashboard mixes api-token and admin-token endpoints on the same page
        (knowledge upload/delete, config, memory writes). Without this the UI can
        only discover the boundary by firing a request and reading a 403, so it
        rendered enabled buttons that were guaranteed to fail. Reporting the
        caller's own scope lets those affordances be disabled up front with an
        explanation.

        Deliberately reports only booleans about the presented token — never the
        configured tokens or whether any exist beyond what the caller's own scope
        already tells them.

        ``auth_required`` says whether this deployment authenticates at all. The
        dashboard needs it because it treated ``!!token`` as "logged in": in the
        officially supported open / no-token mode (see auth.authenticate_token,
        which accepts every request when no token is configured) an empty token
        is *correct*, yet Layout bounced it to /login, Login's probe succeeded
        and navigated back to /, and Layout bounced it again — a redirect loop
        out of which only typing a nonsense non-empty token could escape.
        Reporting the fact server-side is what lets the UI stop guessing."""
        guard = self._require_api_token(request, action="capabilities")
        if guard is not None:
            return guard
        # Mirrors _require_admin_token's own resolution order, including the
        # unauthenticated-deployment case (no tokens configured at all → every
        # caller is effectively admin), so the UI never disables a control the
        # server would in fact allow.
        admin_configured = bool(
            self._config.auth.admin_tokens or self._config.auth.api_tokens
        )
        if not admin_configured:
            is_admin = True
        else:
            is_admin = self.auth.authenticate_admin_token(
                self.auth.token_from_headers(request.headers)
            )
        return web.json_response({
            "admin": is_admin,
            "authRequired": self._tokens_configured(),
        })

    async def _handle_stats(self, request: web.Request) -> web.Response:
        guard = self._require_api_token(request, action="stats")
        if guard is not None:
            return guard
        # ws_clients is now part of health.check() itself, so no need to re-add it.
        health_data = await self.health.check()
        return web.json_response(health_data)

    # ── WebSocket handler ─────────────────────────────────────────────────

    async def _handle_websocket(self, request: web.Request) -> web.StreamResponse:
        """处理 WebSocket 连接：Origin 闸门 → 认证握手 → 消息循环 → 事件分发。"""
        # Gate A: reject cross-site browser upgrades BEFORE prepare(). Once the
        # socket is upgraded the browser's onopen fires, so this must run first.
        # Shared with the dashboard WS via ws_common so the two gates cannot
        # drift apart again.
        rejected = ws_common.reject_cross_site(request, self.auth, action="ws_auth")
        if rejected is not None:
            return rejected

        # Server-driven heartbeat: without it keepalive was entirely client-side
        # (the CLI pings, but nothing pinged the CLI), so a stalled/blocked client
        # during a long turn could die unnoticed and the final reply would be
        # dropped silently. aiohttp auto-pongs peer pings and drops the socket if
        # our ping goes unanswered, surfacing the dead connection promptly.
        hb = self._config.ws_heartbeat_seconds
        websocket = web.WebSocketResponse(heartbeat=hb if hb and hb > 0 else None)
        await websocket.prepare(request)

        delivery_key = None
        platform = "ws"
        user_id = ""
        chat_id = ""
        session_key = ""
        # Absolute bound on the pre-auth window. Passing the per-frame timeout to
        # each wait_for restarted the clock on every frame, so a peer that kept
        # sending frames it had no right to send (junk, bad JSON, `message`
        # before `auth`) renewed its own deadline forever. See ws_common.
        auth_deadline = ws_common.AuthDeadline()

        try:
            while True:
                try:
                    # Once authenticated the socket is a legitimate long-lived
                    # client — a TUI turn can run for many minutes with no client
                    # frames — so remaining() drops the bound entirely.
                    raw_msg = await asyncio.wait_for(
                        websocket.receive(), timeout=auth_deadline.remaining(),
                    )
                except asyncio.TimeoutError:
                    self.auth.audit("ws_auth", ok=False, reason="authentication timeout")
                    await websocket.close()
                    break

                if raw_msg.type == aiohttp.WSMsgType.TEXT:
                    try:
                        data = json.loads(raw_msg.data)
                    except json.JSONDecodeError:
                        await websocket.send_json({"error": "invalid JSON"})
                        continue

                    # Per-message isolation: a failure while handling one
                    # message (audit, content build, publish, session ops…)
                    # must return an error frame and continue, never propagate
                    # out of the loop and silently drop the connection. Only
                    # genuine socket-level errors reach the outer handler below.
                    try:
                        msg_type = data.get("type", "message")

                        if msg_type == "auth":
                            # Folded up-front so every downstream use — the
                            # authorization check, the session key, the delivery
                            # key, the rate-limit bucket, the audit trail and the
                            # inbound channel name — sees the same value. In
                            # particular the channel name decides whether this
                            # client is served retractable drafts, so it must not
                            # be a free-form client string.
                            platform = self._normalize_platform(data.get("platform"))
                            user_id = data.get("user_id", "")
                            chat_id = data.get("chat_id", user_id)
                            # 握手接受三个来源:auth 帧内的 token、请求头、URL 的
                            # ?token=。后者是历史入口,对 api 作用域保留。
                            frame_token = str(data.get("token") or "")
                            token = str(frame_token or self._request_token(request))

                            # Any configured token makes the check mandatory —
                            # admin_tokens alone must not leave the socket open,
                            # and authenticate_token already accepts both kinds.
                            if self._tokens_configured() and not self.auth.authenticate_token(token):
                                self.auth.audit("ws_auth", platform=platform, user_id=user_id, ok=False, reason="invalid api token")
                                await websocket.send_json({"type": "error", "error": "unauthorized"})
                                await websocket.close()
                                return websocket

                            trusted = self._is_loopback_peer(request)
                            normally_ok = self.auth.is_authorized(platform, user_id)
                            if not (normally_ok or trusted):
                                self.auth.audit("ws_auth", platform=platform, user_id=user_id, ok=False, reason="user unauthorized")
                                await websocket.send_json({"type": "error", "error": "unauthorized"})
                                await websocket.close()
                                return websocket

                            # A client let in ONLY by the loopback exemption gets no
                            # server-derived fallback key — it must present an explicit
                            # cli: key, else it could self-report another user's identity.
                            session_key, sk_err = resolve_client_session_key(
                                data.get("session_key"),
                                platform=platform,
                                chat_id=chat_id,
                                allow_fallback=normally_ok,
                            )
                            if sk_err:
                                self.auth.audit(
                                    "ws_auth", platform=platform, user_id=user_id,
                                    ok=False, reason=sk_err,
                                )
                                await websocket.send_json({"type": "error", "error": "forbidden session_key"})
                                await websocket.close()
                                return websocket
                            # 身份键 session_key（cli 自带时=cli:alice）用于会话隔离与冒充拒绝；
                            # 投递键 delivery_key 用出站重算式 gateway:{platform}:{chat_id}，
                            # 让 _handle_outbound 无需任何 metadata 透传即可命中所有出站路径。
                            delivery_key = f"gateway:{platform}:{chat_id}"
                            self._ws_clients[delivery_key] = websocket
                            # Lifts the pre-auth deadline. Set here, past every
                            # rejection branch above, so a failed handshake never
                            # buys an unbounded socket.
                            auth_deadline.mark_authenticated()

                            session, _ = await self._reset_session_if_needed(session_key)

                            await websocket.send_json({"type": "auth_ok", "session_key": session_key})
                            self.auth.audit("ws_auth", platform=platform, user_id=user_id, ok=True)
                            await self.hooks.emit(
                                "auth_success", platform=platform, user_id=user_id,
                            )
                            continue

                        if msg_type == "message":
                            if not session_key:
                                await websocket.send_json({"type": "error", "error": "authenticate first"})
                                continue

                            text = data.get("text", "")
                            is_group = bool(data.get("is_group", False))
                            if not text:
                                continue

                            if not self.rate_limiter.acquire(platform, chat_id):
                                await websocket.send_json({"type": "error", "error": "rate limited"})
                                continue

                            tokens = set_session_vars(
                                platform=platform,
                                chat_id=chat_id,
                                user_id=user_id,
                                session_key=session_key,
                            )
                            try:
                                event = InboundEvent.text_message(
                                    channel=f"gateway:{platform}",
                                    sender_id=user_id,
                                    chat_id=chat_id,
                                    text=text,
                                    session_key_override=session_key,
                                    is_group=is_group,
                                )
                                event.metadata["gateway"] = True
                                event.metadata["platform"] = platform
                                await self._accept_turn(event, session)
                                if not await self._bus.publish_inbound(event):
                                    await self._reject_turn(event.event_id, "server overloaded")
                                    await websocket.send_json({"type": "error", "error": "server overloaded"})
                                    continue
                                await websocket.send_json({
                                    "type": "accepted",
                                    "event_id": event.event_id,
                                })
                            finally:
                                clear_session_vars(tokens)

                        if msg_type == "interrupt":
                            # Cooperative stop of the session's running turn.
                            # Routed as an internal control command that the loop
                            # intercepts BEFORE the session lock (the running turn
                            # holds that lock), so the inference loop can poll the
                            # flag and converge cleanly. is_control bypasses the
                            # rate limiter, mirroring the clarify-cancel escape
                            # valve — same reasoning: a user who just flooded the
                            # session is exactly who needs the stop to land.
                            if not session_key:
                                await websocket.send_json({"type": "error", "error": "authenticate first"})
                                continue
                            interrupt_event = InboundEvent.text_message(
                                channel=f"gateway:{platform}",
                                sender_id=user_id,
                                chat_id=chat_id,
                                text="/__interrupt__",
                                session_key_override=session_key,
                                is_control=True,
                            )
                            # The client echoes the target turn's event_id (learned
                            # from that turn's `accepted` frame). Stamp it so the
                            # loop only stops that turn — a stop frame delayed past
                            # the turn's end can't clip the next one. Absent for
                            # older clients → stop whatever is running.
                            target_id = data.get("event_id")
                            if target_id:
                                interrupt_event.metadata["_interrupt_target_event_id"] = str(target_id)
                            # Only ACK if the interrupt actually entered the bus. A full queue
                            # or a stopped bus returns False; claiming "accepted" then
                            # would tell the user the turn was stopped when the stop
                            # frame was silently dropped. Mirror the normal-send path.
                            if not await self._bus.publish_inbound(interrupt_event):
                                await websocket.send_json({"type": "error", "error": "server overloaded"})
                                continue
                            await websocket.send_json({"type": "accepted"})

                        if msg_type == "ping":
                            await websocket.send_json({"type": "pong"})

                    except Exception as e:
                        # One message failed to process — report it and keep the
                        # connection alive. This is what stops a stray per-message
                        # error (a transient audit/session fault)
                        # from silently tearing down the whole session.
                        logger.warning("WebSocket message handling failed: {}", e)
                        try:
                            await websocket.send_json({"type": "error", "error": "internal error"})
                        except Exception:
                            # Send failed → the socket itself is gone; let the
                            # outer loop observe the close on the next iteration.
                            pass
                        continue

                elif raw_msg.type in (
                    aiohttp.WSMsgType.ERROR,
                    aiohttp.WSMsgType.CLOSE,
                    # CLOSED / CLOSING were implicit while this was `async for`
                    # (the iterator stops on them). With an explicit receive()
                    # they must be handled here, or teardown spins forever.
                    aiohttp.WSMsgType.CLOSED,
                    aiohttp.WSMsgType.CLOSING,
                ):
                    break

        except Exception as e:
            logger.error("WebSocket error: {}", e)
        finally:
            # Only drop the slot if it still holds *this* socket — when two
            # connections collapse onto the same delivery_key, an earlier
            # connection's teardown must not delete the later one's slot.
            if delivery_key and self._ws_clients.get(delivery_key) is websocket:
                del self._ws_clients[delivery_key]
            # Escape valve: a disconnect (/quit, Ctrl+C, dropped socket) must wake
            # any clarify blocked on this session so the agent does not stay parked
            # in wait_for_answer until the 24h registry backstop. Route it through
            # the bus as an internal control command that the loop intercepts
            # before the session lock. Best-effort — never let this break teardown.
            if session_key:
                try:
                    cancel_event = InboundEvent.text_message(
                        channel=f"gateway:{platform}",
                        sender_id=user_id,
                        chat_id=chat_id,
                        text="/__clarify_cancel__",
                        session_key_override=session_key,
                        # Trusted internal producer: bypass the rate limiter so a
                        # user who just flooded the session can't get this escape
                        # valve dropped, leaving the agent parked until the 24h backstop.
                        is_control=True,
                    )
                    await self._bus.publish_inbound(cancel_event)
                except Exception as e:
                    logger.warning("Clarify cancel on ws disconnect failed: {}", e)

        return websocket

    async def _handle_outbound(self, event: OutboundEvent) -> SendResult | None:
        """Deliver a gateway-bound event to its live client, reporting the truth.

        Returns ``None`` — "no opinion" — for anything this handler does not own.
        That matters because this is a *global* outbound handler: it sees every
        channel's events, and ``MessageBus._aggregate`` folds a returned
        SendResult into the delivery verdict. Voting FAILED on, say, a Telegram
        event because no WebSocket was attached would fault deliveries that in
        fact succeeded on their own channel.

        For events it does own, the receipt is real. Previously this returned
        None unconditionally, which ``_aggregate`` reads as ACCEPTED — counted as
        success by ``DeliveryResult.ok``. So a turn whose answer reached nobody
        (client gone, no HTTP waiter) still reported success, and the cron run or
        task that produced it was marked complete. The warning below already knew
        the reply had been dropped; it just never told the caller.
        """
        if event.metadata.get("_drop"):
            return None
        if not event.channel.startswith("gateway:"):
            return None

        _, platform = event.channel.split(":", 1)
        session_key = f"gateway:{platform}:{event.chat_id}"
        payload = self._build_outbound_payload(event)

        # An HTTP waiter is a real delivery target: the caller is blocked on this
        # reply and will receive it, whether or not a WebSocket is also attached.
        answered_http_waiter = False
        correlation_id = str(event.metadata.get("_inbound_event_id") or event.reply_to_id or "")
        if correlation_id:
            future = self._pending_http.get(correlation_id)
            if future is not None and not future.done() and event.is_final:
                try:
                    future.set_result(payload)
                    answered_http_waiter = True
                except asyncio.InvalidStateError:
                    pass
                self._pending_http.pop(correlation_id, None)

        delivered = await self.broadcast_to_ws(session_key, payload)
        if delivered or answered_http_waiter:
            return SendResult(success=True)

        is_final = event.is_final or event.message_kind == "final"
        if not is_final:
            # Interim stream frames are level-triggered progress: the final still
            # carries the full text, so a dropped one is not a delivery failure.
            # Stay silent rather than faulting the turn over a skipped frame.
            return None

        # A dropped FINAL reply is the severe case: the turn's answer was
        # produced and persisted to history but never reached the live client
        # (closed/rebound socket), and there is no replay — so the CLI shows
        # nothing. Log it loudly with the routing key so the silent-drop is
        # diagnosable instead of vanishing.
        logger.warning(
            "Outbound FINAL reply not delivered to live client "
            "(session_key={}, event_id={}): socket missing or closed. "
            "Reply is persisted to history but the attached client missed it.",
            session_key, event.event_id,
        )
        return SendResult(
            success=False,
            error="no live gateway client for this session (reply persisted to history only)",
        )

    async def broadcast_to_ws(self, session_key: str, data: dict[str, Any]) -> bool:
        ws = self._ws_clients.get(session_key)
        if ws is None:
            logger.debug("broadcast_to_ws: no live client for session_key={}", session_key)
            return False
        if ws.closed:
            logger.debug("broadcast_to_ws: client socket closed for session_key={}", session_key)
            return False
        try:
            await ws.send_json(data)
            return True
        except Exception as e:
            logger.warning("Failed to send WebSocket message (session_key={}): {}", session_key, e)
            return False
