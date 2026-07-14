"""Agent-facing skill install tool — install skills from git, local path, or URL.

Fetches skill sources into a temp directory, validates SKILL.md presence,
and copies into the SkillStore user directory.
"""

from __future__ import annotations

import asyncio
import os
import re
import shutil
import tempfile
import zipfile
from pathlib import Path
from typing import Any

from loguru import logger

from echo_agent.agent.tools.base import Tool, ToolExecutionContext, ToolResult
from echo_agent.dependencies.lazy_deps import (
    INSTALL_TIMEOUT_SECONDS,
    install_authorized_async,
)
from echo_agent.skills.store import SkillStore, parse_frontmatter

_TIMEOUT = 60
# Bound the recursive SKILL.md search so a mistakenly broad local source
# (e.g. a home directory) cannot walk unbounded even off the event loop.
_MAX_SCAN_ENTRIES = 20_000
_MAX_COPY_FILES = 2_000
_GIT_URL_RE = re.compile(
    r"^(https?://[^\s]+\.git|git@[^\s]+\.git|https?://github\.com/[^\s]+|https?://gitlab\.com/[^\s]+)$"
)
_SAFE_URL_RE = re.compile(r"^https?://[^\s]+$")
_SAFE_NAME_RE = re.compile(r"^[a-z0-9][a-z0-9._-]{0,63}$")

_SAFE_PIP_PKG = re.compile(r"^[a-zA-Z0-9][a-zA-Z0-9._-]*(\[[a-zA-Z0-9,._-]+\])?(([><=!~]=?|===?)[a-zA-Z0-9.*_-]+)?$")
_SAFE_BREW_FORMULA = re.compile(r"^[a-z0-9][a-z0-9+._@-]*(\/[a-z0-9][a-z0-9+._@-]*){0,2}$")


async def _run(cmd: list[str], cwd: str | None = None, timeout: int = _TIMEOUT) -> tuple[int, str, str]:
    proc = await asyncio.create_subprocess_exec(
        *cmd,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
        cwd=cwd,
    )
    try:
        stdout, stderr = await asyncio.wait_for(proc.communicate(), timeout=timeout)
    except asyncio.TimeoutError:
        proc.kill()
        await proc.wait()
        return -1, "", f"Command timed out after {timeout}s"
    return proc.returncode or 0, stdout.decode(errors="replace"), stderr.decode(errors="replace")


async def _fetch_git(location: str, tmpdir: str) -> tuple[str | None, str]:
    if not _GIT_URL_RE.match(location):
        return None, f"Invalid git URL: {location}"
    dest = str(Path(tmpdir) / "repo")
    code, _, stderr = await _run(["git", "clone", "--depth", "1", location, dest])
    if code != 0:
        return None, f"git clone failed: {stderr.strip()}"
    return dest, ""


async def _fetch_url(location: str, tmpdir: str) -> tuple[str | None, str]:
    if not _SAFE_URL_RE.match(location):
        return None, f"Invalid URL: {location}"
    dest = Path(tmpdir) / "download"
    dest.mkdir()
    archive = Path(tmpdir) / "archive"
    code, _, stderr = await _run(["curl", "-fsSL", "-o", str(archive), location])
    if code != 0:
        return None, f"Download failed: {stderr.strip()}"
    if location.endswith(".zip"):
        try:
            with zipfile.ZipFile(str(archive)) as zf:
                zf.extractall(str(dest))
        except zipfile.BadZipFile:
            return None, "Downloaded file is not a valid zip"
    else:
        code, _, stderr = await _run(["tar", "xf", str(archive), "-C", str(dest)])
        if code != 0:
            return None, f"Extract failed: {stderr.strip()}"
    return str(dest), ""


def _fetch_local(location: str) -> tuple[str | None, str]:
    p = Path(location).expanduser().resolve()
    if not p.exists():
        return None, f"Path not found: {location}"
    if not p.is_dir():
        return None, f"Not a directory: {location}"
    return str(p), ""


def _find_skill_md(base: str, subdirectory: str) -> tuple[Path | None, str]:
    root = Path(base)
    if subdirectory:
        root = root / subdirectory
    if not root.is_dir():
        return None, f"Subdirectory not found: {subdirectory}"
    skill_md = root / "SKILL.md"
    if skill_md.exists():
        return root, ""
    # Bounded recursive scan: prune vendored dirs and cap total entries so a
    # broad source can't walk the whole filesystem.
    _SKIP = {".git", "__pycache__", "node_modules", ".venv", "venv"}
    scanned = 0

    for dirpath, dirnames, filenames in os.walk(root, followlinks=False):
        dirnames[:] = [d for d in dirnames if d not in _SKIP]
        if "SKILL.md" in filenames:
            return Path(dirpath), ""
        scanned += len(filenames) + len(dirnames)
        if scanned > _MAX_SCAN_ENTRIES:
            return None, "No SKILL.md found within scan limit"
    return None, "No SKILL.md found in source"


def _resolve_name(skill_dir: Path, override: str) -> tuple[str, str]:
    if override:
        if not _SAFE_NAME_RE.match(override):
            return "", f"Invalid skill name: {override}"
        return override, ""
    skill_md = skill_dir / "SKILL.md"
    fm, _ = parse_frontmatter(skill_md.read_text(encoding="utf-8"))
    name = fm.get("name", "")
    if not name:
        name = skill_dir.name
    if not _SAFE_NAME_RE.match(name):
        return "", f"Skill name '{name}' from frontmatter is invalid, provide a name override"
    return name, ""


