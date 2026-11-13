"""Find saved originals in the current conversation, including after a restart."""
from pathlib import Path
from typing import Any

from echo_agent.attachments import AttachmentStore
from echo_agent.tools import Tool, ToolExecutionContext, ToolResult


class AttachmentListTool(Tool):
    name = "attachment_list"
    description = "List saved original attachments in this conversation. Use their IDs with read_document, send_file or cronjob."
    risk_level = "read_only"
    parameters = {"type": "object", "properties": {"limit": {"type": "integer", "minimum": 1, "maximum": 50}}}

    def __init__(self, workspace: Path):
        self._store = AttachmentStore(workspace)

    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        if not ctx or not ctx.session_key:
            return ToolResult(success=False, error="Attachment access requires a conversation")
        records = self._store.recent(ctx.session_key, limit=max(1, min(int(params.get("limit", 10)), 50)))
        usage = self._store.usage(ctx.session_key)
        out = "\n".join(self._store.describe(r) for r in records) or "此会话没有已保存的附件。"
        out += (f"\n此会话共 {usage['count']} 份原件，{usage['size_bytes']} 字节。"
                f"未被提醒引用的原件保存 {usage['retention_days']} 天；提醒和待投递任务引用的原件保留至引用结束。"
                "可用 attachment_delete 删除不再需要的原件；删除原件不删除历史聊天和已提取正文。")
        return ToolResult(output=out, metadata={"attachments": records, "usage": usage})


class AttachmentDeleteTool(Tool):
    name = "attachment_delete"
    description = "Permanently delete a saved original in this conversation when the user requests it. Files still referenced by reminders cannot be deleted. Chat history/extracted text is unaffected."
    risk_level = "dangerous"
    parameters = {"type": "object", "properties": {"attachment_id": {"type": "string"}}, "required": ["attachment_id"]}

    def __init__(self, workspace: Path):
        self._store = AttachmentStore(workspace)

    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        if not ctx or not ctx.session_key:
            return ToolResult(success=False, error="Attachment access requires a conversation")
        try:
            record = self._store.get(ctx.session_key, params["attachment_id"])
            self._store.delete(ctx.session_key, record["id"])
        except (ValueError, OSError) as exc:
            return ToolResult(success=False, error=str(exc), error_kind="business")
        return ToolResult(output=f"已删除原件「{record['name']}」。历史聊天和已提取正文仍保留。")
