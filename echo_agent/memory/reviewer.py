"""Background memory reviewer — auto-extracts memories from conversations.

After a non-trivial conversation, reviews the exchange and decides whether
user preferences, project facts, or lessons learned should be persisted.
"""

from __future__ import annotations

from typing import Any

from loguru import logger

from echo_agent.memory.service import ActorContext, MemoryService
from echo_agent.memory.types import MemoryEntry, MemoryType
from echo_agent.models.provider import LLMProvider

_REVIEW_PROMPT = """\
Review the conversation above and decide if any information should be saved to memory.

Focus on:
- User preferences, habits, or communication style (save as "user" type)
- Project facts, conventions, tool configurations, or domain knowledge (save as "environment" type)
- Lessons learned from debugging or problem-solving

Guidelines:
- Only save information that would be useful in future conversations.
- Do NOT save trivial or one-off details.
- User names, identity facts, and personal preferences belong only to the current session/user.
  Do not turn a name from one chat into a global default form of address.
- If a memory with the same key already exists, use "replace" to update it.
- Keep memory content concise and factual.

If nothing is worth saving, respond with "No memory changes needed." and stop."""

_MAX_REVIEW_ITERATIONS = 6

_TOOL_DEFS = [
    {
        "type": "function",
        "function": {
            "name": "memory_manage",
            "description": "Add, replace, or remove a persistent memory entry.",
            "parameters": {
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["add", "replace", "remove"]},
                    "target": {"type": "string", "enum": ["user", "environment"]},
                    "key": {"type": "string", "description": "Short label for the entry"},
                    "content": {"type": "string", "description": "Memory content"},
                    "old_text": {"type": "string", "description": "Substring to match for replace/remove"},
                    "importance": {"type": "number", "description": "0.0-1.0 importance score"},
                },
                "required": ["action", "target"],
            },
        },
    }
]


