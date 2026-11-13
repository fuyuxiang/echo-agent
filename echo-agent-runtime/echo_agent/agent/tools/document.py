"""read_document tool — model-facing wrapper over the document_extract core."""
from __future__ import annotations

from pathlib import Path
from typing import Any

from echo_agent.agent.media.document_extract import extract
from echo_agent.tools import Tool, ToolExecutionContext, ToolResult
from echo_agent.security.path_policy import check_read, resolve_path


class ReadDocumentTool(Tool):
    name = "read_document"
    description = (
        "Read text content from a document file (pdf/docx/xlsx/pptx/txt/csv/md). "
        "Use after a user sends a document, or to read a specific page/sheet. "
        "Optional 'unit' selects a PDF/PPTX page number or an XLSX sheet name/index."
    )
    risk_level = "read_only"
    parameters = {
        "type": "object",
        "properties": {
            "path": {"type": "string", "description": "Local path to the document."},
            "attachment_id": {"type": "string", "description": "Saved attachment ID in the current conversation; use instead of path."},
            "unit": {
                "type": ["integer", "string"],
                "description": "Optional: PDF/PPTX page number, or XLSX sheet name/index. Omit for full doc.",
            },
            "max_chars": {"type": "integer", "description": "Optional cap on returned characters."},
        },
    }

    def __init__(self, workspace: str, restrict: bool = False, spill_root: Path | None = None):
        self._workspace = str(Path(workspace).resolve())
        self._restrict = restrict
        self._spill_root = spill_root

    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        path = params.get("path", "")
        if params.get("attachment_id"):
            from echo_agent.attachments import AttachmentStore
            store = AttachmentStore(Path(self._workspace))
            try:
                record = store.get(ctx.session_key if ctx else "", params["attachment_id"])
                path = str(store.path(record))
            except (ValueError, OSError) as exc:
                return ToolResult(success=False, error=str(exc))
        if not path:
            return ToolResult(success=False, error="Provide path or attachment_id")
        # spill 产物是 .txt,extract 认得它,故这里同样是一条会话无关的读取路径。
        violation = check_read(path, self._workspace, spill_root=self._spill_root)
        if violation:
            return ToolResult(success=False, error=violation)
        if self._restrict:
            resolved = resolve_path(path, self._workspace)
            try:
                resolved.relative_to(self._workspace)
            except ValueError:
                return ToolResult(success=False, error=f"Path {path} is outside workspace {self._workspace}")
        target = resolve_path(path, self._workspace)
        res = extract(target, max_chars=params.get("max_chars"), unit=params.get("unit"))
        if params.get("attachment_id"):
            store.update_extraction(ctx.session_key, params["attachment_id"],
                                    "ready" if res.text else "failed" if res.meta.get("error") else "no_text",
                                    str(res.meta.get("error") or ""))
        if not res.text:
            err = res.meta.get("error") or f"no extractable text ({res.meta.get('format')})"
            return ToolResult(success=False, error=err, metadata=res.meta)
        meta = dict(res.meta)
        meta["truncated"] = res.truncated
        meta["unit_count"] = res.unit_count
        return ToolResult(success=True, output=res.text, metadata=meta)
