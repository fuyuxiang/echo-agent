"""A2A JSON-RPC protocol handler."""

from __future__ import annotations

import asyncio
from typing import Any, Callable, Awaitable

from loguru import logger

from echo_agent.a2a.models import A2ATask, A2AMessage, TaskState
from echo_agent.a2a.task_store import (
    DEFAULT_TASK_OWNER,
    TERMINAL_STATES as _TERMINAL_STATES,
    TaskStore,
)

# JSON-RPC 2.0 标准错误码
_JSONRPC_PARSE_ERROR = -32700
_JSONRPC_INVALID_REQUEST = -32600
_JSONRPC_METHOD_NOT_FOUND = -32601
_JSONRPC_INVALID_PARAMS = -32602
_JSONRPC_INTERNAL_ERROR = -32603
_A2A_TASK_NOT_FOUND = -32001
_A2A_TASK_CONFLICT = -32002
_A2A_TASK_BUSY = -32003
_A2A_CAPACITY_EXHAUSTED = -32004

_MAX_TASK_ID_CHARS = 256


class A2ATaskNotFound(ValueError):
    """Owner-scoped not-found error (also used for cross-owner access)."""


class A2AInvalidParams(ValueError):
    """Invalid caller-controlled JSON-RPC parameters."""


class A2ATaskConflict(ValueError):
    """Requested transition conflicts with an owner-visible task state."""


class A2ATaskBusy(ValueError):
    """A second run was requested for the same owner/task while one is active."""


class A2ACapacityExhausted(ValueError):
    """The process-wide active A2A task admission bound is exhausted."""


