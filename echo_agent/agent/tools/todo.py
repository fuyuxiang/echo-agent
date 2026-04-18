"""Todo tool — task planning and tracking per session."""

from __future__ import annotations

import json
import time
from pathlib import Path
from typing import Any

from echo_agent.agent.tools.base import Tool, ToolExecutionContext, ToolPermission, ToolResult


class TodoTool(Tool):
    name = "todo"
    description = "Manage a task list: create, update, list, or complete tasks for planning multi-step work."
    parameters = {
        "type": "object",
        "properties": {
            "action": {"type": "string", "enum": ["create", "update", "list", "complete", "delete"], "description": "Action to perform."},
            "title": {"type": "string", "description": "Task title (for create/update)."},
            "task_id": {"type": "string", "description": "Task ID (for update/complete/delete)."},
            "status": {"type": "string", "enum": ["pending", "in_progress", "done"], "description": "New status (for update)."},
            "notes": {"type": "string", "description": "Additional notes."},
        },
        "required": ["action"],
    }
    required_permissions = [ToolPermission.WRITE]

    def __init__(self, store_dir: Path):
        self._store_dir = store_dir
        self._store_dir.mkdir(parents=True, exist_ok=True)

    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        action = params["action"]
        tasks = self._load()

        if action == "create":
            title = params.get("title", "Untitled")
            task_id = f"t_{int(time.time() * 1000) % 100000}"
            tasks[task_id] = {"title": title, "status": "pending", "notes": params.get("notes", ""), "created": time.time()}
            self._save(tasks)
            return ToolResult(output=f"Created task {task_id}: {title}")

        if action == "list":
            if not tasks:
                return ToolResult(output="No tasks.")
            lines = []
            for tid, t in tasks.items():
                lines.append(f"[{t['status']}] {tid}: {t['title']}" + (f" — {t['notes']}" if t.get("notes") else ""))
            return ToolResult(output="\n".join(lines))

        task_id = params.get("task_id", "")
        if task_id not in tasks:
            return ToolResult(success=False, error=f"Task '{task_id}' not found")

        if action == "update":
            if "title" in params:
                tasks[task_id]["title"] = params["title"]
            if "status" in params:
                tasks[task_id]["status"] = params["status"]
            if "notes" in params:
                tasks[task_id]["notes"] = params["notes"]
            self._save(tasks)
            return ToolResult(output=f"Updated {task_id}")

        if action == "complete":
            tasks[task_id]["status"] = "done"
            self._save(tasks)
            return ToolResult(output=f"Completed {task_id}: {tasks[task_id]['title']}")

        if action == "delete":
            title = tasks.pop(task_id)["title"]
            self._save(tasks)
            return ToolResult(output=f"Deleted {task_id}: {title}")

        return ToolResult(success=False, error=f"Unknown action: {action}")

    def _load(self) -> dict[str, Any]:
        path = self._store_dir / "todos.json"
        if path.exists():
            return json.loads(path.read_text())
        return {}

    def _save(self, tasks: dict[str, Any]) -> None:
        path = self._store_dir / "todos.json"
        path.write_text(json.dumps(tasks, indent=2))
