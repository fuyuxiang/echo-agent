"""A2A HTTP server — agent card and JSON-RPC task handling."""

from __future__ import annotations

import json
import hashlib
from typing import Callable, Protocol, runtime_checkable

from aiohttp import web
from loguru import logger

from echo_agent.a2a.models import AgentCard, A2ATask, A2AMessage, TaskState
from echo_agent.a2a.protocol import A2AProtocol
from echo_agent.a2a.task_store import DEFAULT_TASK_OWNER

# Returning a principal is the authorization success result. ``None`` remains a
# backward-compatible single-owner success for embedders whose callback only
# implemented allow/deny; production Gateway auth returns a distinct principal
# for every configured token.
AuthFn = Callable[[web.Request], str | web.StreamResponse | None]


@runtime_checkable
class DirectProcessor(Protocol):
    """The only capability A2A needs from the agent core."""

    async def process_direct(self, content: str, session_key: str = ..., channel: str = ...) -> str: ...


class A2AServer:
    def __init__(
        self,
        agent_loop: DirectProcessor,
        agent_card: AgentCard,
        auth_fn: AuthFn | None = None,
        *,
        task_ttl_seconds: float | None = None,
        max_tasks: int | None = None,
        active_task_ttl_seconds: float | None = None,
    ):
        self._loop = agent_loop
        self._card = agent_card
        self._auth_fn = auth_fn
        # Only forward what the caller actually set, so A2AProtocol's own
        # defaults stay the single source of truth for retention.
        store_opts: dict[str, float | int] = {}
        if task_ttl_seconds is not None:
            store_opts["task_ttl_seconds"] = task_ttl_seconds
        if max_tasks is not None:
            store_opts["max_tasks"] = max_tasks
        if active_task_ttl_seconds is not None:
            store_opts["active_task_ttl_seconds"] = active_task_ttl_seconds
        self._protocol = A2AProtocol(self._process_task, **store_opts)

    def register_routes(self, app: web.Application) -> None:
        app.router.add_get("/.well-known/agent.json", self._handle_agent_card)
        app.router.add_post("/a2a", self._handle_rpc)
        logger.info("A2A routes registered: /.well-known/agent.json, /a2a")

    async def _handle_agent_card(self, request: web.Request) -> web.Response:
        return web.json_response(self._card.to_dict())

    async def _handle_rpc(self, request: web.Request) -> web.Response:
        principal = DEFAULT_TASK_OWNER
        if self._auth_fn:
            decision = self._auth_fn(request)
            if isinstance(decision, web.StreamResponse):
                return decision
            if decision is not None:
                if not isinstance(decision, str) or not decision.strip():
                    logger.error("A2A auth callback returned an invalid principal")
                    return web.json_response({"error": "unauthorized"}, status=401)
                principal = decision.strip()
        try:
            body = await request.json()
        except json.JSONDecodeError:
            return web.json_response(
                {"jsonrpc": "2.0", "id": None, "error": {"code": -32700, "message": "Parse error"}},
                status=400,
            )
        result = await self._protocol.handle(body, principal=principal)
        return web.json_response(result)

    async def _process_task(self, task: A2ATask) -> A2ATask:
        """Process an A2A task by routing through the agent loop.

        Only text parts are processed. If the inbound user message carries
        non-text parts (files, data, images) we do NOT silently drop them —
        the agent core has no channel to receive attachments — so we tell the
        caller plainly rather than pretending they were handled."""
        # A task ID may be continued with another tasks/send. Process the newly
        # appended user turn, not the first user message forever; replaying the
        # oldest text silently ignored every continuation while still appending
        # agent answers to the task history.
        user_msg = next(
            (m for m in reversed(task.messages) if m.role == "user"), None,
        )
        user_text = user_msg.text_content if user_msg else ""
        dropped = (
            [p.get("type", "unknown") for p in user_msg.parts if p.get("type") != "text"]
            if user_msg else []
        )

        if not user_text:
            task.state = TaskState.FAILED
            detail = (
                f"No text content found. Unsupported parts were ignored: {sorted(set(dropped))}"
                if dropped else "No user message found"
            )
            task.messages.append(A2AMessage.text("agent", detail))
            return task

        try:
            # Keep both memory and session history owner-scoped. Task IDs are
            # caller-controlled and intentionally reusable across principals,
            # so ``a2a:{task.id}`` alone would recreate the same horizontal leak
            # below the now-owner-aware TaskStore.
            session_id = task.id
            if task.owner != DEFAULT_TASK_OWNER:
                session_id = hashlib.sha256(
                    f"{task.owner}\0{task.id}".encode("utf-8")
                ).hexdigest()[:32]
            response_text = await self._loop.process_direct(
                user_text, session_key=f"a2a:{session_id}", channel="a2a",
            )
            if dropped:
                # Text was processed; flag the ignored parts so the caller knows
                # they were not consumed rather than assuming full handling.
                response_text = (
                    f"{response_text or ''}\n\n"
                    f"[note] This agent processes text only; ignored parts: {sorted(set(dropped))}"
                )
            task.state = TaskState.COMPLETED
            task.messages.append(A2AMessage.text("agent", response_text or ""))
        except Exception as e:
            logger.error("A2A task processing failed: {}", e)
            task.state = TaskState.FAILED
            task.messages.append(A2AMessage.text("agent", "Error: task processing failed"))

        return task
