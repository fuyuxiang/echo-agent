"""Skill system — installable, configurable, versioned capability modules.

Also includes experience store for learning from past executions.
"""

from __future__ import annotations

import json
import shutil
from dataclasses import dataclass, field
from datetime import datetime
from enum import Enum
from pathlib import Path
from typing import Any

from loguru import logger


class SkillStatus(str, Enum):
    INSTALLED = "installed"
    ENABLED = "enabled"
    DISABLED = "disabled"
    ERROR = "error"


@dataclass
class SkillManifest:
    name: str = ""
    version: str = "0.1.0"
    description: str = ""
    author: str = ""
    dependencies: list[str] = field(default_factory=list)
    config_schema: dict[str, Any] = field(default_factory=dict)
    scope: str = "global"  # global | workspace | session
    entry_file: str = "SKILL.md"

    def to_dict(self) -> dict[str, Any]:
        return {
            "name": self.name, "version": self.version,
            "description": self.description, "author": self.author,
            "dependencies": self.dependencies,
            "config_schema": self.config_schema,
            "scope": self.scope, "entry_file": self.entry_file,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> SkillManifest:
        return cls(
            name=data.get("name", ""), version=data.get("version", "0.1.0"),
            description=data.get("description", ""), author=data.get("author", ""),
            dependencies=data.get("dependencies", []),
            config_schema=data.get("config_schema", {}),
            scope=data.get("scope", "global"),
            entry_file=data.get("entry_file", "SKILL.md"),
        )


@dataclass
class InstalledSkill:
    manifest: SkillManifest
    status: SkillStatus = SkillStatus.INSTALLED
    config: dict[str, Any] = field(default_factory=dict)
    installed_at: str = field(default_factory=lambda: datetime.now().isoformat())
    path: str = ""
    previous_versions: list[str] = field(default_factory=list)


class SkillManager:
    """Manages skill lifecycle: install, enable, disable, configure, upgrade, rollback."""

    def __init__(self, skills_dir: Path):
        self._skills_dir = skills_dir
        self._skills_dir.mkdir(parents=True, exist_ok=True)
        self._skills: dict[str, InstalledSkill] = {}
        self._load_installed()

    def _load_installed(self) -> None:
        for skill_dir in self._skills_dir.iterdir():
            if not skill_dir.is_dir():
                continue
            manifest_path = skill_dir / "manifest.json"
            if not manifest_path.exists():
                continue
            try:
                data = json.loads(manifest_path.read_text(encoding="utf-8"))
                manifest = SkillManifest.from_dict(data)
                status_file = skill_dir / ".status"
                status = SkillStatus(status_file.read_text().strip()) if status_file.exists() else SkillStatus.INSTALLED
                config_file = skill_dir / "config.json"
                config = json.loads(config_file.read_text(encoding="utf-8")) if config_file.exists() else {}
                self._skills[manifest.name] = InstalledSkill(
                    manifest=manifest, status=status, config=config, path=str(skill_dir),
                )
            except Exception as e:
                logger.warning("Failed to load skill from {}: {}", skill_dir, e)

    def install(self, name: str, source_path: Path, manifest: SkillManifest | None = None) -> InstalledSkill:
        target = self._skills_dir / name
        if target.exists():
            shutil.rmtree(target)
        shutil.copytree(source_path, target)

        if manifest is None:
            mp = target / "manifest.json"
            if mp.exists():
                manifest = SkillManifest.from_dict(json.loads(mp.read_text(encoding="utf-8")))
            else:
                manifest = SkillManifest(name=name)

        (target / "manifest.json").write_text(json.dumps(manifest.to_dict(), indent=2), encoding="utf-8")
        skill = InstalledSkill(manifest=manifest, status=SkillStatus.INSTALLED, path=str(target))
        self._skills[name] = skill
        self._write_status(name, SkillStatus.INSTALLED)
        return skill

    def uninstall(self, name: str) -> bool:
        skill = self._skills.pop(name, None)
        if skill:
            target = self._skills_dir / name
            if target.exists():
                shutil.rmtree(target)
            return True
        return False

    def enable(self, name: str) -> bool:
        skill = self._skills.get(name)
        if not skill:
            return False
        missing = [d for d in skill.manifest.dependencies if d not in self._skills or self._skills[d].status != SkillStatus.ENABLED]
        if missing:
            logger.warning("Skill {} has unmet dependencies: {}", name, missing)
            return False
        skill.status = SkillStatus.ENABLED
        self._write_status(name, SkillStatus.ENABLED)
        return True

    def disable(self, name: str) -> bool:
        skill = self._skills.get(name)
        if not skill:
            return False
        skill.status = SkillStatus.DISABLED
        self._write_status(name, SkillStatus.DISABLED)
        return True

    def configure(self, name: str, config: dict[str, Any]) -> bool:
        skill = self._skills.get(name)
        if not skill:
            return False
        skill.config.update(config)
        config_path = self._skills_dir / name / "config.json"
        config_path.write_text(json.dumps(skill.config, indent=2), encoding="utf-8")
        return True

    def upgrade(self, name: str, source_path: Path, new_manifest: SkillManifest) -> bool:
        skill = self._skills.get(name)
        if not skill:
            return False
        skill.previous_versions.append(skill.manifest.version)
        backup = self._skills_dir / f"{name}.bak"
        if backup.exists():
            shutil.rmtree(backup)
        shutil.copytree(self._skills_dir / name, backup)
        self.install(name, source_path, new_manifest)
        return True

    def rollback(self, name: str) -> bool:
        backup = self._skills_dir / f"{name}.bak"
        if not backup.exists():
            return False
        target = self._skills_dir / name
        if target.exists():
            shutil.rmtree(target)
        shutil.move(str(backup), str(target))
        self._load_installed()
        return True

    def get_skill(self, name: str) -> InstalledSkill | None:
        return self._skills.get(name)

    def list_skills(self, status: SkillStatus | None = None) -> list[InstalledSkill]:
        skills = list(self._skills.values())
        if status:
            skills = [s for s in skills if s.status == status]
        return skills

    def get_enabled_context(self) -> str:
        parts = []
        for skill in self._skills.values():
            if skill.status != SkillStatus.ENABLED:
                continue
            entry = Path(skill.path) / skill.manifest.entry_file
            if entry.exists():
                parts.append(f"## Skill: {skill.manifest.name}\n\n{entry.read_text(encoding='utf-8')}")
        return "\n\n".join(parts)

    def _write_status(self, name: str, status: SkillStatus) -> None:
        status_file = self._skills_dir / name / ".status"
        status_file.write_text(status.value, encoding="utf-8")


@dataclass
class ExperienceEntry:
    id: str = ""
    task_type: str = ""
    pattern: str = ""
    outcome: str = ""  # success | failure
    steps: list[str] = field(default_factory=list)
    tools_used: list[str] = field(default_factory=list)
    error_info: str = ""
    reuse_count: int = 0
    created_at: str = field(default_factory=lambda: datetime.now().isoformat())


class ExperienceStore:
    """Stores and retrieves execution patterns for learning from past tasks."""

    def __init__(self, store_path: Path):
        self._path = store_path
        self._entries: list[ExperienceEntry] = []
        self._load()

    def _load(self) -> None:
        if not self._path.exists():
            return
        try:
            data = json.loads(self._path.read_text(encoding="utf-8"))
            for item in data:
                self._entries.append(ExperienceEntry(**item))
        except Exception as e:
            logger.debug("Failed to load experience entries: {}", e)

    def _save(self) -> None:
        self._path.parent.mkdir(parents=True, exist_ok=True)
        data = [
            {
                "id": e.id, "task_type": e.task_type, "pattern": e.pattern,
                "outcome": e.outcome, "steps": e.steps, "tools_used": e.tools_used,
                "error_info": e.error_info, "reuse_count": e.reuse_count,
                "created_at": e.created_at,
            }
            for e in self._entries
        ]
        self._path.write_text(json.dumps(data, ensure_ascii=False, indent=2), encoding="utf-8")

    def record_success(self, task_type: str, pattern: str, steps: list[str], tools: list[str]) -> None:
        entry = ExperienceEntry(
            id=f"exp_{len(self._entries)}",
            task_type=task_type, pattern=pattern, outcome="success",
            steps=steps, tools_used=tools,
        )
        self._entries.append(entry)
        self._save()

    def record_failure(self, task_type: str, pattern: str, error: str, tools: list[str]) -> None:
        entry = ExperienceEntry(
            id=f"exp_{len(self._entries)}",
            task_type=task_type, pattern=pattern, outcome="failure",
            error_info=error, tools_used=tools,
        )
        self._entries.append(entry)
        self._save()

    def find_similar(self, task_type: str, limit: int = 5) -> list[ExperienceEntry]:
        matches = [e for e in self._entries if e.task_type == task_type and e.outcome == "success"]
        matches.sort(key=lambda e: e.reuse_count, reverse=True)
        return matches[:limit]

    def mark_reused(self, entry_id: str) -> None:
        for e in self._entries:
            if e.id == entry_id:
                e.reuse_count += 1
                self._save()
                return

    def get_error_patterns(self, task_type: str = "") -> list[ExperienceEntry]:
        entries = [e for e in self._entries if e.outcome == "failure"]
        if task_type:
            entries = [e for e in entries if e.task_type == task_type]
        return entries
