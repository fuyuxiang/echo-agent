"""Search files tool — regex and glob search across workspace.

All filesystem traversal here runs on a worker thread via ``asyncio.to_thread``.
``rglob``/``os.walk`` + ``read_text`` are blocking syscalls; running them inline
on the single asyncio event loop freezes every other task on that loop (channel
poll loops, the HTTP healthz endpoint, other sessions). The traversal is also
bounded — max files scanned + a wall-clock soft budget — so a mistakenly broad
root (e.g. ``path="/"``) degrades to a truncated result instead of pinning a
worker thread for minutes.
"""

from __future__ import annotations

import asyncio
import os
import re
import time
from pathlib import Path
from typing import Any

from echo_agent.agent.tools.base import Tool, ToolExecutionContext, ToolResult
from echo_agent.security.path_policy import check_read, resolve_path

# Directories never worth descending into for a code/content search.
_SKIP_DIRS = frozenset({".git", "__pycache__", "node_modules", ".venv", "venv"})
# Hard ceilings so an over-broad root cannot run unbounded even off-loop.
_MAX_FILES_SCANNED = 20_000
_MAX_FILE_BYTES = 1_000_000
# Wall-clock soft budget for a single traversal (seconds). Kept below the
# tool's ``timeout_seconds`` so we return a clean truncated result rather than
# being killed by the registry's asyncio.wait_for.
_SOFT_BUDGET_SECONDS = 20.0


class SearchFilesTool(Tool):
    name = "search_files"
    description = "Search file contents by regex pattern or find files by glob pattern within the workspace."
    risk_level = "read_only"
    parameters = {
        "type": "object",
        "properties": {
            "pattern": {"type": "string", "description": "Regex pattern to search in file contents, or glob pattern for file names."},
            "path": {"type": "string", "description": "Subdirectory to search in (relative to workspace or absolute). Defaults to '.'."},
            "mode": {"type": "string", "enum": ["content", "glob"], "description": "Search mode: 'content' for regex in files, 'glob' for filename matching."},
            "max_results": {"type": "integer", "description": "Maximum results to return.", "default": 50},
        },
        "required": ["pattern"],
    }
    timeout_seconds = 30

    def __init__(self, workspace: str, restrict: bool = False):
        self._workspace = Path(workspace).resolve()
        self._restrict = restrict

    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        pattern = params["pattern"]
        mode = params.get("mode", "content")
        sub = params.get("path", ".")
        max_results = params.get("max_results", 50)

        search_root = resolve_path(sub, str(self._workspace))
        if self._restrict:
            try:
                search_root.relative_to(self._workspace)
            except ValueError:
                return ToolResult(success=False, error="Path outside workspace")
        violation = check_read(str(search_root), str(self._workspace))
        if violation:
            return ToolResult(success=False, error=violation)

        if not search_root.is_dir():
            return ToolResult(success=False, error=f"Directory not found: {sub}")

        # Offload the blocking traversal to a worker thread so the event loop
        # (and thus channel polling / healthz / other sessions) stays live.
        if mode == "glob":
            return await asyncio.to_thread(self._glob_search, search_root, pattern, max_results)
        return await asyncio.to_thread(self._content_search, search_root, pattern, max_results)

    def _rel(self, path: Path) -> str:
        try:
            return str(path.relative_to(self._workspace))
        except ValueError:
            return str(path)

    def _walk_files(self, root: Path):
        """Yield files under *root*, pruning skip dirs before descending.

        Uses os.walk so skipped directories are never entered (unlike
        rglob("*"), which materialises every entry then filters). Prunes
        in-place via the dirnames slice so large vendored trees are skipped
        at the top rather than walked.
        """
        for dirpath, dirnames, filenames in os.walk(root, followlinks=False):
            dirnames[:] = [d for d in dirnames if d not in _SKIP_DIRS]
            base = Path(dirpath)
            for name in filenames:
                yield base / name

    def _glob_search(self, root: Path, pattern: str, limit: int) -> ToolResult:
        matches: list[str] = []
        scanned = 0
        deadline = time.monotonic() + _SOFT_BUDGET_SECONDS
        truncated = False
        for p in self._walk_files(root):
            scanned += 1
            if scanned > _MAX_FILES_SCANNED or time.monotonic() > deadline:
                truncated = True
                break
            if p.match(pattern):
                matches.append(self._rel(p))
                if len(matches) >= limit:
                    truncated = True
                    break
        meta = {"count": len(matches), "scanned": scanned}
        if truncated:
            meta["truncated"] = True
        return ToolResult(output="\n".join(matches) if matches else "No files matched.", metadata=meta)

    def _content_search(self, root: Path, pattern: str, limit: int) -> ToolResult:
        try:
            regex = re.compile(pattern)
        except re.error as e:
            return ToolResult(success=False, error=f"Invalid regex: {e}")

        results: list[str] = []
        scanned = 0
        deadline = time.monotonic() + _SOFT_BUDGET_SECONDS
        truncated = False

        for path in self._walk_files(root):
            scanned += 1
            if scanned > _MAX_FILES_SCANNED or time.monotonic() > deadline:
                truncated = True
                break
            try:
                if not path.is_file() or path.stat().st_size > _MAX_FILE_BYTES:
                    continue
                text = path.read_text(errors="replace")
            except (OSError, PermissionError):
                continue
            for i, line in enumerate(text.splitlines(), 1):
                if regex.search(line):
                    results.append(f"{self._rel(path)}:{i}: {line.rstrip()[:200]}")
                    if len(results) >= limit:
                        truncated = True
                        break
            if truncated:
                break

        meta = {"count": len(results), "scanned": scanned}
        if truncated:
            meta["truncated"] = True
        return ToolResult(output="\n".join(results) if results else "No matches found.", metadata=meta)

    def execution_mode(self, params: dict[str, Any]) -> str:
        return "read_only"
