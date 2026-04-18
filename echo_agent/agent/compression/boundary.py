"""Phase 2 — Boundary resolution.

Determines head / middle / tail segments for compression, ensuring
tool_call/tool_result pairs are never split across boundaries.
"""

from __future__ import annotations

from typing import Any, Callable

from echo_agent.agent.compression.types import BoundaryResult


class BoundaryResolver:

    def __init__(
        self,
        head_protect_count: int,
        tail_budget_ratio: float,
        context_window_tokens: int,
        token_estimator: Callable[[list[dict[str, Any]]], int],
    ):
        self._head_protect = head_protect_count
        self._tail_budget = int(context_window_tokens * tail_budget_ratio)
        self._estimate = token_estimator

    def resolve(self, messages: list[dict[str, Any]]) -> BoundaryResult:
        n = len(messages)
        if n == 0:
            return BoundaryResult(
                head_end=0, tail_start=0,
                head_messages=[], middle_messages=[], tail_messages=[],
                no_compression_needed=True,
            )

        head_end = min(self._head_protect, n)
        head_end = self._align_forward(messages, head_end)

        tail_start = self._find_tail_start_by_budget(messages, head_end)
        tail_start = self._align_backward(messages, tail_start, head_end)
        tail_start = self._ensure_last_user_in_tail(messages, tail_start, head_end)

        if tail_start <= head_end:
            return BoundaryResult(
                head_end=n, tail_start=n,
                head_messages=list(messages),
                middle_messages=[],
                tail_messages=[],
                no_compression_needed=True,
            )

        return BoundaryResult(
            head_end=head_end,
            tail_start=tail_start,
            head_messages=messages[:head_end],
            middle_messages=messages[head_end:tail_start],
            tail_messages=messages[tail_start:],
        )

    def _align_forward(self, messages: list[dict[str, Any]], idx: int) -> int:
        """Push head boundary forward past orphan tool results."""
        n = len(messages)
        while idx < n and messages[idx].get("role") == "tool":
            idx += 1
        return idx

    def _align_backward(
        self, messages: list[dict[str, Any]], idx: int, floor: int,
    ) -> int:
        """Pull tail boundary backward to keep tool_call/result groups together."""
        if idx <= floor or idx >= len(messages):
            return idx

        msg = messages[idx]
        if msg.get("role") == "tool":
            call_id = msg.get("tool_call_id", "")
            if call_id:
                owner = self._find_tool_call_owner(messages, call_id, floor, idx)
                if owner is not None and owner < idx:
                    idx = owner

        if idx > floor and idx < len(messages):
            prev = messages[idx - 1]
            if prev.get("role") == "assistant" and prev.get("tool_calls"):
                pass
            elif messages[idx].get("role") == "tool":
                for j in range(idx - 1, floor - 1, -1):
                    if messages[j].get("role") == "assistant" and messages[j].get("tool_calls"):
                        idx = j
                        break

        return max(idx, floor)

    def _ensure_last_user_in_tail(
        self, messages: list[dict[str, Any]], tail_start: int, floor: int,
    ) -> int:
        """Guarantee the most recent user message is in the tail segment."""
        last_user = None
        for i in range(len(messages) - 1, -1, -1):
            if messages[i].get("role") == "user":
                last_user = i
                break

        if last_user is not None and last_user < tail_start:
            tail_start = max(last_user, floor)

        return tail_start

    def _find_tail_start_by_budget(
        self, messages: list[dict[str, Any]], head_end: int,
    ) -> int:
        """Walk backward from end, accumulating tokens until budget is spent."""
        budget = self._tail_budget
        n = len(messages)
        tail_start = n

        for i in range(n - 1, head_end - 1, -1):
            cost = self._estimate([messages[i]])
            if budget - cost < 0 and tail_start < n:
                break
            budget -= cost
            tail_start = i

        return max(tail_start, head_end)

    def _find_tool_call_owner(
        self,
        messages: list[dict[str, Any]],
        call_id: str,
        start: int,
        end: int,
    ) -> int | None:
        """Find the assistant message that issued a given tool_call_id."""
        for i in range(end - 1, start - 1, -1):
            msg = messages[i]
            if msg.get("role") != "assistant":
                continue
            for tc in msg.get("tool_calls", []):
                if tc.get("id") == call_id:
                    return i
        return None
