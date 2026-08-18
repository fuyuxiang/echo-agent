"""read_spill 工具 —— spill 产物的唯一取回通道。

为什么不复用 read_file:
  1. 授权。read_file 只认路径,不认会话。产物路径是模型上下文里的一个字符串,
     会话 A 把它复述给会话 B 就等于交出了内容。取回必须以 ctx.session_key 为
     授权凭据,而不是以"模型说得出这个路径"为凭据。
  2. 寻址。read_file 按行分页,而 spill 最常见的大输出恰恰是单行 JSON、压缩后
     的日志——整段只有一行,分页无从下手,尾部结论永远读不到。这里按字符寻址。
"""

from __future__ import annotations

import re
from pathlib import Path
from typing import Any

from echo_agent.agent.tools.base import Tool, ToolExecutionContext, ToolResult
from echo_agent.spill.layout import is_artifact

# 单次返回的字符上限。默认值与 spill.maxInlineChars 无关:这里返回的是模型
# 主动要的内容,不该被"预览"的尺度约束,但仍需一个上限防止一次拉回 2 MB。
_DEFAULT_LIMIT = 4000
_MAX_LIMIT = 20000
# 产物内检索每条命中的上下文字符数,以及命中条数上限。
_MATCH_CONTEXT = 120
_MAX_MATCHES = 50


class ReadSpillTool(Tool):
    name = "read_spill"
    description = (
        "Retrieve the full content of a spilled tool-output artifact, using the path "
        "given in a truncation notice. Reads by character offset (not lines), so "
        "single-line output such as minified JSON is fully reachable. "
        "Set 'pattern' to search within the artifact instead of paging through it."
    )
    risk_level = "read_only"
    parameters = {
        "type": "object",
        "properties": {
            "path": {"type": "string", "description": "Artifact path from the truncation notice."},
            "offset": {"type": "integer", "description": "Start character offset (0-based)."},
            "limit": {"type": "integer", "description": f"Max characters to return (default {_DEFAULT_LIMIT})."},
            "pattern": {"type": "string", "description": "Optional regex; returns matching excerpts instead of a slice."},
        },
        "required": ["path"],
    }

    def __init__(self, spill_root: Path | None = None):
        self._spill_root = Path(spill_root).resolve() if spill_root else None

    def is_ready(self) -> bool:
        return self._spill_root is not None

    def readiness_detail(self) -> tuple[bool, str]:
        if self._spill_root is None:
            return False, "spill storage is not configured"
        return True, "ok"

    def _authorize(self, path: str, ctx: ToolExecutionContext | None) -> tuple[Path | None, str]:
        """解析并校验路径,返回 (产物路径, 错误信息)。

        授权边界是"本会话的产物目录",由 SpillStore 派生同一个目录名,故与写侧
        恒定一致。校验用解析后的路径比对父目录,不用字符串前缀——``..`` 和
        符号链接都能骗过前缀匹配。
        """
        if self._spill_root is None:
            return None, "spill storage is not configured"
        session_key = (ctx.session_key if ctx else "") or "unscoped"

        from echo_agent.spill.store import SpillStore
        session_dir = SpillStore(self._spill_root).session_dir(session_key)
        try:
            resolved = Path(path).expanduser()
            resolved = resolved.resolve() if resolved.is_absolute() else (session_dir / resolved).resolve()
        except (OSError, ValueError) as e:
            return None, f"Invalid path: {e}"

        # 目录必须严格相等,不是"在 spill 根之下"。后者会放行同实例其他会话的
        # 产物,而那正是要防的越权。
        if resolved.parent != session_dir.resolve() or not is_artifact(resolved):
            return None, (
                "Not a spill artifact belonging to this session. read_spill only "
                "retrieves artifacts produced by this session's own tool calls."
            )
        if not resolved.exists():
            from echo_agent.spill.expired import EXPIRED_NOTICE
            return None, EXPIRED_NOTICE
        return resolved, ""

    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        target, error = self._authorize(params["path"], ctx)
        if target is None:
            return ToolResult(success=False, error=error, error_kind="business")

        import asyncio
        try:
            text = await asyncio.to_thread(
                target.read_text, encoding="utf-8", errors="replace",
            )
        except OSError as e:
            return ToolResult(success=False, error=f"Cannot read artifact: {e}")

        pattern = params.get("pattern")
        if pattern:
            return self._search(text, pattern)
        return self._slice(text, params)

    def _slice(self, text: str, params: dict[str, Any]) -> ToolResult:
        total = len(text)
        offset = max(0, params.get("offset", 0))
        limit = min(max(1, params.get("limit", _DEFAULT_LIMIT)), _MAX_LIMIT)
        chunk = text[offset:offset + limit]
        end = offset + len(chunk)
        meta = {"total_chars": total, "offset": offset, "returned_chars": len(chunk)}
        if not chunk:
            return ToolResult(
                output=f"(offset {offset} is past end of artifact; total {total} chars)",
                metadata=meta,
            )
        if end < total:
            # 明确给出下一个 offset:让模型继续翻页时不必自己算。
            chunk += f"\n\n（本段到字符 {end}，共 {total}。继续读取请用 offset={end}。）"
            meta["next_offset"] = end
        return ToolResult(output=chunk, metadata=meta)

    def _search(self, text: str, pattern: str) -> ToolResult:
        try:
            regex = re.compile(pattern)
        except re.error as e:
            return ToolResult(success=False, error=f"Invalid regex: {e}", error_kind="validation")

        excerpts: list[str] = []
        for m in regex.finditer(text):
            start = max(0, m.start() - _MATCH_CONTEXT)
            end = min(len(text), m.end() + _MATCH_CONTEXT)
            excerpts.append(f"@{m.start()}: {text[start:end]}")
            if len(excerpts) >= _MAX_MATCHES:
                break
        if not excerpts:
            return ToolResult(output="No matches in artifact.", metadata={"count": 0, "total_chars": len(text)})
        meta: dict[str, Any] = {"count": len(excerpts), "total_chars": len(text)}
        if len(excerpts) >= _MAX_MATCHES:
            meta["truncated"] = True
        # 命中位置就是 offset,可直接喂回本工具读全文。
        return ToolResult(output="\n---\n".join(excerpts), metadata=meta)

    def execution_mode(self, params: dict[str, Any]) -> str:
        return "read_only"