class A2AProtocol:
    """Handles A2A JSON-RPC methods: tasks/send, tasks/get, tasks/cancel."""

    def __init__(
        self,
        process_fn: Callable[[A2ATask], Awaitable[A2ATask]],
        *,
        task_ttl_seconds: float = 3600.0,
        max_tasks: int = 1000,
        active_task_ttl_seconds: float | None = None,
        clock: Callable[[], float] | None = None,
    ):
        self._process = process_fn
        # Bounded, TTL store replaces the previous unbounded dict so terminal
        # tasks are reclaimed instead of leaking for the process lifetime.
        store_kwargs: dict[str, Any] = {"ttl_seconds": task_ttl_seconds, "max_tasks": max_tasks}
        if active_task_ttl_seconds is not None:
            store_kwargs["active_ttl_seconds"] = active_task_ttl_seconds
        if clock is not None:
            store_kwargs["clock"] = clock
        self._tasks: TaskStore = TaskStore(on_drop=self._forget, **store_kwargs)
        # task_id -> the asyncio.Task running _process for it. This is what makes
        # tasks/cancel mean something: flipping the state field alone left the
        # worker running, so tool side effects continued and its return value
        # overwrote the CANCELED state with COMPLETED. Entries are removed as
        # soon as the run settles, so this never grows past the in-flight set.
        self._runs: dict[str, asyncio.Task[A2ATask]] = {}
        # task_id -> the terminal state already confirmed to a caller. A terminal
        # state is a promise: once tasks/cancel has answered "canceled", no later
        # writeback may contradict it.
        #
        # The state is stored here, not just a "settled" flag, because the store
        # holds a *reference* to the same mutable A2ATask the worker is holding.
        # A worker that ignores cancellation and sets .state = COMPLETED mutates
        # the stored object in place, so declining to write it back does not undo
        # the damage — the settled state has to be re-asserted onto the object.
        self._settled: dict[str, TaskState] = {}

    async def handle(
        self,
        request: dict[str, Any],
        *,
        principal: str = DEFAULT_TASK_OWNER,
    ) -> dict[str, Any]:
        """分发 JSON-RPC 请求到对应的处理方法。

        支持的方法: tasks/send（发送任务）, tasks/get（查询任务）, tasks/cancel（取消任务）。
        """
        if not isinstance(request, dict):
            return self._error(
                None, _JSONRPC_INVALID_REQUEST, "Invalid Request: expected an object",
            )
        method = request.get("method", "")
        params = request.get("params", {})
        req_id = request.get("id")
        if not isinstance(params, dict):
            return self._error(req_id, _JSONRPC_INVALID_PARAMS, "params must be an object")

        try:
            owner = self._normalize_principal(principal)
            if method == "tasks/send":
                result = await self._handle_send(params, principal=owner)
            elif method == "tasks/get":
                result = self._handle_get(params, principal=owner)
            elif method == "tasks/cancel":
                result = self._handle_cancel(params, principal=owner)
            else:
                return self._error(req_id, _JSONRPC_METHOD_NOT_FOUND, f"Method not found: {method}")
            return {"jsonrpc": "2.0", "id": req_id, "result": result}
        except A2ATaskNotFound:
            # Deliberately identical for a missing ID and an ID owned by another
            # principal. A 403 here would turn tasks/get into an ownership oracle.
            return self._error(req_id, _A2A_TASK_NOT_FOUND, "Task not found")
        except A2ATaskConflict:
            return self._error(
                req_id,
                _A2A_TASK_CONFLICT,
                "Task cannot be canceled in its current state",
            )
        except A2ATaskBusy:
            return self._error(req_id, _A2A_TASK_BUSY, "Task is already running")
        except A2ACapacityExhausted:
            return self._error(
                req_id, _A2A_CAPACITY_EXHAUSTED, "Task capacity is exhausted",
            )
        except A2AInvalidParams as e:
            return self._error(req_id, _JSONRPC_INVALID_PARAMS, str(e))
        except Exception as e:
            logger.error("A2A protocol error: {}", e)
            # Internal exception text can include provider URLs, credentials,
            # file paths, or another tenant's identifiers. Keep diagnostics in
            # server logs and expose only the stable JSON-RPC error class.
            return self._error(req_id, _JSONRPC_INTERNAL_ERROR, "Internal error")

    async def _handle_send(
        self,
        params: dict[str, Any],
        *,
        principal: str = DEFAULT_TASK_OWNER,
    ) -> dict[str, Any]:
        """处理任务发送请求。如果任务 ID 已存在则追加消息,否则创建新任务。"""
        owner = self._normalize_principal(principal)
        task_id = self._validate_task_id(params.get("id", ""), allow_empty=True)
        message = params.get("message", {})
        if not isinstance(message, dict):
            raise A2AInvalidParams("message must be an object")

        # No await occurs between this check and installing the run handle
        # below, so it is atomic on the event loop. A second same-owner send
        # must not overwrite `_runs[storage_key]`: doing so made cancel stop only
        # the newer worker while the first continued side effects untracked.
        requested_storage_key = (
            self._tasks.storage_key(owner, task_id) if task_id else ""
        )
        if requested_storage_key and requested_storage_key in self._runs:
            raise A2ATaskBusy(task_id)

        task = self._tasks.get_owned(owner, task_id) if task_id else None
        if task is not None:
            # Continuing a genuinely active record does not consume another
            # slot. Reviving a terminal record does, so it must pass the same
            # admission bound as a brand-new ID.
            if task.state in _TERMINAL_STATES and not self._has_active_capacity():
                raise A2ACapacityExhausted(task_id)
            task.messages.append(A2AMessage.from_dict(message))
        else:
            if not self._has_active_capacity():
                raise A2ACapacityExhausted(task_id)
            task = A2ATask(owner=owner)
            if task_id:
                task.id = task_id
            task.messages.append(A2AMessage.from_dict(message))

        authoritative_task_id = task.id
        storage_key = self._tasks.storage_key(owner, authoritative_task_id)

        # A revived task starts a fresh run: whatever terminal state a previous
        # round confirmed no longer constrains this one.
        self._settled.pop(storage_key, None)
        # A2ATask is a mutable dataclass, so assigning .state only changes the
        # object — the store's TTL bookkeeping is keyed off __setitem__. Every
        # state change must therefore be written back, including this one: it
        # re-arms a revived terminal task as active so a later _purge_expired
        # cannot reclaim it while _process is still running.
        task.state = TaskState.WORKING
        self._tasks.set_owned(owner, task)

        # Run through a Task so tasks/cancel has something to cancel. Awaiting
        # the coroutine directly gave the cancel handler no handle at all: it
        # could only edit the state field while the work ran on to completion.
        run: asyncio.Task[A2ATask] = asyncio.ensure_future(self._process(task))
        self._runs[storage_key] = run
        try:
            task = await run
        except asyncio.CancelledError:
            # Two distinct causes land here and they must not be conflated:
            #   - tasks/cancel cancelled this run → CANCELED is already settled,
            #     keep it and report the cancellation to the sender.
            #   - the client disconnected → aiohttp cancels the request coroutine
            #     and the task would otherwise stay WORKING forever, immune to
            #     both TTL expiry and capacity eviction.
            task.id = authoritative_task_id
            task.owner = owner
            self._record_state(task, TaskState.CANCELED)
            raise
        except Exception:
            task.id = authoritative_task_id
            task.owner = owner
            self._record_state(task, TaskState.FAILED)
            raise
        finally:
            # Drop the handle before anything else can observe it: a settled run
            # must never look cancellable. Compare before deleting as a defence
            # against any future admission path replacing the slot: an older
            # handler must never erase a newer worker's cancellation handle.
            if self._runs.get(storage_key) is run:
                self._runs.pop(storage_key, None)

        # A processor may return a replacement object. Ownership and the public
        # ID are authorization facts fixed at admission, not worker-controlled
        # output, so re-assert both before any store/side-table lookup.
        task.id = authoritative_task_id
        task.owner = owner

        # The worker's own terminal state, unless a cancel already settled one.
        # Without this guard a cancel that arrived mid-run was silently undone —
        # tasks/cancel answered "canceled" and tasks/get then said "completed".
        return self._commit(task).to_dict()

    def _forget(self, task_id: str) -> None:
        """Drop per-task bookkeeping for a task the store just reclaimed.

        Keeps `_settled` bounded by the store's own retention instead of growing
        for the process lifetime — the leak the bounded store was introduced to
        fix would otherwise just move into this side table.

        If the active-task backstop reclaims genuine in-flight work, dropping its
        run handle would let side effects continue without any way for cancel to
        reach them. Request cancellation, but retain the handle until the owning
        `_handle_send` performs terminal writeback and compare-and-pop cleanup.
        A same-ID send therefore stays BUSY throughout that cancellation window.
        """
        self._settled.pop(task_id, None)
        run = self._runs.get(task_id)
        if run is not None and not run.done():
            run.cancel()

    def _has_active_capacity(self) -> bool:
        """Count actual workers as well as store-visible active records.

        The active TTL may remove a stuck row and request cancellation, but a
        processor is allowed to catch CancelledError and keep running. Such a
        worker still consumes execution/resources and its `_runs` handle must
        continue occupying the admission slot until it truly finishes.
        """
        active = self._tasks.active_storage_keys()
        active.update(
            storage_key
            for storage_key, run in self._runs.items()
            if not run.done()
        )
        return len(active) < self._tasks.max_tasks

    def _commit(self, task: A2ATask) -> A2ATask:
        """Write `task` back unless a terminal state was already confirmed.

        Returns the authoritative task, so the caller reports what every other
        observer will see.
        """
        storage_key = self._tasks.storage_key(task.owner, task.id)
        settled = self._settled.get(storage_key)
        if settled is not None:
            # Re-assert rather than merely skip the write: a worker that ignored
            # its cancellation may have mutated this very object (the store holds
            # the same reference) after we answered "canceled".
            stored = self._tasks.get_owned(task.owner, task.id)
            target = stored if stored is not None else task
            target.state = settled
            return target
        self._tasks.set_owned(task.owner, task)
        return task

    def _record_state(self, task: A2ATask, state: TaskState) -> None:
        """Settle `task` into a terminal state, unless one is already settled."""
        storage_key = self._tasks.storage_key(task.owner, task.id)
        if storage_key in self._settled:
            self._commit(task)  # re-assert the settled state, drop this one
            return
        task.state = state
        self._settled[storage_key] = state
        self._tasks.set_owned(task.owner, task)

    def _handle_get(
        self,
        params: dict[str, Any],
        *,
        principal: str = DEFAULT_TASK_OWNER,
    ) -> dict[str, Any]:
        owner = self._normalize_principal(principal)
        task_id = self._validate_task_id(params.get("id", ""))
        task = self._tasks.get_owned(owner, task_id)
        if not task:
            raise A2ATaskNotFound(task_id)
        return task.to_dict()

    def _handle_cancel(
        self,
        params: dict[str, Any],
        *,
        principal: str = DEFAULT_TASK_OWNER,
    ) -> dict[str, Any]:
        owner = self._normalize_principal(principal)
        task_id = self._validate_task_id(params.get("id", ""))
        storage_key = self._tasks.storage_key(owner, task_id)
        task = self._tasks.get_owned(owner, task_id)
        if not task:
            raise A2ATaskNotFound(task_id)
        # Honour the A2A contract: a task in a terminal state cannot be canceled.
        if task.state in _TERMINAL_STATES:
            raise A2ATaskConflict(
                f"Task '{task_id}' is already {task.state.value} and cannot be canceled"
            )
        # Settle CANCELED *before* cancelling the run, so the worker's own
        # writeback (or its CancelledError handler) cannot overwrite it.
        task.state = TaskState.CANCELED
        self._settled[storage_key] = TaskState.CANCELED
        # Write back so the store arms this task's TTL. Skipping it leaves the
        # entry permanently un-expirable and — because capacity eviction only
        # considers TTL-armed entries — stalls eviction for every task behind it.
        self._tasks.set_owned(owner, task)
        # Actually stop the work. Cancelling the run is what makes the reported
        # state true: the tool calls in flight stop at their next await point
        # instead of running to completion after we answered "canceled".
        run = self._runs.get(storage_key)
        if run is not None and not run.done():
            run.cancel()
        return task.to_dict()

    @staticmethod
    def _normalize_principal(principal: Any) -> str:
        if not isinstance(principal, str) or not principal.strip():
            raise A2AInvalidParams("authenticated principal is missing")
        return principal.strip()

    @staticmethod
    def _validate_task_id(value: Any, *, allow_empty: bool = False) -> str:
        if value in (None, "") and allow_empty:
            return ""
        if not isinstance(value, str) or not value:
            raise A2AInvalidParams("task id must be a non-empty string")
        if len(value) > _MAX_TASK_ID_CHARS:
            raise A2AInvalidParams(f"task id exceeds {_MAX_TASK_ID_CHARS} characters")
        if any(ord(ch) < 32 or ord(ch) == 127 for ch in value):
            raise A2AInvalidParams("task id contains control characters")
        return value

    @staticmethod
    def _error(req_id: Any, code: int, message: str) -> dict[str, Any]:
        return {"jsonrpc": "2.0", "id": req_id, "error": {"code": code, "message": message}}
