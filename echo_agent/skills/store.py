"""Skill store — agentskills.io-compatible on-disk skill management.

Handles discovery, CRUD, validation, progressive disclosure, and atomic writes.
Skills are stored as SKILL.md files with YAML frontmatter in a directory hierarchy:
  {skills_root}/{category}/{skill-name}/SKILL.md
"""

from __future__ import annotations

import os
import re
import shutil
import tempfile
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import yaml
from loguru import logger

_NAME_RE = re.compile(r"^[a-z0-9][a-z0-9._-]{0,63}$")
_MAX_CONTENT_BYTES = 100_000
_ALLOWED_SUBDIRS = frozenset({"references", "templates", "scripts", "assets"})


@dataclass
class SkillMeta:
    name: str
    description: str
    category: str = ""
    version: str = "1.0.0"
    tags: list[str] = field(default_factory=list)
    path: str = ""

    def to_dict(self) -> dict[str, Any]:
        return {
            "name": self.name,
            "description": self.description,
            "category": self.category,
            "version": self.version,
            "tags": self.tags,
        }


def parse_frontmatter(content: str) -> tuple[dict[str, Any], str]:
    """Parse YAML frontmatter from markdown content. Returns (frontmatter, body)."""
    if not content.startswith("---"):
        return {}, content
    end = content.find("\n---", 3)
    if end == -1:
        return {}, content
    fm_text = content[3:end].strip()
    body = content[end + 4:].lstrip("\n")
    try:
        fm = yaml.safe_load(fm_text) or {}
    except yaml.YAMLError:
        fm = {}
    return fm, body


def _build_frontmatter(fm: dict[str, Any], body: str) -> str:
    fm_text = yaml.dump(fm, default_flow_style=False, allow_unicode=True).strip()
    return f"---\n{fm_text}\n---\n\n{body}"


def _validate_name(name: str) -> str | None:
    if not name:
        return "name is required"
    if not _NAME_RE.match(name):
        return f"invalid name '{name}': must be lowercase alphanumeric with hyphens/dots/underscores, max 64 chars"
    return None


def _validate_category(category: str) -> str | None:
    if not category:
        return None
    if "/" in category or "\\" in category:
        return "category must be a single directory segment"
    if not _NAME_RE.match(category):
        return f"invalid category '{category}'"
    return None


