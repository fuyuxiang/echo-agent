"""Phase 1 — Tool output pruning.

Replaces old tool results outside the protected tail with informative
single-line summaries. This is a cheap pre-pass that doesn't call the LLM.
"""

from __future__ import annotations

from typing import Any, Callable

from echo_agent.agent.compression.types import PruneResult

_ARG_TRUNCATE_THRESHOLD = 1500
_ARG_TRUNCATE_TO = 1200


class ToolOutputPruner:

    def __init__(
        self,
        tail_budget_ratio: float,
        context_window_tokens: int,
        token_estimator: Callable[[list[dict[str, Any]]], int],
    ):
        self._tail_budget = int(context_window_tokens * tail_budget_ratio)
        self._estimate = token_estimator

    def prune(self, messages: list[dict[str, Any]]) -> PruneResult:
        if not messages:
            return PruneResult(messages=list(messages), pruned_count=0)

        protected = self._find_protected_tail(messages)
        seen_tool_sigs: dict[str, int] = {}
        pruned_count = 0
        result: list[dict[str, Any]] = []

        for i, msg in enumerate(messages):
            if i in protected:
                result.append(msg)
                if msg.get("role") == "tool":
                    sig = self._tool_signature(msg)
                    seen_tool_sigs[sig] = i
                continue

            if msg.get("role") == "tool":
                sig = self._tool_signature(msg)
                if sig in seen_tool_sigs and seen_tool_sigs[sig] > i:
                    result.append(self._make_dedup_placeholder(msg))
                    pruned_count += 1
                else:
                    result.append(self._prune_tool_result(msg))
                    pruned_count += 1
                    seen_tool_sigs[sig] = i
            elif msg.get("role") == "assistant" and msg.get("tool_calls"):
                result.append(self._truncate_tool_call_args(msg))
            else:
                result.append(msg)

        return PruneResult(messages=result, pruned_count=pruned_count)

    def _find_protected_tail(self, messages: list[dict[str, Any]]) -> set[int]:
        protected: set[int] = set()
        budget = self._tail_budget
        for i in range(len(messages) - 1, -1, -1):
            msg = messages[i]
            tokens = self._estimate([msg])
            if budget - tokens < 0 and protected:
                break
            budget -= tokens
            protected.add(i)
        return protected

    def _prune_tool_result(self, msg: dict[str, Any]) -> dict[str, Any]:
        name = msg.get("name", "tool")
        content = msg.get("content", "")
        summary = self._build_summary(name, content)
        pruned = dict(msg)
        pruned["content"] = summary
        return pruned

    def _build_summary(self, name: str, content: str) -> str:
        if not content:
            return f"[pruned] {name}: (empty result)"

        first_line = content.split("\n", 1)[0].strip()
        line_count = content.count("\n") + 1
        char_count = len(content)

        if name in ("terminal", "shell", "exec"):
            exit_code = ""
            for line in content.split("\n"):
                if "exit code" in line.lower() or "exitcode" in line.lower():
                    exit_code = line.strip()
                    break
            suffix = f", {exit_code}" if exit_code else ""
            return f"[pruned] {name}: ran command → {line_count} lines output{suffix}"

        if name in ("read_file", "readfile"):
            return f"[pruned] {name}: read file ({char_count} chars, {line_count} lines)"

        if name in ("search_files", "searchfiles", "grep"):
            match_count = content.count("\n")
            return f"[pruned] {name}: search → {match_count} matches"

        if name in ("write_file", "writefile", "edit_file", "editfile", "patch"):
            return f"[pruned] {name}: file operation ({char_count} chars written)"

        if name in ("web_fetch", "webfetch", "web_search", "websearch"):
            return f"[pruned] {name}: web result ({char_count} chars)"

        truncated = first_line[:100] + "..." if len(first_line) > 100 else first_line
        return f"[pruned] {name}: {truncated} ({line_count} lines)"

    def _make_dedup_placeholder(self, msg: dict[str, Any]) -> dict[str, Any]:
        name = msg.get("name", "tool")
        pruned = dict(msg)
        pruned["content"] = f"[pruned] {name}: (duplicate result, see latest)"
        return pruned

    def _tool_signature(self, msg: dict[str, Any]) -> str:
        name = msg.get("name", "")
        call_id = msg.get("tool_call_id", "")
        return f"{name}:{call_id}"

    def _truncate_tool_call_args(self, msg: dict[str, Any]) -> dict[str, Any]:
        tool_calls = msg.get("tool_calls", [])
        if not tool_calls:
            return msg

        modified = False
        new_calls = []
        for tc in tool_calls:
            func = tc.get("function", {})
            args = func.get("arguments", "")
            if isinstance(args, str) and len(args) > _ARG_TRUNCATE_THRESHOLD:
                new_func = dict(func)
                new_func["arguments"] = args[:_ARG_TRUNCATE_TO] + '..."}'
                new_tc = dict(tc)
                new_tc["function"] = new_func
                new_calls.append(new_tc)
                modified = True
            else:
                new_calls.append(tc)

        if not modified:
            return msg

        result = dict(msg)
        result["tool_calls"] = new_calls
        return result
