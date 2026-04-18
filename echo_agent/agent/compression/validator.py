"""Phase 5 — Post-compression message validation.

Ensures structural integrity of the compressed message list:
- Every tool_result has a matching tool_call
- Every tool_call has a matching tool_result
- Messages don't start with orphan tool results
- tool_call/result counts are consistent
"""

from __future__ import annotations

from typing import Any

from loguru import logger


class MessageValidator:

    def validate(self, messages: list[dict[str, Any]]) -> list[dict[str, Any]]:
        messages = self._remove_leading_tool_results(messages)
        messages = self._remove_orphan_tool_results(messages)
        messages = self._patch_missing_tool_results(messages)
        return messages

    def _remove_leading_tool_results(
        self, messages: list[dict[str, Any]],
    ) -> list[dict[str, Any]]:
        start = 0
        for i, msg in enumerate(messages):
            if msg.get("role") != "tool":
                start = i
                break
        else:
            return []

        if start > 0:
            logger.debug("Removed {} leading orphan tool results", start)
        return messages[start:]

    def _remove_orphan_tool_results(
        self, messages: list[dict[str, Any]],
    ) -> list[dict[str, Any]]:
        known_call_ids: set[str] = set()
        for msg in messages:
            for tc in msg.get("tool_calls", []):
                known_call_ids.add(tc.get("id", ""))

        result: list[dict[str, Any]] = []
        removed = 0
        for msg in messages:
            if msg.get("role") == "tool":
                call_id = msg.get("tool_call_id", "")
                if call_id and call_id not in known_call_ids:
                    removed += 1
                    continue
            result.append(msg)

        if removed:
            logger.debug("Removed {} orphan tool results", removed)
        return result

    def _patch_missing_tool_results(
        self, messages: list[dict[str, Any]],
    ) -> list[dict[str, Any]]:
        result: list[dict[str, Any]] = []

        for i, msg in enumerate(messages):
            result.append(msg)

            if msg.get("role") != "assistant" or not msg.get("tool_calls"):
                continue

            existing_ids: set[str] = set()
            for j in range(i + 1, len(messages)):
                following = messages[j]
                if following.get("role") == "tool":
                    existing_ids.add(following.get("tool_call_id", ""))
                else:
                    break

            for tc in msg.get("tool_calls", []):
                call_id = tc.get("id", "")
                if call_id and call_id not in existing_ids:
                    func_name = tc.get("function", {}).get("name", "unknown")
                    result.append({
                        "role": "tool",
                        "tool_call_id": call_id,
                        "name": func_name,
                        "content": "[result lost during context compression]",
                    })
                    logger.debug(
                        "Patched missing tool result for {}:{}",
                        func_name, call_id,
                    )

        return result
