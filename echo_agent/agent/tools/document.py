"""read_document tool — model-facing wrapper over the document_extract core."""
from __future__ import annotations

from pathlib import Path
from typing import Any

from echo_agent.agent.media.document_extract import extract
from echo_agent.agent.tools.base import Tool, ToolExecutionContext, ToolResult
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
            "unit": {
                "type": ["integer", "string"],
                "description": "Optional: PDF/PPTX page number, or XLSX sheet name/index. Omit for full doc.",
            },
            "max_chars": {"type": "integer", "description": "Optional cap on returned characters."},
        },
        "required": ["path"],
    }

    def __init__(self, workspace: str, restrict: bool = False):
        self._workspace = str(Path(workspace).resolve())
        self._restrict = restrict

    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        path = params["path"]
        violation = check_read(path, self._workspace)
        if violation:
            return ToolResult(success=False, error=violation)
        if self._restrict:
            resolved = resolve_path(path, self._workspace)
            try:
                resolved.relative_to(self._workspace)
            except ValueError:
                return ToolResult(success=False, error=f"Path {path} is outside workspace {self._workspace}")
        res = extract(path, max_chars=params.get("max_chars"), unit=params.get("unit"))
        if not res.text:
            err = res.meta.get("error") or f"no extractable text ({res.meta.get('format')})"
            return ToolResult(success=False, error=err, metadata=res.meta)
        meta = dict(res.meta)
        meta["truncated"] = res.truncated
        meta["unit_count"] = res.unit_count
        return ToolResult(success=True, output=res.text, metadata=meta)
