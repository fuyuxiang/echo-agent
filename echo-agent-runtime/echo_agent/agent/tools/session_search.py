"""Session search tool — search past session messages."""

from __future__ import annotations

import asyncio
import time
from typing import Any

import regex as re

from echo_agent.tools import Tool, ToolExecutionContext, ToolResult
from echo_agent.session.manager import SessionManager

_SEARCH_TIMEOUT_SECONDS = 5.0


class SessionSearchTool(Tool):
    name = "session_search"
    description = "Search past conversation messages across sessions by keyword or regex."
    parameters = {
        "type": "object",
        "properties": {
            "query": {"type": "string", "description": "Search query (keyword or regex pattern)."},
            "session_key": {"type": "string", "description": "Limit search to a specific session. Omit to search all."},
            "max_results": {"type": "integer", "description": "Maximum results to return.", "default": 20},
            "role_filter": {"type": "string", "enum": ["user", "assistant", "all"], "description": "Filter by message role."},
        },
        "required": ["query"],
    }

    def __init__(self, session_manager: SessionManager):
        self._sessions = session_manager

    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        deadline = time.monotonic() + _SEARCH_TIMEOUT_SECONDS
        query = params["query"]
        session_key = params.get("session_key")
        max_results = params.get("max_results", 20)
        role_filter = params.get("role_filter", "all")

        try:
            pattern = re.compile(query, re.IGNORECASE)
        except re.error:
            pattern = re.compile(re.escape(query), re.IGNORECASE)

        results: list[str] = []

        try:
            if session_key:
                # 只读检索:用 get 而非 get_or_create,搜一个不存在的 key 不应把它建出来。
                session = await self._sessions.get(session_key)
                if session is not None:
                    await asyncio.to_thread(
                        self._search_session, session, pattern, role_filter, results, max_results, deadline,
                    )
            else:
                list_sessions = getattr(self._sessions, "list_sessions_async", None)
                session_items = await list_sessions() if list_sessions else self._sessions.list_sessions()
                for item in session_items:
                    if time.monotonic() >= deadline:
                        raise TimeoutError
                    if len(results) >= max_results:
                        break
                    key = item.get("key", "") if isinstance(item, dict) else str(item)
                    if not key:
                        continue
                    session = await self._sessions.get(key)
                    if session is None:
                        continue
                    await asyncio.to_thread(
                        self._search_session, session, pattern, role_filter, results, max_results, deadline,
                    )
        except TimeoutError:
            return ToolResult(
                success=False, error="Regex search exceeded the request time budget.",
                error_kind="timeout",
            )

        if not results:
            return ToolResult(output="No matches found.")
        return ToolResult(output="\n---\n".join(results), metadata={"count": len(results)})

    def _search_session(self, session, pattern, role_filter, results, limit, deadline):
        for msg in session.messages:
            if len(results) >= limit:
                return
            role = msg.get("role", "")
            if role_filter != "all" and role != role_filter:
                continue
            content = msg.get("content", "")
            if not content or not isinstance(content, str):
                continue
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError
            if pattern.search(content, timeout=remaining):
                snippet = content[:300]
                results.append(f"[{session.key}] {role}: {snippet}")

    def execution_mode(self, params: dict[str, Any]) -> str:
        return "read_only"
