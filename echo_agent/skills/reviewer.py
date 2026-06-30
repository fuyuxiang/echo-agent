"""Background skill reviewer — auto-creates/updates skills after task completion.

Spawns a lightweight LLM call that reviews the conversation and decides whether
to capture a reusable skill. Runs in the background so it doesn't block the user.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any

from loguru import logger

from echo_agent.memory.store import scan_text_for_threats
from echo_agent.models.provider import LLMProvider
from echo_agent.skills.store import SkillStore

if TYPE_CHECKING:
    from echo_agent.skills.admission import SkillAdmission

_REVIEW_PROMPT = """\
Review the conversation above and consider saving or updating a skill if appropriate.

Focus on: was a non-trivial approach used to complete a task that required trial and error, \
changing course due to experiential findings, or domain-specific knowledge?

Guidelines:
- If a relevant skill already exists, update it with what you learned using the patch action.
- Otherwise, create a new skill if the approach is reusable across similar future tasks.
- Do NOT create skills for trivial or one-off tasks.
- Skills should capture the procedure, pitfalls, and verification steps — not just the final answer.
- Use YAML frontmatter with at least 'name' and 'description' fields.

If nothing is worth saving, simply respond with "No skill changes needed." and stop."""

_MAX_REVIEW_ITERATIONS = 8


class SkillReviewer:
    """Reviews conversations and auto-creates/updates skills."""

    def __init__(
        self,
        provider: LLMProvider,
        store: SkillStore,
        model: str = "",
        admission: "SkillAdmission | None" = None,
        session_key: str = "",
        channel: str = "",
    ):
        self._provider = provider
        self._store = store
        self._model = model
        self._admission = admission
        self._session_key = session_key
        self._channel = channel

    async def review(self, conversation: list[dict[str, Any]]) -> list[str]:
        """Run a background review. Returns list of action summaries."""
        actions: list[str] = []
        tool_defs = self._build_tool_defs()

        messages = list(conversation)
        messages.append({"role": "user", "content": _REVIEW_PROMPT})

        for _ in range(_MAX_REVIEW_ITERATIONS):
            try:
                response = await self._provider.chat_with_retry(
                    messages=messages,
                    tools=tool_defs,
                    model=self._model or None,
                )
            except Exception as e:
                logger.warning("Skill review LLM call failed: {}", e)
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
                result = await self._execute_tool(tc.name, tc.arguments)
                messages.append({"role": "tool", "tool_call_id": tc.id, "name": tc.name, "content": result})
                if not result.startswith("Error"):
                    actions.append(f"{tc.name}: {result}")

        if actions:
            logger.info("Skill review completed with {} action(s)", len(actions))
        return actions

    async def _execute_tool(self, name: str, params: dict[str, Any]) -> str:
        """Execute a skill management tool call."""
        if name == "skill_manage":
            return await self._handle_skill_manage(params)
        return f"Error: unknown tool '{name}'"

    # action → (operation, risk) 映射;edit 语义等同 patch(改已存在技能)
    _ACTION_TO_OP = {
        "create": ("create", "high"),
        "edit": ("patch", "low"),
        "patch": ("patch", "low"),
        "delete": ("delete", "high"),
    }

    async def _route_via_admission(self, action: str, params: dict[str, Any]) -> str:
        from echo_agent.evolution.types import SkillCandidate

        op, risk = self._ACTION_TO_OP[action]
        skill_name = params.get("name", "")
        patch_old = params.get("old_text", "")
        patch_new = params.get("new_text", "")
        # edit 语义是「整篇替换」,tool schema 给的是 content 而非 old/new。映射成
        # patch 时必须基于当前 SKILL.md 生成明确的整篇 old→new patch,否则空 old/new
        # 会让 admission 落一个 no-op patch(空串匹配任意文本、replace 不改动),审批后
        # 内容根本不变。读不到当前内容时退回普通 patch 字段(保持旧行为不崩)。
        if action == "edit":
            current = self._store.read_skill(skill_name)
            if current is not None:
                patch_old = current
                patch_new = params.get("content", "")
        c = SkillCandidate(
            operation=op,
            skill_name=skill_name,
            source="reviewer",
            created_by="reviewer",
            created_from_session=self._session_key,
            channel=self._channel,
            risk=risk,
            proposed_content=params.get("content", ""),
            proposed_patch_old=patch_old,
            proposed_patch_new=patch_new,
            rationale="background skill review",
        )
        res = await self._admission.admit(c)
        return f"{res.outcome}: {res.message}"

    async def _handle_skill_manage(self, params: dict[str, Any]) -> str:
        action = params.get("action", "")
        skill_name = params.get("name", "")

        # 收编:已接 admission 时,技能正文类操作统一走准入治理层
        if self._admission is not None and action in self._ACTION_TO_OP:
            return await self._route_via_admission(action, params)

        # 治理层缺口封堵:admission 激活时,支持文件(scripts/assets/templates 等)
        # 也属于会被技能系统加载/执行的产物。背景 reviewer 读的是跨通道、可能含
        # 注入内容的对话,绝不能绕过候选/审批直接落盘可执行脚本。这类写入必须走
        # 显式/人工路径,故在此直接拒绝,且不留审计记录。
        if self._admission is not None and action in ("write_file", "remove_file"):
            logger.warning(
                "skill review blocked supporting-file op under admission: action={} name={}",
                action, skill_name,
            )
            return (
                f"Error: '{action}' is not allowed for the background reviewer "
                "(supporting files must go through an explicit path, not auto-admission)."
            )

        # Lightweight gate: scan any content that will land in the skill store
        # for prompt-injection/exfiltration before writing. A poisoned turn
        # must not auto-persist into SKILL.md. (trusted-operator model still
        # treats reviewer-written skills as a tool-boundary that needs vetting.)
        to_scan = " ".join(str(params.get(k, "")) for k in ("content", "new_text"))
        if to_scan.strip():
            threat = scan_text_for_threats(to_scan)
            if threat:
                logger.warning("skill review blocked: action={} name={} reason={}",
                               action, skill_name, threat)
                return f"Error: blocked by injection scan: {threat}"

        # Audit log fires only after a store call succeeds, with the real action
        # name — blocked, empty-content, and unknown-action turns must not leave
        # a "write" record in the audit trail.
        if action == "create":
            content = params.get("content", "")
            if not content:
                return "Error: content is required"
            err = self._store.create_skill(skill_name, content, category=params.get("category", ""))
            if err is None:
                logger.info("skill review applied: action=create name={}", skill_name)
            return err or f"Skill '{skill_name}' created."

        elif action == "edit":
            content = params.get("content", "")
            if not content:
                return "Error: content is required"
            err = self._store.update_skill(skill_name, content)
            if err is None:
                logger.info("skill review applied: action=edit name={}", skill_name)
            return err or f"Skill '{skill_name}' updated."

        elif action == "patch":
            old_text = params.get("old_text", "")
            new_text = params.get("new_text", "")
            if not old_text:
                return "Error: old_text is required"
            err = self._store.patch_skill(skill_name, old_text, new_text, file_path=params.get("file_path", ""))
            if err is None:
                logger.info("skill review applied: action=patch name={}", skill_name)
            return err or f"Skill '{skill_name}' patched."

        elif action == "delete":
            err = self._store.delete_skill(skill_name)
            if err is None:
                logger.info("skill review applied: action=delete name={}", skill_name)
            return err or f"Skill '{skill_name}' deleted."

        elif action == "write_file":
            file_path = params.get("file_path", "")
            content = params.get("content", "")
            if not file_path or not content:
                return "Error: file_path and content required"
            err = self._store.write_file(skill_name, file_path, content)
            if err is None:
                logger.info("skill review applied: action=write_file name={} file={}", skill_name, file_path)
            return err or f"File '{file_path}' written."

        elif action == "remove_file":
            file_path = params.get("file_path", "")
            if not file_path:
                return "Error: file_path required"
            err = self._store.remove_file(skill_name, file_path)
            if err is None:
                logger.info("skill review applied: action=remove_file name={} file={}", skill_name, file_path)
            return err or f"File '{file_path}' removed."

        return f"Error: unknown action '{action}'"

    @staticmethod
    def _build_tool_defs() -> list[dict[str, Any]]:
        return [
            {
                "type": "function",
                "function": {
                    "name": "skill_manage",
                    "description": "Create, edit, patch, or delete skills to capture reusable knowledge.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "action": {
                                "type": "string",
                                "enum": ["create", "edit", "patch", "delete", "write_file", "remove_file"],
                            },
                            "name": {"type": "string"},
                            "category": {"type": "string"},
                            "content": {"type": "string"},
                            "file_path": {"type": "string"},
                            "old_text": {"type": "string"},
                            "new_text": {"type": "string"},
                        },
                        "required": ["action", "name"],
                    },
                },
            }
        ]
