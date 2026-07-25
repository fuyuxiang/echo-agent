"""A2A JSON-RPC protocol handler."""

from __future__ import annotations

import asyncio
from typing import Any, Callable, Awaitable

from loguru import logger

from echo_agent.a2a.models import A2ATask, A2AMessage, TaskState
from echo_agent.a2a.task_store import TERMINAL_STATES as _TERMINAL_STATES, TaskStore

# JSON-RPC 2.0 标准错误码
_JSONRPC_PARSE_ERROR = -32700
_JSONRPC_METHOD_NOT_FOUND = -32601
_JSONRPC_INTERNAL_ERROR = -32603


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

    async def handle(self, request: dict[str, Any]) -> dict[str, Any]:
        """分发 JSON-RPC 请求到对应的处理方法。

        支持的方法: tasks/send（发送任务）, tasks/get（查询任务）, tasks/cancel（取消任务）。
        """
        method = request.get("method", "")
        params = request.get("params", {})
        req_id = request.get("id")

        try:
            if method == "tasks/send":
                result = await self._handle_send(params)
            elif method == "tasks/get":
                result = self._handle_get(params)
            elif method == "tasks/cancel":
                result = self._handle_cancel(params)
            else:
                return self._error(req_id, _JSONRPC_METHOD_NOT_FOUND, f"Method not found: {method}")
            return {"jsonrpc": "2.0", "id": req_id, "result": result}
        except Exception as e:
            logger.error("A2A protocol error: {}", e)
            return self._error(req_id, _JSONRPC_INTERNAL_ERROR, str(e))

    async def _handle_send(self, params: dict[str, Any]) -> dict[str, Any]:
        """处理任务发送请求。如果任务 ID 已存在则追加消息,否则创建新任务。"""
        task_id = params.get("id", "")
        message = params.get("message", {})

        if task_id and task_id in self._tasks:
            task = self._tasks[task_id]
            task.messages.append(A2AMessage.from_dict(message))
        else:
            task = A2ATask()
            if task_id:
                task.id = task_id
            task.messages.append(A2AMessage.from_dict(message))

        # A revived task starts a fresh run: whatever terminal state a previous
        # round confirmed no longer constrains this one.
        self._settled.pop(task.id, None)
        # A2ATask is a mutable dataclass, so assigning .state only changes the
        # object — the store's TTL bookkeeping is keyed off __setitem__. Every
        # state change must therefore be written back, including this one: it
        # re-arms a revived terminal task as active so a later _purge_expired
        # cannot reclaim it while _process is still running.
        task.state = TaskState.WORKING
        self._tasks[task.id] = task

        # Run through a Task so tasks/cancel has something to cancel. Awaiting
        # the coroutine directly gave the cancel handler no handle at all: it
        # could only edit the state field while the work ran on to completion.
        run: asyncio.Task[A2ATask] = asyncio.ensure_future(self._process(task))
        self._runs[task.id] = run
        try:
            task = await run
        except asyncio.CancelledError:
            # Two distinct causes land here and they must not be conflated:
            #   - tasks/cancel cancelled this run → CANCELED is already settled,
            #     keep it and report the cancellation to the sender.
            #   - the client disconnected → aiohttp cancels the request coroutine
            #     and the task would otherwise stay WORKING forever, immune to
            #     both TTL expiry and capacity eviction.
            self._record_state(task, TaskState.CANCELED)
            raise
        except Exception:
            self._record_state(task, TaskState.FAILED)
            raise
        finally:
            # Drop the handle before anything else can observe it: a settled run
            # must never look cancellable.
            self._runs.pop(task.id, None)

        # The worker's own terminal state, unless a cancel already settled one.
        # Without this guard a cancel that arrived mid-run was silently undone —
        # tasks/cancel answered "canceled" and tasks/get then said "completed".
        return self._commit(task).to_dict()

    def _forget(self, task_id: str) -> None:
        """Drop per-task bookkeeping for a task the store just reclaimed.

        Keeps `_settled` bounded by the store's own retention instead of growing
        for the process lifetime — the leak the bounded store was introduced to
        fix would otherwise just move into this side table. `_runs` is keyed by
        in-flight work and cleared in _handle_send's finally, but a task reclaimed
        while somehow still registered would strand a handle, so clear both.
        """
        self._settled.pop(task_id, None)
        self._runs.pop(task_id, None)

    def _commit(self, task: A2ATask) -> A2ATask:
        """Write `task` back unless a terminal state was already confirmed.

        Returns the authoritative task, so the caller reports what every other
        observer will see.
        """
        settled = self._settled.get(task.id)
        if settled is not None:
            # Re-assert rather than merely skip the write: a worker that ignored
            # its cancellation may have mutated this very object (the store holds
            # the same reference) after we answered "canceled".
            stored = self._tasks.get(task.id)
            target = stored if stored is not None else task
            target.state = settled
            return target
        self._tasks[task.id] = task
        return task

    def _record_state(self, task: A2ATask, state: TaskState) -> None:
        """Settle `task` into a terminal state, unless one is already settled."""
        if task.id in self._settled:
            self._commit(task)  # re-assert the settled state, drop this one
            return
        task.state = state
        self._settled[task.id] = state
        self._tasks[task.id] = task

    def _handle_get(self, params: dict[str, Any]) -> dict[str, Any]:
        task_id = params.get("id", "")
        task = self._tasks.get(task_id)
        if not task:
            raise ValueError(f"Task not found: {task_id}")
        return task.to_dict()

    def _handle_cancel(self, params: dict[str, Any]) -> dict[str, Any]:
        task_id = params.get("id", "")
        task = self._tasks.get(task_id)
        if not task:
            raise ValueError(f"Task not found: {task_id}")
        # Honour the A2A contract: a task in a terminal state cannot be canceled.
        if task.state in _TERMINAL_STATES:
            raise ValueError(f"Task '{task_id}' is already {task.state.value} and cannot be canceled")
        # Settle CANCELED *before* cancelling the run, so the worker's own
        # writeback (or its CancelledError handler) cannot overwrite it.
        task.state = TaskState.CANCELED
        self._settled[task_id] = TaskState.CANCELED
        # Write back so the store arms this task's TTL. Skipping it leaves the
        # entry permanently un-expirable and — because capacity eviction only
        # considers TTL-armed entries — stalls eviction for every task behind it.
        self._tasks[task_id] = task
        # Actually stop the work. Cancelling the run is what makes the reported
        # state true: the tool calls in flight stop at their next await point
        # instead of running to completion after we answered "canceled".
        run = self._runs.get(task_id)
        if run is not None and not run.done():
            run.cancel()
        return task.to_dict()

    @staticmethod
    def _error(req_id: Any, code: int, message: str) -> dict[str, Any]:
        return {"jsonrpc": "2.0", "id": req_id, "error": {"code": code, "message": message}}
