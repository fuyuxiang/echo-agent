"""Skill store — agentskills.io-compatible on-disk skill management.

Handles discovery, CRUD, validation, progressive disclosure, and atomic writes.
Skills are stored as SKILL.md files with YAML frontmatter in a directory hierarchy:
  {skills_root}/{category}/{skill-name}/SKILL.md
"""

from __future__ import annotations

import asyncio
import json
import os
import re
import shutil
import tempfile
from collections.abc import Awaitable, Callable
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import yaml
from loguru import logger

_NAME_RE = re.compile(r"^[a-z0-9][a-z0-9._-]{0,63}$")
_MAX_CONTENT_BYTES = 100_000
_MAX_FILE_BYTES = 1_048_576
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
    body = content[end + 4 :].lstrip("\n")
    try:
        fm = yaml.safe_load(fm_text) or {}
    except yaml.YAMLError:
        fm = {}
    # Valid YAML is not necessarily a mapping: "- item" parses to a list and
    # "text" to a str, and every caller here goes straight to fm.get(). One
    # malformed SKILL.md anywhere under a skills root used to raise
    # AttributeError out of list_all() and take the entire skills context with
    # it — all skills invisible because of one bad file. Treat non-mappings as
    # "no frontmatter" so the damage stays local to that skill.
    if not isinstance(fm, dict):
        return {}, body
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
        # Disables promoted by the evolution gate are persisted here so they
        # survive restarts — the in-memory set alone evaporates with the process.
        self._disabled_file = self._user_dir / ".evolution_disabled.json"
        self._persisted_disabled = self._load_persisted_disabled()
        self._disabled = set(disabled or []) | self._persisted_disabled
        self._event_sink: Callable[[str, dict[str, Any]], Awaitable[None]] | None = None

    def set_event_sink(
        self,
        sink: Callable[[str, dict[str, Any]], Awaitable[None]] | None,
    ) -> None:
        """Publish best-effort control-plane updates after a skill mutation."""
        self._event_sink = sink

    def _emit_changed(self, operation: str, name: str) -> None:
        if self._event_sink is None:
            return
        try:
            asyncio.get_running_loop().create_task(
                self._event_sink(
                    "skill_changed",
                    {"operation": operation, "name": name},
                )
            )
        except RuntimeError:
            # SkillStore is also usable from synchronous maintenance scripts.
            # Missing a dashboard hint there is preferable to owning an event
            # loop in this storage class.
            pass

    def _load_persisted_disabled(self) -> set[str]:
        try:
            if self._disabled_file.exists():
                data = json.loads(self._disabled_file.read_text(encoding="utf-8"))
                if isinstance(data, list):
                    return {str(item) for item in data}
        except Exception as e:
            logger.warning("Failed to load persisted skill disables: {}", e)
        return set()

    def _save_persisted_disabled(self) -> None:
        try:
            self._disabled_file.write_text(
                json.dumps(sorted(self._persisted_disabled), indent=2),
                encoding="utf-8",
            )
        except Exception as e:
            logger.warning("Failed to persist skill disables: {}", e)

    def persist_disable(self, name: str) -> None:
        """Disable a skill durably (used by evolution promote).

        Expands to every alias of the skill: a skill installed under a name
        override answers to both its directory name and its frontmatter name,
        and disabling just one would leave the other runnable.
        """
        for alias in self.resolved_names(name):
            self._disabled.add(alias)
            self._persisted_disabled.add(alias)
        self._save_persisted_disabled()
        self._emit_changed("disabled", name)

    def persist_enable(self, name: str) -> None:
        """Undo a durable disable (used by evolution rollback)."""
        # include_disabled: the skill is disabled right now — that is the whole
        # reason we are here — so plain resolution would not find its aliases.
        for alias in self.resolved_names(name):
            self._disabled.discard(alias)
            self._persisted_disabled.discard(alias)
        self._save_persisted_disabled()
        self._emit_changed("enabled", name)

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

    def _find_skill_dir(self, name: str, *, include_disabled: bool = False) -> Path | None:
        """Locate a skill's directory by name.

        This is the single entry point every read, write and run goes through,
        so the disabled check belongs here rather than in each caller. It used
        to live only in ``list_all()``, which meant a disabled skill was merely
        *hidden*: read_skill, read_file and skill_run all still resolved it, and
        the evolution gate's persist_disable() could not actually stop a
        misbehaving skill from running. Callers that legitimately need a
        disabled skill (re-enabling it, deleting it, admin listings) opt in via
        ``include_disabled``.
        """
        if not include_disabled and name in self._disabled:
            return None
        for root, _ in self._all_roots():
            for candidate in root.rglob("SKILL.md"):
                # A single unreadable SKILL.md (bad encoding, races with an
                # install, permissions) must not abort discovery for every
                # other skill.
                try:
                    fm, _ = parse_frontmatter(candidate.read_text(encoding="utf-8"))
                except (OSError, UnicodeDecodeError) as e:
                    logger.debug("Skipping unreadable SKILL.md at {}: {}", candidate, e)
                    continue
                if fm.get("name") == name:
                    return candidate.parent
            direct = root / name
            if (direct / "SKILL.md").exists():
                return direct
        return None

    def find_skill_dir(self, name: str, *, include_disabled: bool = False) -> Path | None:
        """Public version of ``_find_skill_dir``.

        Exposed so the runnable script path (skill_run) can pin its cwd to
        the skill root without reaching into a private API.
        """
        return self._find_skill_dir(name, include_disabled=include_disabled)

    def is_disabled(self, name: str) -> bool:
        """Whether a skill is disabled, by either config or a persisted disable.

        Public so callers can tell "disabled" from "missing" in their error
        messages, and so the gateway stops reaching into ``store._disabled``.
        """
        return name in self._disabled

    def resolved_names(self, name: str) -> set[str]:
        """Every name that resolves to the same skill directory as ``name``.

        A skill installed under a name override answers to two names: the
        directory name (matched by the ``root / name`` probe) and the one in
        frontmatter (matched by the rglob). Disabling only the name the user
        typed would leave the other one runnable, so disable operations expand
        through this.
        """
        skill_dir = self._find_skill_dir(name, include_disabled=True)
        if skill_dir is None:
            return {name}
        names = {name, skill_dir.name}
        skill_md = skill_dir / "SKILL.md"
        try:
            fm, _ = parse_frontmatter(skill_md.read_text(encoding="utf-8"))
            fm_name = fm.get("name")
            if isinstance(fm_name, str) and fm_name:
                names.add(fm_name)
        except (OSError, UnicodeDecodeError):
            # Directory-name resolution still works when optional frontmatter is
            # unreadable; metadata loading reports its own diagnostics later.
            pass
        return names

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

    def list_all(self, *, include_disabled: bool = False) -> list[SkillMeta]:
        """Tier 0: compact metadata for all skills.

        ``include_disabled`` is for admin surfaces (the gateway's skill list)
        that must show a disabled skill in order to offer an enable button.
        """
        results: list[SkillMeta] = []
        seen: set[str] = set()
        for root, _ in self._all_roots():
            if not root.exists():
                continue
            for skill_md in root.rglob("SKILL.md"):
                meta = self._read_meta(skill_md.parent)
                if not meta or meta.name in seen:
                    continue
                if not include_disabled and self._is_dir_disabled(skill_md.parent, meta.name):
                    continue
                seen.add(meta.name)
                results.append(meta)
        results.sort(key=lambda m: m.name)
        return results

    def _is_dir_disabled(self, skill_dir: Path, meta_name: str) -> bool:
        """Whether a skill directory is disabled under any of its names.

        Checked against the directory name too, not just the frontmatter name:
        an aliased skill is reachable by both, so listing it as enabled while
        one of its names is disabled would misreport its state.
        """
        return meta_name in self._disabled or skill_dir.name in self._disabled

    def read_skill(self, name: str, *, include_disabled: bool = False) -> str | None:
        """Tier 1: full SKILL.md content."""
        skill_dir = self._find_skill_dir(name, include_disabled=include_disabled)
        if not skill_dir:
            return None
        skill_md = skill_dir / "SKILL.md"
        if not skill_md.exists():
            return None
        try:
            return skill_md.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError) as e:
            logger.warning("Failed to read SKILL.md for '{}': {}", name, e)
            return None

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
        try:
            return target.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError) as e:
            # assets/ legitimately holds PNGs and fonts. Returning None with a
            # log beats propagating UnicodeDecodeError to the model as a crash.
            logger.debug("Failed to read '{}' in skill '{}': {}", file_path, name, e)
            return None

    def list_files(self, name: str, *, include_disabled: bool = False) -> list[str]:
        """List supporting files for a skill.

        Disabled by default, like every other read path: a disabled skill's files
        must not be reachable. ``include_disabled`` exists for the maintenance
        callers that legitimately need them — snapshot and rollback around an
        install, which must be able to save and restore a disabled skill's
        support files or an upgrade that fails takes them with it.
        """
        skill_dir = self._find_skill_dir(name, include_disabled=include_disabled)
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

        # include_disabled: a disabled skill still occupies its name on disk.
        # Without this, creating over a disabled skill would silently write into
        # its directory and the "already exists" guard would not fire.
        if self._find_skill_dir(name, include_disabled=True):
            return f"skill '{name}' already exists"

        parent = self._user_dir / category if category else self._user_dir
        skill_dir = parent / name
        skill_dir.mkdir(parents=True, exist_ok=True)

        final_content = _build_frontmatter(fm, body)
        self._atomic_write(skill_dir / "SKILL.md", final_content)
        logger.info("Created skill '{}' at {}", name, skill_dir)
        self._emit_changed("created", name)
        return None

    def update_skill(self, name: str, content: str) -> str | None:
        """Replace full SKILL.md content. Returns error string or None.

        Resolves disabled skills too: disabling a skill must stop it from
        *running*, not lock the operator out of repairing or deleting it.
        """
        skill_dir = self._find_skill_dir(name, include_disabled=True)
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
        self._emit_changed("updated", name)
        return None

    def patch_skill(self, name: str, old_text: str, new_text: str, file_path: str = "") -> str | None:
        """Find-and-replace within SKILL.md or a supporting file.

        include_disabled: repairing a disabled skill is the normal way to make
        it safe to re-enable.
        """
        skill_dir = self._find_skill_dir(name, include_disabled=True)
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
        self._emit_changed("updated", name)
        return None

    def delete_skill(self, name: str) -> str | None:
        """Remove a skill entirely.

        include_disabled: deleting is the most likely thing an operator wants to
        do to a skill they just disabled.
        """
        skill_dir = self._find_skill_dir(name, include_disabled=True)
        if not skill_dir:
            return f"skill '{name}' not found"
        if not self._is_writable(skill_dir):
            return f"skill '{name}' is read-only"
        shutil.rmtree(skill_dir)
        # Drop the disable entries too, or the name stays poisoned: a later
        # skill installed under it would be silently disabled with no directory
        # left to explain why.
        for alias in {name, skill_dir.name}:
            self._disabled.discard(alias)
            self._persisted_disabled.discard(alias)
        self._save_persisted_disabled()
        logger.info("Deleted skill '{}'", name)
        self._emit_changed("deleted", name)
        return None

    def write_file(self, name: str, file_path: str, content: str) -> str | None:
        """Add or overwrite a supporting file (text)."""
        return self._write_file_payload(name, file_path, content)

    def write_file_bytes(self, name: str, file_path: str, data: bytes) -> str | None:
        """Add or overwrite a supporting file (binary).

        ``assets`` is in _ALLOWED_SUBDIRS precisely so skills can ship images
        and fonts, but the only writer was text-only — so installing any skill
        with a PNG died on UnicodeDecodeError partway through copying.
        """
        return self._write_file_payload(name, file_path, data)

    def _write_file_payload(
        self,
        name: str,
        file_path: str,
        payload: str | bytes,
    ) -> str | None:
        skill_dir = self._find_skill_dir(name, include_disabled=True)
        if not skill_dir:
            return f"skill '{name}' not found"
        if not self._is_writable(skill_dir):
            return f"skill '{name}' is read-only"
        if ".." in file_path or file_path.startswith("/"):
            return "path traversal not allowed"

        parts = Path(file_path).parts
        if not parts or parts[0] not in _ALLOWED_SUBDIRS:
            return f"file must be under one of: {', '.join(sorted(_ALLOWED_SUBDIRS))}"
        size = len(payload.encode("utf-8")) if isinstance(payload, str) else len(payload)
        if size > _MAX_FILE_BYTES:
            return "file exceeds 1 MiB limit"

        target = skill_dir / file_path
        # The subdir check above constrains the first segment, but a symlinked
        # directory inside the skill could still redirect the write outside it.
        try:
            target.parent.resolve().relative_to(skill_dir.resolve())
        except ValueError:
            return "path traversal not allowed"
        target.parent.mkdir(parents=True, exist_ok=True)
        self._atomic_write(target, payload)
        logger.info("Wrote file '{}' in skill '{}'", file_path, name)
        self._emit_changed("updated", name)
        return None

    def remove_file(self, name: str, file_path: str) -> str | None:
        """Remove a supporting file."""
        skill_dir = self._find_skill_dir(name, include_disabled=True)
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
        self._emit_changed("updated", name)
        return None

    def write_provenance(
        self,
        name: str,
        *,
        source: str,
        created_at: str,
        promotion_status: str,
        created_from_session: str,
    ) -> str | None:
        """Mirror provenance into SKILL.md frontmatter under metadata.echo.provenance.

        SQLite (evolution_candidates) is the source of truth; this is a derived,
        human-readable, git-visible copy. Preserves all other frontmatter.

        include_disabled: the evolution gate disables a skill and records
        provenance in the same promotion, in either order.
        """
        skill_dir = self._find_skill_dir(name, include_disabled=True)
        if not skill_dir:
            return f"skill '{name}' not found"
        if not self._is_writable(skill_dir):
            return f"skill '{name}' is read-only"
        skill_md = skill_dir / "SKILL.md"
        if not skill_md.exists():
            return f"skill '{name}' has no SKILL.md"

        fm, body = parse_frontmatter(skill_md.read_text(encoding="utf-8"))
        meta = fm.setdefault("metadata", {})
        if not isinstance(meta, dict):
            meta = {}
            fm["metadata"] = meta
        echo_meta = meta.setdefault("echo", {})
        if not isinstance(echo_meta, dict):
            echo_meta = {}
            meta["echo"] = echo_meta
        echo_meta["provenance"] = {
            "source": source,
            "created_at": created_at,
            "promotion_status": promotion_status,
            "created_from_session": created_from_session,
        }
        self._atomic_write(skill_md, _build_frontmatter(fm, body))
        logger.info("Wrote provenance for skill '{}' (source={})", name, source)
        return None

    # ── Helpers ──────────────────────────────────────────────────────────────

    def _is_writable(self, skill_dir: Path) -> bool:
        try:
            skill_dir.resolve().relative_to(self._user_dir.resolve())
            return True
        except ValueError:
            return False

    def is_protected(self, name: str) -> bool:
        """A skill is protected from evolution if it resides outside user_dir
        (i.e. shipped builtin or external). Unknown skills are not protected —
        the caller's not-found handling takes over. Mirror of _is_writable,
        exposed as the public evolution-safety predicate.

        include_disabled: a builtin skill stays protected while disabled —
        reporting it unprotected would let evolution overwrite shipped files."""
        skill_dir = self._find_skill_dir(name, include_disabled=True)
        if skill_dir is None:
            return False
        return not self._is_writable(skill_dir)

    @staticmethod
    def _atomic_write(path: Path, content: str | bytes) -> None:
        fd, tmp = tempfile.mkstemp(dir=path.parent, suffix=".tmp")
        try:
            if isinstance(content, bytes):
                with os.fdopen(fd, "wb") as f:
                    f.write(content)
            else:
                with os.fdopen(fd, "w", encoding="utf-8") as f:
                    f.write(content)
            os.replace(tmp, path)
        except BaseException:
            try:
                os.unlink(tmp)
            except OSError:
                # Preserve the original skill write failure; atomic replace keeps
                # the prior canonical skill file intact.
                pass
            raise
