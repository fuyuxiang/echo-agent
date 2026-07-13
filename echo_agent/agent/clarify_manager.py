"""Clarify primitive — ask the user a question and block the agent until
they answer. Channel-agnostic (mirrors ApprovalManager's Event-based wake):
resolve() may be called from any path — the CLI TUI's /clarify command, or a
future IM adapter. CLI has no timeout; the only unblock is resolve()."""

from __future__ import annotations

import asyncio
import uuid
from dataclasses import dataclass, field


@dataclass
class ClarifyRequest:
    id: str = field(default_factory=lambda: uuid.uuid4().hex[:10])
    question: str = ""
    options: list[str] = field(default_factory=list)
    user_id: str = ""
    answer: str | None = None


class ClarifyManager:
    """Blocking question/answer registry. One pending entry per clarify_id;
    a dict is used (not a single slot) to stay safe if a future sub-agent
    path ever issues concurrent clarifies."""

    def __init__(self) -> None:
        self._pending: dict[str, ClarifyRequest] = {}
        self._waiters: dict[str, asyncio.Event] = {}

    def request(self, question: str, options: list[str] | None = None, user_id: str = "") -> ClarifyRequest:
        req = ClarifyRequest(question=question, options=list(options or []), user_id=user_id)
        self._pending[req.id] = req
        return req

    def resolve(self, clarify_id: str, answer: str) -> bool:
        req = self._pending.pop(clarify_id, None)
        if req is None:
            return False
        req.answer = answer
        waiter = self._waiters.get(clarify_id)
        if waiter is not None:
            waiter.set()
        return True

    async def wait_for_answer(self, clarify_id: str) -> str:
        req = self._pending.get(clarify_id)
        if req is None:
            return ""
        waiter = self._waiters.setdefault(clarify_id, asyncio.Event())
        try:
            await waiter.wait()
        finally:
            self._waiters.pop(clarify_id, None)
            self._pending.pop(clarify_id, None)
        return req.answer or ""

    def get(self, clarify_id: str) -> ClarifyRequest | None:
        return self._pending.get(clarify_id)