class MemoryReviewer:
    """Reviews conversations and auto-extracts memories."""

    def __init__(self, provider: LLMProvider, service: MemoryService, model: str = "", session_key: str = ""):
        # 统一写入口:provenance/ENV 门禁/失效/审计全部收敛在 service 的八步写序。
        # reviewer 只负责参数解析、读操作(find/resolve)与 WriteResult→结果映射。
        self._provider = provider
        self._service = service
        self._store = service.store
        self._model = model
        self._session_key = session_key

    def _resolve_entry(self, key: str, old_text: str, mem_type: MemoryType) -> tuple[MemoryEntry | None, str | None]:
        if key:
            entry = self._store.find_by_key(key, mem_type, session_key=self._session_key)
            if entry:
                return entry, None
        if old_text:
            matches = self._store.find_by_content_matches(
                old_text,
                mem_type=mem_type,
                limit=6,
                session_key=self._session_key,
            )
            if not matches:
                return None, None
            if len(matches) > 1:
                previews = ", ".join(entry.key or entry.content[:30] for entry in matches[:5])
                return None, f"Error: multiple matching memories found ({previews})"
            return matches[0], None
        return None, None

    async def review(self, conversation: list[dict[str, Any]]) -> list[str]:
        """Run a background review. Returns list of action summaries."""
        actions: list[str] = []

        messages = list(conversation)
        messages.append({"role": "user", "content": _REVIEW_PROMPT})

        for _ in range(_MAX_REVIEW_ITERATIONS):
            try:
                response = await self._provider.chat_with_retry(
                    messages=messages,
                    tools=_TOOL_DEFS,
                    model=self._model or None,
                )
            except Exception as e:
                logger.warning("Memory review LLM call failed: {}", e)
                break

            if response.content:
                messages.append({"role": "assistant", "content": response.content})

            if not response.has_tool_calls:
                break

            assistant_msg: dict[str, Any] = {"role": "assistant", "content": response.content or ""}
            assistant_msg["tool_calls"] = [tc.to_openai_format() for tc in response.tool_calls]
            if response.content:
                messages.pop()
            messages.append(assistant_msg)

            for tc in response.tool_calls:
                result = await self._execute(tc.arguments)
                messages.append({"role": "tool", "tool_call_id": tc.id, "name": tc.name, "content": result})
                if not result.startswith("Error"):
                    actions.append(f"memory: {result}")

        if actions:
            logger.info("Memory review completed with {} action(s)", len(actions))

        return actions

    def _actor_ctx(self) -> ActorContext:
        # reviewer 恒 model_inferred;USER 写以 session_key 为 scope,ENV 写由 service 门禁拦。
        return ActorContext(
            actor="reviewer",
            session_key=self._session_key,
            memory_scope=self._session_key,
        )

    async def _execute(self, params: dict[str, Any]) -> str:
        action = params.get("action", "")
        target = params.get("target", "user")
        mem_type = MemoryType.USER if target == "user" else MemoryType.ENVIRONMENT
        key = params.get("key", "")
        content = params.get("content", "")
        old_text = params.get("old_text", "")
        try:
            importance = min(1.0, max(0.0, float(params.get("importance", 0.5))))
        except (TypeError, ValueError):
            importance = 0.5

        if action == "add":
            if not key or not content:
                return "Error: key and content required"
            # 决策1:不额外前置 provenance 判定,直接走 service.add;
            # 同 key 合并语义由 store 内部保持,失效由 service 统一触发。
            res = await self._service.add(
                self._actor_ctx(),
                type=mem_type,
                key=key,
                content=content,
                importance=importance,
                source="model_inferred",
            )
            if not res.ok:
                return self._map_reject(res, target, "add", key)
            return f"Added [{target}] {res.entry.key}"

        elif action == "replace":
            if not content:
                return "Error: content required"
            entry, resolve_error = self._resolve_entry(key, old_text, mem_type)
            if resolve_error:
                return resolve_error
            if not entry:
                res = await self._service.add(
                    self._actor_ctx(),
                    type=mem_type,
                    key=key or "auto",
                    content=content,
                    importance=importance,
                    source="model_inferred",
                )
                if not res.ok:
                    return self._map_reject(res, target, "replace", key or "auto")
                return f"Added (new) [{target}] {res.entry.key}"
            # provenance 守卫(移入 service):低于目标优先级则拒,只拒不写 contradiction。
            res = await self._service.replace(
                self._actor_ctx(), entry.id, content=content, source="model_inferred",
            )
            if not res.ok:
                return self._map_reject(res, target, "replace", entry.key)
            return f"Updated [{target}] {entry.key}"

        elif action == "remove":
            entry, resolve_error = self._resolve_entry(key, old_text, mem_type)
            if resolve_error:
                return resolve_error
            if not entry:
                return "Error: no matching memory found"
            # provenance 守卫(据 reviewer actor 派生的 model_inferred)由 service.remove 强制。
            res = await self._service.remove(self._actor_ctx(), entry.id)
            if not res.ok:
                return self._map_reject(res, target, "remove", entry.key)
            return f"Removed [{target}] {entry.key}"

        return f"Error: unknown action '{action}'"

    @staticmethod
    def _map_reject(res: Any, target: str, op: str, key: str) -> str:
        """WriteResult 拒绝原因 → reviewer 结果文案(非 Error 前缀者不计入 actions 由 review 判定)。"""
        reason = res.reason
        if reason == "rejected_provenance":
            # 保住高优先级旧内容;被拒仅拒绝,不谎称已写入。
            logger.info("Reviewer {} denied (higher provenance): {}", op, key)
            return f"Kept existing (higher provenance) [{target}] {key}"
        if reason == "rejected_env":
            return (
                f"Error: writing ENVIRONMENT or global-tagged memory is disabled "
                f"[{target}] {key}"
            )
        if reason == "rejected_scope":
            return f"Error: cannot write memory without a resolved scope [{target}] {key}"
        return f"Error: memory {op} failed for '{key}' ({reason})"
