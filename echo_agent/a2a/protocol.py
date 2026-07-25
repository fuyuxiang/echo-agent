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
        self._tasks: TaskStore = TaskStore(**store_kwargs)

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
        """处理任务发送请求。如果任务 ID 已存在则追加消息，否则创建新任务。"""
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

        # A2ATask is a mutable dataclass, so assigning .state only changes the
        # object — the store's TTL bookkeeping is keyed off __setitem__. Every
        # state change must therefore be written back, including this one: it
        # re-arms a revived terminal task as active so a later _purge_expired
        # cannot reclaim it while _process is still running.
        task.state = TaskState.WORKING
        self._tasks[task.id] = task
        try:
            task = await self._process(task)
        except BaseException as e:
            # Includes CancelledError: aiohttp cancels the request coroutine when
            # a client disconnects, and _process is the long await where that
            # lands. Without this, the task would stay WORKING forever — immune
            # to TTL expiry AND to capacity eviction, since the store never
            # evicts what it believes is still in flight.
            task.state = TaskState.CANCELED if isinstance(e, asyncio.CancelledError) else TaskState.FAILED
            self._tasks[task.id] = task
            raise
        self._tasks[task.id] = task
        return task.to_dict()

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
        # tasks/send runs synchronously to completion, so by the time a separate
        # cancel request arrives the task is almost always already terminal.
        # Honour the A2A contract: a task in a terminal state cannot be canceled.
        if task.state in _TERMINAL_STATES:
            raise ValueError(f"Task '{task_id}' is already {task.state.value} and cannot be canceled")
        task.state = TaskState.CANCELED
        # Write back so the store arms this task's TTL. Skipping it leaves the
        # entry permanently un-expirable and — because capacity eviction only
        # considers TTL-armed entries — stalls eviction for every task behind it.
        self._tasks[task_id] = task
        return task.to_dict()

    @staticmethod
    def _error(req_id: Any, code: int, message: str) -> dict[str, Any]:
        return {"jsonrpc": "2.0", "id": req_id, "error": {"code": code, "message": message}}