class SkillStore:
    """Manages on-disk skills with agentskills.io-compatible format."""

    def __init__(
        self,
        user_dir: Path | None = None,
        builtin_dir: Path | None = None,
        external_dirs: list[Path] | None = None,
        disabled: list[str] | None = None,
    ):
        self._user_dir = user_dir or Path.home() / ".echo-agent" / "skills"
        self._user_dir.mkdir(parents=True, exist_ok=True)
        self._builtin_dir = builtin_dir
        self._external_dirs = external_dirs or []
        self._disabled = set(disabled or [])

    @property
    def user_dir(self) -> Path:
        return self._user_dir

    def _all_roots(self) -> list[tuple[Path, bool]]:
        """Returns (path, is_writable) pairs for all skill directories."""
        roots: list[tuple[Path, bool]] = [(self._user_dir, True)]
        if self._builtin_dir and self._builtin_dir.exists():
            roots.append((self._builtin_dir, False))
        for d in self._external_dirs:
            if d.exists():
                roots.append((d, False))
        return roots

    def _find_skill_dir(self, name: str) -> Path | None:
        for root, _ in self._all_roots():
            for candidate in root.rglob("SKILL.md"):
                fm, _ = parse_frontmatter(candidate.read_text(encoding="utf-8"))
                if fm.get("name") == name:
                    return candidate.parent
            direct = root / name
            if (direct / "SKILL.md").exists():
                return direct
        return None

    def _read_meta(self, skill_dir: Path) -> SkillMeta | None:
        skill_md = skill_dir / "SKILL.md"
        if not skill_md.exists():
            return None
        try:
            fm, _ = parse_frontmatter(skill_md.read_text(encoding="utf-8"))
        except Exception as e:
            logger.debug("Failed to read skill metadata from {}: {}", skill_dir.name, e)
            return None
        name = fm.get("name", skill_dir.name)
        if not name:
            return None
        parent = skill_dir.parent
        category = ""
        if parent != self._user_dir and parent.parent in [r for r, _ in self._all_roots()]:
            category = parent.name
        meta_block = fm.get("metadata", {}) or {}
        echo_meta = meta_block.get("echo", {}) or {}
        return SkillMeta(
            name=name,
            description=fm.get("description", ""),
            category=category,
            version=fm.get("version", "1.0.0"),
            tags=echo_meta.get("tags", []),
            path=str(skill_dir),
        )

    # ── Progressive disclosure ──────────────────────────────────────────────

    def list_all(self) -> list[SkillMeta]:
        """Tier 0: compact metadata for all skills."""
        results: list[SkillMeta] = []
        seen: set[str] = set()
        for root, _ in self._all_roots():
            if not root.exists():
                continue
            for skill_md in root.rglob("SKILL.md"):
                meta = self._read_meta(skill_md.parent)
                if meta and meta.name not in seen and meta.name not in self._disabled:
                    seen.add(meta.name)
                    results.append(meta)
        results.sort(key=lambda m: m.name)
        return results

    def read_skill(self, name: str) -> str | None:
        """Tier 1: full SKILL.md content."""
        skill_dir = self._find_skill_dir(name)
        if not skill_dir:
            return None
        skill_md = skill_dir / "SKILL.md"
        return skill_md.read_text(encoding="utf-8") if skill_md.exists() else None

    def read_file(self, name: str, file_path: str) -> str | None:
        """Tier 2: specific supporting file content."""
        skill_dir = self._find_skill_dir(name)
        if not skill_dir:
            return None
        if ".." in file_path or file_path.startswith("/"):
            return None
        target = skill_dir / file_path
        if not target.exists() or not target.is_file():
            return None
        try:
            target.resolve().relative_to(skill_dir.resolve())
        except ValueError:
            return None
        return target.read_text(encoding="utf-8")

    def list_files(self, name: str) -> list[str]:
        """List supporting files for a skill."""
        skill_dir = self._find_skill_dir(name)
        if not skill_dir:
            return []
        files: list[str] = []
        for sub in _ALLOWED_SUBDIRS:
            sub_dir = skill_dir / sub
            if sub_dir.is_dir():
                for f in sub_dir.rglob("*"):
                    if f.is_file():
                        files.append(str(f.relative_to(skill_dir)))
        return sorted(files)

    # ── CRUD operations ─────────────────────────────────────────────────────

    def create_skill(self, name: str, content: str, category: str = "") -> str | None:
        """Create a new skill. Returns error string or None on success."""
        err = _validate_name(name)
        if err:
            return err
        err = _validate_category(category)
        if err:
            return err
        if len(content.encode("utf-8")) > _MAX_CONTENT_BYTES:
            return f"content exceeds {_MAX_CONTENT_BYTES} byte limit"

        fm, body = parse_frontmatter(content)
        if not fm.get("name"):
            fm["name"] = name
        if not fm.get("description"):
            return "frontmatter must include a 'description' field"

        if self._find_skill_dir(name):
            return f"skill '{name}' already exists"

        parent = self._user_dir / category if category else self._user_dir
        skill_dir = parent / name
        skill_dir.mkdir(parents=True, exist_ok=True)

        final_content = _build_frontmatter(fm, body)
        self._atomic_write(skill_dir / "SKILL.md", final_content)
        logger.info("Created skill '{}' at {}", name, skill_dir)
        return None

    def update_skill(self, name: str, content: str) -> str | None:
        """Replace full SKILL.md content. Returns error string or None."""
        skill_dir = self._find_skill_dir(name)
        if not skill_dir:
            return f"skill '{name}' not found"
        if not self._is_writable(skill_dir):
            return f"skill '{name}' is read-only"
        if len(content.encode("utf-8")) > _MAX_CONTENT_BYTES:
            return f"content exceeds {_MAX_CONTENT_BYTES} byte limit"

        fm, body = parse_frontmatter(content)
        if not fm.get("name"):
            fm["name"] = name
        if not fm.get("description"):
            return "frontmatter must include a 'description' field"

        final_content = _build_frontmatter(fm, body)
        self._atomic_write(skill_dir / "SKILL.md", final_content)
        logger.info("Updated skill '{}'", name)
        return None

    def patch_skill(self, name: str, old_text: str, new_text: str, file_path: str = "") -> str | None:
        """Find-and-replace within SKILL.md or a supporting file."""
        skill_dir = self._find_skill_dir(name)
        if not skill_dir:
            return f"skill '{name}' not found"
        if not self._is_writable(skill_dir):
            return f"skill '{name}' is read-only"

        target = skill_dir / (file_path or "SKILL.md")
        if not target.exists():
            return f"file '{file_path or 'SKILL.md'}' not found in skill '{name}'"
        try:
            target.resolve().relative_to(skill_dir.resolve())
        except ValueError:
            return "path traversal not allowed"

        current = target.read_text(encoding="utf-8")
        if old_text not in current:
            return "old_text not found in file"
        updated = current.replace(old_text, new_text, 1)
        self._atomic_write(target, updated)
        logger.info("Patched skill '{}' file '{}'", name, file_path or "SKILL.md")
        return None

    def delete_skill(self, name: str) -> str | None:
        """Remove a skill entirely."""
        skill_dir = self._find_skill_dir(name)
        if not skill_dir:
            return f"skill '{name}' not found"
        if not self._is_writable(skill_dir):
            return f"skill '{name}' is read-only"
        shutil.rmtree(skill_dir)
        logger.info("Deleted skill '{}'", name)
        return None

    def write_file(self, name: str, file_path: str, content: str) -> str | None:
        """Add or overwrite a supporting file."""
        skill_dir = self._find_skill_dir(name)
        if not skill_dir:
            return f"skill '{name}' not found"
        if not self._is_writable(skill_dir):
            return f"skill '{name}' is read-only"
        if ".." in file_path or file_path.startswith("/"):
            return "path traversal not allowed"

        parts = Path(file_path).parts
        if not parts or parts[0] not in _ALLOWED_SUBDIRS:
            return f"file must be under one of: {', '.join(sorted(_ALLOWED_SUBDIRS))}"
        if len(content.encode("utf-8")) > 1_048_576:
            return "file exceeds 1 MiB limit"

        target = skill_dir / file_path
        target.parent.mkdir(parents=True, exist_ok=True)
        self._atomic_write(target, content)
        logger.info("Wrote file '{}' in skill '{}'", file_path, name)
        return None

    def remove_file(self, name: str, file_path: str) -> str | None:
        """Remove a supporting file."""
        skill_dir = self._find_skill_dir(name)
        if not skill_dir:
            return f"skill '{name}' not found"
        if not self._is_writable(skill_dir):
            return f"skill '{name}' is read-only"
        if ".." in file_path or file_path.startswith("/"):
            return "path traversal not allowed"

        target = skill_dir / file_path
        if not target.exists():
            return f"file '{file_path}' not found"
        try:
            target.resolve().relative_to(skill_dir.resolve())
        except ValueError:
            return "path traversal not allowed"
        target.unlink()
        logger.info("Removed file '{}' from skill '{}'", file_path, name)
        return None

    # ── Helpers ──────────────────────────────────────────────────────────────

    def _is_writable(self, skill_dir: Path) -> bool:
        try:
            skill_dir.resolve().relative_to(self._user_dir.resolve())
            return True
        except ValueError:
            return False

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
