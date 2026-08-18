from __future__ import annotations

from typing import TYPE_CHECKING

from aiohttp import web

from echo_agent.tasks.models import TaskStatus

if TYPE_CHECKING:
    from echo_agent.gateway.server import GatewayServer


class TasksAPI:
    def __init__(self, server: GatewayServer):
        self._server = server

    def _guard(self, request: web.Request, action: str) -> web.Response | None:
        """Read-level guard: listing and fetching a task."""
        return self._server._require_api_token(request, action=action)

    def _write_guard(self, request: web.Request, action: str) -> web.Response | None:
        """Guard for state changes (create / update / delete / transition / retry).

        These are not reads: creating a task enqueues work for the agent to run,
        and delete/transition can interrupt a turn that is executing (see
        ``_interrupt_session``). They used to accept any read-scope api token and
        skipped CSRF entirely, so a deployment with separate ``admin_tokens`` gave
        every chat-level token the ability to drive the board, and a malicious web
        page could POST to a localhost gateway.

        ``_require_admin_token`` adds both halves: an admin-scoped token, and the
        CSRF + Host-allowlist check that blocks cross-site browser requests.
        """
        return self._server._require_admin_token(request, action=action)

    def _manager(self):
        return self._server._agent_loop.task_manager

    async def list_tasks(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "tasks_list")
        if guard is not None:
            return guard

        status = request.query.get("status")
        assignee = request.query.get("assignee")
        label = request.query.get("label")
        board_id = request.query.get("board_id", "default")

        tasks = await self._manager().list_by_filters(
            status=status, assignee=assignee, label=label, board_id=board_id
        )
        return web.json_response({
            "tasks": [t.to_dict() for t in tasks],
            "total": len(tasks),
        })

    async def create_task(self, request: web.Request) -> web.Response:
        guard = self._write_guard(request, "tasks_create")
        if guard is not None:
            return guard

        try:
            body = await request.json()
        except Exception:
            return web.json_response({"error": "invalid JSON body"}, status=400)

        title = body.get("title", "")
        if not title:
            return web.json_response({"error": "title is required"}, status=400)

        task = await self._manager().create(
            title=title,
            description=body.get("description", ""),
            priority=body.get("priority", 5),
            labels=body.get("labels", []),
            assignee=body.get("assignee", ""),
            source=body.get("source", "human"),
            board_id=body.get("board_id", "default"),
        )
        return web.json_response({"task": task.to_dict()}, status=201)

    async def get_task(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "tasks_get")
        if guard is not None:
            return guard

        task_id = request.match_info["id"]
        task = await self._manager().get(task_id)
        if not task:
            return web.json_response({"error": "not found"}, status=404)
        return web.json_response({"task": task.to_dict()})

    async def update_task(self, request: web.Request) -> web.Response:
        guard = self._write_guard(request, "tasks_update")
        if guard is not None:
            return guard

        task_id = request.match_info["id"]
        try:
            body = await request.json()
        except Exception:
            return web.json_response({"error": "invalid JSON body"}, status=400)

        allowed = {"title", "description", "priority", "labels", "assignee", "blocked_reason", "review_summary", "metadata"}
        fields = {k: v for k, v in body.items() if k in allowed}
        try:
            task = await self._manager().update(task_id, **fields)
        except ValueError as e:
            return web.json_response({"error": str(e)}, status=404)
        return web.json_response({"task": task.to_dict()})

    async def delete_task(self, request: web.Request) -> web.Response:
        guard = self._write_guard(request, "tasks_delete")
        if guard is not None:
            return guard

        task_id = request.match_info["id"]
        task = await self._manager().get(task_id)
        if not task:
            return web.json_response({"error": "not found"}, status=404)
        was_running = task.status == TaskStatus.RUNNING
        session_key = task.session_id
        interrupt_event_id = str(task.metadata.get("_interrupt_event_id") or "")
        try:
            await self._manager().cancel(task_id)
        except ValueError as e:
            return web.json_response({"error": str(e)}, status=409)
        # Cancel is a pure DB status flip; if the task is actually being executed
        # by the agent, also cooperatively stop that turn. We synthesize the same
        # /__interrupt__ control event the WS interrupt path uses, scoped to the
        # task's own session and stamped with the running turn's event_id so it
        # stops exactly that turn (not a later, unrelated one on the same session).
        if was_running and session_key:
            await self._interrupt_session(session_key, interrupt_event_id)
        return web.json_response({"status": "deleted"})

    async def _interrupt_session(self, session_key: str, target_event_id: str) -> None:
        """Best-effort cooperative interrupt of the turn executing a task. Mirrors
        GatewayServer's WS interrupt frame. Never raises — a failed interrupt must
        not fail the cancel, which already persisted."""
        from echo_agent.bus.events import InboundEvent
        bus = getattr(self._server, "_bus", None)
        if bus is None:
            return
        try:
            event = InboundEvent.text_message(
                channel="task",
                sender_id="dispatcher",
                chat_id=session_key,
                text="/__interrupt__",
                session_key_override=session_key,
                is_control=True,
            )
            if target_event_id:
                event.metadata["_interrupt_target_event_id"] = target_event_id
            await bus.publish_inbound(event)
        except Exception:
            pass

    async def transition_task(self, request: web.Request) -> web.Response:
        guard = self._write_guard(request, "tasks_transition")
        if guard is not None:
            return guard

        task_id = request.match_info["id"]
        try:
            body = await request.json()
        except Exception:
            return web.json_response({"error": "invalid JSON body"}, status=400)

        to_status = body.get("to")
        if not to_status:
            return web.json_response({"error": "'to' status is required"}, status=400)

        try:
            new_status = TaskStatus(to_status)
        except ValueError:
            return web.json_response({"error": f"invalid status: {to_status}"}, status=400)

        # Entering RUNNING is the dispatcher's exclusive job: it flips a QUEUED
        # task to RUNNING *and* publishes the agent event that actually executes
        # it. A manual transition to running only changes the DB status — no
        # executor is ever attached — producing an "orphan running" task the
        # dispatcher (which only scans QUEUED) will never pick up or close. Reject
        # it so a task only becomes running by being queued for the dispatcher.
        if new_status == TaskStatus.RUNNING:
            return web.json_response(
                {"error": "cannot transition to running directly; queue the task so the dispatcher runs it"},
                status=400,
            )

        # Snapshot the running context BEFORE the transition: if this task is
        # currently being executed by the agent, moving it OUT of running (cancel,
        # fail, suspend, block from the board) must also cooperatively stop that
        # turn — otherwise the DB says "cancelled" while the agent and its tool
        # side effects keep running. This mirrors delete_task; the generic
        # transition endpoint is what the Kanban actually calls.
        prev = await self._manager().get(task_id)
        was_running = prev is not None and prev.status == TaskStatus.RUNNING
        session_key = prev.session_id if was_running else ""
        interrupt_event_id = str(prev.metadata.get("_interrupt_event_id") or "") if was_running else ""

        try:
            task = await self._manager().transition(task_id, new_status)
        except ValueError as e:
            return web.json_response({"error": str(e)}, status=400)

        # Stop the executing turn when the board moved a running task off "running".
        if was_running and new_status != TaskStatus.RUNNING and session_key:
            await self._interrupt_session(session_key, interrupt_event_id)

        # Terminal transition on a workflow step: advance the owning workflow
        # so its next eligible steps get queued (same hook as TaskTool).
        # Best-effort — the transition already persisted; a failed advance is
        # recoverable via an explicit workflow advance.
        engine = getattr(self._server._agent_loop, "workflow_engine", None)
        if engine is not None and task.workflow_id and new_status in (
            TaskStatus.SUCCESS, TaskStatus.FAILED,
        ):
            try:
                await engine.on_task_complete(task.id)
            except Exception:
                pass
        return web.json_response({"task": task.to_dict()})

    async def retry_task(self, request: web.Request) -> web.Response:
        """Retry a failed task. Distinct from a generic failed→queued transition:
        it goes through manager.retry, which increments retry_count and enforces
        max_retries. Routing retry through the plain /transition endpoint (as the
        board used to) silently bypassed both — a task could be retried forever
        and its attempt count never moved."""
        guard = self._write_guard(request, "tasks_retry")
        if guard is not None:
            return guard

        task_id = request.match_info["id"]
        try:
            task = await self._manager().retry(task_id)
        except ValueError as e:
            return web.json_response({"error": str(e)}, status=400)
        return web.json_response({"task": task.to_dict()})