async def _run_install_specs(specs: list[dict], timeout: int = _TIMEOUT) -> list[str]:
    results: list[str] = []
    for spec in specs:
        kind = spec.get("kind", "")
        if kind == "pip":
            pkg = spec.get("package", "")
            if not pkg or not _SAFE_PIP_PKG.match(pkg):
                results.append(f"[pip] skipped unsafe package: {pkg}")
                continue
            res = await install_authorized_async((pkg,), source=f"tool:skill_install:{pkg}")
            results.append(f"[pip] {pkg}: {'ok' if res['success'] else res['detail']}")
        elif kind == "brew":
            formula = spec.get("formula", "")
            if not formula or not _SAFE_BREW_FORMULA.match(formula):
                results.append(f"[brew] skipped unsafe formula: {formula}")
                continue
            code, out, err = await _run(["brew", "install", formula], timeout=timeout)
            results.append(f"[brew] {formula}: {'ok' if code == 0 else err.strip()}")
        elif kind == "shell":
            cmd = spec.get("command", "")
            if not cmd:
                continue
            results.append(f"[shell] skipped for safety: {cmd[:80]}")
        else:
            results.append(f"[{kind}] unknown install kind, skipped")
    return results


class SkillInstallTool(Tool):
    name = "skill_install"
    risk_level = "dangerous"
    description = (
        "Install a skill from an external source into the local skill store. "
        "Supported sources: 'git' (clone a repo), 'local' (copy from filesystem path), "
        "'url' (download tarball/zip). The source must contain a SKILL.md file."
    )
    parameters = {
        "type": "object",
        "properties": {
            "source": {
                "type": "string",
                "enum": ["git", "local", "url"],
                "description": "Where to fetch the skill from",
            },
            "location": {
                "type": "string",
                "description": "Git URL, local path, or download URL",
            },
            "name": {
                "type": "string",
                "description": "Override skill name (default: read from SKILL.md frontmatter)",
            },
            "subdirectory": {
                "type": "string",
                "description": "Path within the source to the skill directory (for monorepos)",
            },
            "run_install": {
                "type": "boolean",
                "description": "Run install specs from skill metadata (default true)",
            },
        },
        "required": ["source", "location"],
    }

    # May run one or more pip installs (each up to INSTALL_TIMEOUT_SECONDS on
    # the serialized install executor) after fetching the source. Keep the
    # registry's wait_for ceiling above a single install plus fetch overhead so
    # a slow install runs to completion instead of being abandoned mid-write.
    timeout_seconds = INSTALL_TIMEOUT_SECONDS + _TIMEOUT

    def __init__(self, store: SkillStore):
        self._store = store

    def _materialize(
        self, fetched: str, subdirectory: str, name_override: str
    ) -> tuple[str, dict, str]:
        """Blocking: locate SKILL.md, register the skill, copy support files.

        Runs on a worker thread (see execute). Returns (skill_name, frontmatter,
        error); on error skill_name is "" and frontmatter is {}.
        """
        skill_dir, err = _find_skill_md(fetched, subdirectory)
        if err:
            return "", {}, err

        skill_name, err = _resolve_name(skill_dir, name_override)
        if err:
            return "", {}, err

        skill_md = skill_dir / "SKILL.md"
        content = skill_md.read_text(encoding="utf-8")
        fm, _ = parse_frontmatter(content)
        category = fm.get("category", "")

        install_err = self._store.create_skill(skill_name, content, category=category)
        if install_err and "already exists" in install_err:
            install_err = self._store.update_skill(skill_name, content)
        if install_err:
            return "", {}, install_err

        copied = 0
        for subdir_name in ("references", "templates", "scripts", "assets"):
            src_sub = skill_dir / subdir_name
            if not src_sub.is_dir():
                continue
            for f in src_sub.rglob("*"):
                if not f.is_file():
                    continue
                copied += 1
                if copied > _MAX_COPY_FILES:
                    return "", {}, f"Too many support files (>{_MAX_COPY_FILES}); refusing to copy"
                rel = str(f.relative_to(skill_dir))
                self._store.write_file(skill_name, rel, f.read_text(encoding="utf-8"))

        return skill_name, fm, ""

    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        source = params["source"]
        location = params["location"]
        name_override = params.get("name", "")
        subdirectory = params.get("subdirectory", "")
        run_install = params.get("run_install", True)

        tmpdir = tempfile.mkdtemp(prefix="echo_skill_")
        try:
            if source == "git":
                fetched, err = await _fetch_git(location, tmpdir)
            elif source == "url":
                fetched, err = await _fetch_url(location, tmpdir)
            elif source == "local":
                fetched, err = _fetch_local(location)
            else:
                return ToolResult(success=False, error=f"Unknown source: {source}")

            if err:
                return ToolResult(success=False, error=err)

            # The scan + read + copy sequence is all blocking filesystem work.
            # Offload it to a worker thread so the event loop (channel polling,
            # healthz, other sessions) stays responsive during install.
            skill_name, fm, err = await asyncio.to_thread(
                self._materialize, fetched, subdirectory, name_override
            )
            if err:
                return ToolResult(success=False, error=err)

            install_results: list[str] = []
            if run_install:
                meta = fm.get("metadata", {}) or {}
                echo_meta = meta.get("echo", {}) or {}
                specs = echo_meta.get("install", [])
                if specs:
                    install_results = await _run_install_specs(specs)

            output = f"Skill '{skill_name}' installed successfully."
            if install_results:
                output += "\n\nInstall results:\n" + "\n".join(f"  {r}" for r in install_results)
            logger.info("Installed skill '{}' from {} ({})", skill_name, source, location)
            return ToolResult(success=True, output=output)

        except Exception as e:
            logger.error("Skill install failed: {}", e)
            return ToolResult(success=False, error=f"Install failed: {e}")
        finally:
            # Cleanup can walk a large cloned repo; keep it off the loop too.
            await asyncio.to_thread(shutil.rmtree, tmpdir, ignore_errors=True)
