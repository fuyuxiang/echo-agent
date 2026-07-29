"""Clarify primitive — ask the user a question and block the agent until
they answer. Channel-agnostic (mirrors ApprovalManager's Event-based wake):
resolve() may be called from any path — the CLI TUI's /clarify command, or a
future IM adapter. CLI has no timeout; the only unblock is resolve()."""

from __future__ import annotations

import asyncio
import time
import uuid
from dataclasses import dataclass, field


def _as_option_list(options) -> list:
    """Coerce an ``options`` payload into a real list.

    ``list(options)`` on a str explodes it into one entry per character, which
    is how a clarify whose options arrived as JSON *text* ended up offering
    "[", "'", "全" … as its choices. validate_params rejects that shape now;
    this keeps the manager correct for any other caller (IM adapters, tests,
    a tool invoked directly) that bypasses schema validation.
    """
    if options is None:
        return []
    if isinstance(options, (list, tuple)):
        return list(options)
    return [options]


@dataclass
class ClarifyRequest:
    id: str = field(default_factory=lambda: uuid.uuid4().hex[:10])
    question: str = ""
    options: list[str] = field(default_factory=list)
    user_id: str = ""
    session_key: str = ""
    answer: str | None = None
    interrupted: bool = False
    # Monotonic timestamp of registration; only the IM (non-blocking) path reads
    # it, to expire a pending follow-up the user never got around to answering.
    created_at: float = field(default_factory=time.monotonic)


class ClarifyManager:
    """Blocking question/answer registry. One pending entry per clarify_id;
    a dict is used (not a single slot) to stay safe if a future sub-agent
    path ever issues concurrent clarifies.

    A second, independent registry (`_im_pending`, keyed by session_key) backs
    the non-blocking IM follow-up flow: IM channels cannot park a turn in
    wait_for_answer (callback-style, no long-held lock), so instead the agent's
    question is remembered per session and the *next* inbound message on that
    session is consumed as its answer. The two registries never share entries,
    so the CLI blocking semantics (resolve/wait_for_answer/cancel_session) are
    untouched by the IM path."""

    def __init__(self) -> None:
        self._pending: dict[str, ClarifyRequest] = {}
        self._waiters: dict[str, asyncio.Event] = {}
        self._im_pending: dict[str, ClarifyRequest] = {}

    def request(self, question: str, options: list[str] | None = None,
                user_id: str = "", session_key: str = "") -> ClarifyRequest:
        req = ClarifyRequest(question=question, options=_as_option_list(options),
                             user_id=user_id, session_key=session_key)
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

    async def wait_for_answer(self, clarify_id: str) -> tuple[str, bool]:
        req = self._pending.get(clarify_id)
        if req is None:
            return "", False
        # cancel_session may have interrupted this request before the caller
        # started waiting; return the sentinel immediately instead of blocking
        # on a waiter that will never be set again.
        if req.interrupted:
            self._pending.pop(clarify_id, None)
            return (req.answer or ""), True
        waiter = self._waiters.setdefault(clarify_id, asyncio.Event())
        try:
            await waiter.wait()
        finally:
            self._waiters.pop(clarify_id, None)
            self._pending.pop(clarify_id, None)
        return (req.answer or ""), req.interrupted

    def cancel_session(self, session_key: str) -> int:
        count = 0
        for cid, req in list(self._pending.items()):
            if req.session_key == session_key:
                req.interrupted = True
                req.answer = ""
                count += 1
                waiter = self._waiters.get(cid)
                if waiter is not None:
                    waiter.set()
        return count

    def get(self, clarify_id: str) -> ClarifyRequest | None:
        return self._pending.get(clarify_id)

    # ---- IM (non-blocking) follow-up registry, keyed by session_key --------

    def register_im_pending(self, session_key: str, question: str,
                            options: list[str] | None = None, user_id: str = "") -> ClarifyRequest:
        """Remember an IM channel's question so the next inbound message on this
        session can be routed as its answer. Overwrites any prior unanswered
        pending for the same session — the latest question is the live one."""
        req = ClarifyRequest(question=question, options=_as_option_list(options),
                             user_id=user_id, session_key=session_key)
        self._im_pending[session_key] = req
        return req

    def take_im_pending(self, session_key: str, ttl_seconds: float) -> ClarifyRequest | None:
        """Pop the pending IM follow-up for this session if it exists and has not
        expired. Returns None (and clears an expired entry) otherwise, so a stale
        pending degrades to normal message handling rather than mis-binding."""
        req = self._im_pending.get(session_key)
        if req is None:
            return None
        self._im_pending.pop(session_key, None)
        if ttl_seconds > 0 and (time.monotonic() - req.created_at) > ttl_seconds:
            return None
        return req

    def clear_im_pending(self, session_key: str) -> None:
        self._im_pending.pop(session_key, None)
