"""Workflow store — JSON-based persistence for orchestrator state."""

from __future__ import annotations

import json
import os
import tempfile
from pathlib import Path
from typing import Any

from loguru import logger

from echo_agent.orchestrator.models import (
    AssignmentRecord,
    ExecutionLogRecord,
    ProviderHealthState,
    TaskRecord,
    WorkflowRecord,
)


class WorkflowStore:
    """Persists workflow records and provider health state."""

    def __init__(self, store_dir: Path):
        self._dir = store_dir
        self._dir.mkdir(parents=True, exist_ok=True)
        self._workflows_dir = self._dir / "workflows"
        self._workflows_dir.mkdir(exist_ok=True)
        self._health_file = self._dir / "provider_health.json"

    def save_workflow(
        self,
        workflow: WorkflowRecord,
        tasks: list[TaskRecord],
        assignments: list[AssignmentRecord],
    ) -> None:
        data = {
            "workflow": workflow.to_dict(),
            "tasks": [t.to_dict() for t in tasks],
            "assignments": [a.to_dict() for a in assignments],
        }
        path = self._workflows_dir / f"{workflow.id}.json"
        self._atomic_write(path, json.dumps(data, ensure_ascii=False, indent=2))

    def load_workflow(self, workflow_id: str) -> tuple[WorkflowRecord, list[TaskRecord], list[AssignmentRecord]] | None:
        path = self._workflows_dir / f"{workflow_id}.json"
        if not path.exists():
            return None
        try:
            data = json.loads(path.read_text(encoding="utf-8"))
            wf = WorkflowRecord.from_dict(data["workflow"])
            tasks = [TaskRecord.from_dict(t) for t in data.get("tasks", [])]
            assignments = [AssignmentRecord.from_dict(a) for a in data.get("assignments", [])]
            return wf, tasks, assignments
        except Exception as e:
            logger.error("Failed to load workflow {}: {}", workflow_id, e)
            return None

    def append_log(
        self,
        workflow: WorkflowRecord,
        tasks: list[TaskRecord],
        assignments: list[AssignmentRecord],
        log: ExecutionLogRecord,
    ) -> None:
        workflow.execution_logs.append(log.to_dict())
        self.save_workflow(workflow, tasks, assignments)

    def save_health(self, health: dict[str, ProviderHealthState]) -> None:
        data = {k: v.to_dict() for k, v in health.items()}
        self._atomic_write(self._health_file, json.dumps(data, ensure_ascii=False, indent=2))

    def load_health(self) -> dict[str, ProviderHealthState]:
        if not self._health_file.exists():
            return {}
        try:
            data = json.loads(self._health_file.read_text(encoding="utf-8"))
            return {k: ProviderHealthState.from_dict(v) for k, v in data.items()}
        except Exception as e:
            logger.warning("Failed to load health state: {}", e)
            return {}

    def recover_incomplete(self) -> list[str]:
        incomplete = []
        for f in self._workflows_dir.glob("*.json"):
            try:
                data = json.loads(f.read_text(encoding="utf-8"))
                status = data.get("workflow", {}).get("status", "")
                if status == "running":
                    incomplete.append(data["workflow"]["id"])
            except Exception:
                pass
        return incomplete

    @staticmethod
    def _atomic_write(path: Path, content: str) -> None:
        fd, tmp = tempfile.mkstemp(dir=path.parent, suffix=".tmp")
        try:
            with os.fdopen(fd, "w", encoding="utf-8") as f:
                f.write(content)
            os.replace(tmp, path)
        except BaseException:
            try:
                os.unlink(tmp)
            except OSError:
                pass
            raise
