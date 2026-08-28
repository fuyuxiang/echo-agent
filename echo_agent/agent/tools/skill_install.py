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

from echo_agent.tools.base import Tool, ToolExecutionContext, ToolResult
from echo_agent.dependencies.lazy_deps import (
    INSTALL_TIMEOUT_SECONDS,
    install_authorized_async,
)
from echo_agent.memory.store import scan_document_for_threats
from echo_agent.security.net_guard import check_url_ssrf
from echo_agent.skills.store import _build_frontmatter, SkillStore, parse_frontmatter

_TIMEOUT = 60
# Bound the recursive SKILL.md search so a mistakenly broad local source
# (e.g. a home directory) cannot walk unbounded even off the event loop.
_MAX_SCAN_ENTRIES = 20_000
_MAX_COPY_FILES = 2_000
# Support-file subdirs mirror SkillStore._ALLOWED_SUBDIRS — the store rejects a
# write anywhere else, so copying more would just fail one file at a time.
_SUPPORT_SUBDIRS = ("references", "templates", "scripts", "assets")
# Per-file cap matches the store's own 1 MiB write limit; the aggregate cap
# bounds what a single install can add to disk regardless of file count.
_MAX_SUPPORT_FILE_BYTES = 1_048_576
_MAX_TOTAL_SUPPORT_BYTES = 64 * 1_048_576
# Bounds for URL sources: the downloaded archive, and what it expands to.
# Without these a hostile or mistaken URL can fill the disk (zip bomb).
_MAX_ARCHIVE_BYTES = 64 * 1_048_576
_MAX_EXTRACTED_BYTES = 256 * 1_048_576
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
    # The URL comes from the model, i.e. ultimately from whatever the model was
    # reading. Without this check skill_install is a request forger against the
    # host's internal network and cloud metadata endpoints.
    ssrf_error = await check_url_ssrf(location)
    if ssrf_error:
        return None, ssrf_error
    dest = Path(tmpdir) / "download"
    dest.mkdir()
    archive = Path(tmpdir) / "archive"
    # --max-filesize makes curl refuse an oversized body up front; the stat
    # below still runs because servers may omit Content-Length, in which case
    # curl only notices after writing.
    code, _, stderr = await _run([
        "curl", "-fsSL",
        "--max-filesize", str(_MAX_ARCHIVE_BYTES),
        "--proto", "=http,https",
        "--max-redirs", "5",
        "-o", str(archive), location,
    ])
    if code != 0:
        return None, f"Download failed: {stderr.strip()}"
    try:
        if archive.stat().st_size > _MAX_ARCHIVE_BYTES:
            return None, f"Downloaded archive exceeds {_MAX_ARCHIVE_BYTES} bytes"
    except OSError as e:
        return None, f"Download failed: {e}"

    if location.endswith(".zip"):
        try:
            with zipfile.ZipFile(str(archive)) as zf:
                err = _safe_extract_zip(zf, dest)
                if err:
                    return None, err
        except zipfile.BadZipFile:
            return None, "Downloaded file is not a valid zip"
    else:
        # tar is given the same containment treatment via its own flags: refuse
        # absolute paths and ".." members rather than trusting the archive.
        code, _, stderr = await _run([
            "tar", "xf", str(archive), "-C", str(dest),
        ])
        if code != 0:
            return None, f"Extract failed: {stderr.strip()}"
        err = _check_extracted_tree(dest)
        if err:
            return None, err
    return str(dest), ""


def _safe_extract_zip(zf: zipfile.ZipFile, dest: Path) -> str:
    """Extract a zip, refusing members that escape *dest* or blow up its size.

    ``extractall`` is not safe on untrusted input: a member named
    ``../../x`` writes outside the target, and a small archive can expand to
    fill the disk.
    """
    root = dest.resolve()
    total = 0
    for info in zf.infolist():
        name = info.filename
        if name.endswith("/"):
            continue
        if name.startswith("/") or ".." in Path(name).parts:
            return f"archive member '{name}' escapes the extraction directory"
        total += info.file_size
        if total > _MAX_EXTRACTED_BYTES:
            return f"archive expands beyond {_MAX_EXTRACTED_BYTES} bytes"
        target = (dest / name).resolve()
        try:
            target.relative_to(root)
        except ValueError:
            return f"archive member '{name}' escapes the extraction directory"
    zf.extractall(str(dest))
    return ""


def _check_extracted_tree(dest: Path) -> str:
    """Post-extraction bound for archives handed to tar.

    tar has already written by this point, but the tree is still inside our
    private tmpdir and gets deleted on the way out, so catching it here keeps an
    oversized source from being copied into the store.
    """
    total = 0
    for f in dest.rglob("*"):
        try:
            if f.is_file() and not f.is_symlink():
                total += f.stat().st_size
        except OSError:
            continue
        if total > _MAX_EXTRACTED_BYTES:
            return f"extracted archive exceeds {_MAX_EXTRACTED_BYTES} bytes"
    return ""


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
        if Path(subdirectory).is_absolute():
            return None, f"Subdirectory must be relative: {subdirectory}"
        root = root / subdirectory
        # subdirectory is model-supplied, so "../../etc" would have walked out
        # of the fetched source and installed arbitrary host files as a skill.
        try:
            root.resolve().relative_to(Path(base).resolve())
        except ValueError:
            return None, f"Subdirectory escapes the source directory: {subdirectory}"
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


def _plan_support_files(skill_dir: Path) -> tuple[list[tuple[str, str | bytes]], str]:
    """Read every support file into memory before any of them is written.

    Returns (plan, error). Text files stay ``str`` so they round-trip through the
    existing UTF-8 writer; anything that is not valid UTF-8 becomes ``bytes``.
    Reading everything first is what makes the install transactional: all the
    ways this can fail (undecodable file, too many files, too large, symlink
    escape) happen before the store has been touched.
    """
    plan: list[tuple[str, str | bytes]] = []
    total = 0
    root = skill_dir.resolve()
    for subdir_name in sorted(_SUPPORT_SUBDIRS):
        src_sub = skill_dir / subdir_name
        if not src_sub.is_dir():
            continue
        for f in sorted(src_sub.rglob("*")):
            if not f.is_file():
                continue
            # Symlinks are resolved, so a link pointing out of the source tree
            # would otherwise copy arbitrary host files into the skill.
            try:
                f.resolve().relative_to(root)
            except ValueError:
                return [], f"support file '{f.name}' resolves outside the skill directory"
            if len(plan) >= _MAX_COPY_FILES:
                return [], f"Too many support files (>{_MAX_COPY_FILES}); refusing to copy"
            try:
                raw = f.read_bytes()
            except OSError as e:
                return [], f"cannot read support file '{f.name}': {e}"
            if len(raw) > _MAX_SUPPORT_FILE_BYTES:
                return [], f"support file '{f.name}' exceeds {_MAX_SUPPORT_FILE_BYTES} bytes"
            total += len(raw)
            if total > _MAX_TOTAL_SUPPORT_BYTES:
                return [], f"support files exceed {_MAX_TOTAL_SUPPORT_BYTES} bytes in total"
            rel = str(f.relative_to(skill_dir))
            try:
                plan.append((rel, raw.decode("utf-8")))
            except UnicodeDecodeError:
                plan.append((rel, raw))
    return plan, ""


def _resolve_name(skill_dir: Path, override: str) -> tuple[str, str]:
    if override:
        if not _SAFE_NAME_RE.match(override):
            return "", f"Invalid skill name: {override}"
        return override, ""
    skill_md = skill_dir / "SKILL.md"
    try:
        fm, _ = parse_frontmatter(skill_md.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError) as e:
        return "", f"Cannot read SKILL.md: {e}"
    name = fm.get("name", "")
    if not isinstance(name, str) or not name:
        name = skill_dir.name
    if not _SAFE_NAME_RE.match(name):
        return "", f"Skill name '{name}' from frontmatter is invalid, provide a name override"
    return name, ""


def _apply_name(content: str, name: str) -> str:
    """Force ``name`` into the SKILL.md frontmatter.

    A name override used to rename only the *directory*: the frontmatter kept
    the original name, and since _read_meta prefers frontmatter, a skill
    installed as ``my-alias`` was listed as ``original-name``. Both names then
    resolved to it (one via the frontmatter rglob, one via the ``root / name``
    probe), so it could collide with a genuine ``original-name`` and a disable
    applied to one name left the other live. One skill, one name.
    """
    fm, body = parse_frontmatter(content)
    if fm.get("name") == name:
        return content
    fm["name"] = name
    return _build_frontmatter(fm, body)


def _merge_install_specs(specs: Any, echo_meta: dict) -> list[dict]:
    """Normalize both dependency dialects into one list of specs.

    ``metadata.echo.install`` is a list of ``{kind, package|formula}`` dicts;
    ``metadata.echo.requires.pip`` is a plain list of pip specs and is the form
    skill_view/skill_run precheck against. Only 3 of 35 builtin skills used the
    former, so an install that read only ``install`` skipped nearly every
    declared dependency.
    """
    merged: list[dict] = []
    seen: set[tuple[str, str]] = set()

    if isinstance(specs, list):
        for spec in specs:
            if not isinstance(spec, dict):
                continue
            kind = str(spec.get("kind", ""))
            ident = str(spec.get("package") or spec.get("formula") or spec.get("command") or "")
            if (kind, ident) in seen:
                continue
            seen.add((kind, ident))
            merged.append(spec)

    requires = echo_meta.get("requires") or {}
    if isinstance(requires, dict):
        for pkg in requires.get("pip") or []:
            if not isinstance(pkg, str) or not pkg.strip():
                continue
            key = ("pip", pkg.strip())
            if key in seen:
                continue
            seen.add(key)
            merged.append({"kind": "pip", "package": pkg.strip()})
    return merged


async def _run_install_specs(
    specs: list[dict], timeout: int = _TIMEOUT,
) -> tuple[list[str], list[str]]:
    """Run install specs. Returns (log lines, hard failures).

    A "hard failure" is a declared dependency that did not end up installed —
    the skill is on disk but its scripts will not run. Callers must not report
    success when this list is non-empty. Specs we refuse on principle (shell)
    are reported in the log but are not failures: they were never going to run.
    """
    results: list[str] = []
    failures: list[str] = []
    for spec in specs:
        kind = spec.get("kind", "")
        if kind == "pip":
            pkg = spec.get("package", "")
            if not pkg or not _SAFE_PIP_PKG.match(pkg):
                results.append(f"[pip] skipped unsafe package: {pkg}")
                failures.append(f"pip spec rejected as unsafe: {pkg}")
                continue
            res = await install_authorized_async((pkg,), source=f"tool:skill_install:{pkg}")
            results.append(f"[pip] {pkg}: {'ok' if res['success'] else res['detail']}")
            if not res["success"]:
                failures.append(f"pip {pkg}: {res['detail']}")
        elif kind == "brew":
            formula = spec.get("formula", "")
            if not formula or not _SAFE_BREW_FORMULA.match(formula):
                results.append(f"[brew] skipped unsafe formula: {formula}")
                failures.append(f"brew formula rejected as unsafe: {formula}")
                continue
            code, out, err = await _run(["brew", "install", formula], timeout=timeout)
            results.append(f"[brew] {formula}: {'ok' if code == 0 else err.strip()}")
            if code != 0:
                failures.append(f"brew {formula}: {err.strip() or 'failed'}")
        elif kind == "shell":
            cmd = spec.get("command", "")
            if not cmd:
                continue
            results.append(f"[shell] skipped for safety: {cmd[:80]}")
        else:
            results.append(f"[{kind}] unknown install kind, skipped")
    return results, failures


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
        # Set by _materialize (which runs on a worker thread) and read back in
        # execute() to append to the user-facing output.
        self._last_scan_warnings: list[str] = []

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
        try:
            content = skill_md.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError) as e:
            return "", {}, f"Cannot read SKILL.md: {e}"
        content = _apply_name(content, skill_name)
        fm, _ = parse_frontmatter(content)
        category = fm.get("category", "")
        if not isinstance(category, str):
            category = ""

        # An installed skill's description is injected into the system prompt on
        # every turn, and its body is what the model reads before acting — the
        # same exposure the evolution gate already scans candidate skills for.
        # External installs bypassed that check entirely, so a third-party
        # SKILL.md could carry instructions aimed at the model itself.
        # Content that tries to steer the model is refused; command-shaped
        # matches (a config path, an API call carrying the service's own key) are
        # reported to the user instead, because in documentation those are
        # usually honest and running any of it still needs exec approval.
        fatal, warnings = scan_document_for_threats(content)
        if fatal:
            return "", {}, f"refused: SKILL.md failed the injection scan ({fatal})"
        self._last_scan_warnings = warnings

        # Validate the whole payload before touching the store. Installing was
        # previously "create the skill, then copy files until something throws",
        # which left half-installed skills behind on the first binary asset or
        # over-limit file — visible in skills_list, not runnable.
        plan, err = _plan_support_files(skill_dir)
        if err:
            return "", {}, err

        existing = self._store.read_skill(skill_name, include_disabled=True)
        backup: dict[str, Any] | None = None
        if existing is not None:
            backup = self._snapshot(skill_name, existing)

        install_err = self._store.create_skill(skill_name, content, category=category)
        if install_err and "already exists" in install_err:
            install_err = self._store.update_skill(skill_name, content)
        if install_err:
            return "", {}, install_err

        try:
            for rel, data in plan:
                write_err = (
                    self._store.write_file_bytes(skill_name, rel, data)
                    if isinstance(data, bytes)
                    else self._store.write_file(skill_name, rel, data)
                )
                if write_err:
                    raise RuntimeError(f"{rel}: {write_err}")

            # Sweep files the previous version had and the new one does not.
            # Copying is an overwrite, so without this an upgrade that *removed*
            # a script or asset left the old copy on disk — still discoverable by
            # skills_list and still executable, which makes "upgraded" a lie
            # about what the skill can now do.
            if backup is not None:
                self._prune_removed_files(skill_name, {rel for rel, _ in plan})
        except Exception as e:
            self._rollback(skill_name, backup)
            return "", {}, f"Failed to copy support files ({e}); install rolled back"

        return skill_name, fm, ""

    def _prune_removed_files(self, name: str, keep: set[str]) -> None:
        """Delete support files not present in the new version."""
        for rel in self._store.list_files(name, include_disabled=True):
            if rel in keep:
                continue
            remove_err = self._store.remove_file(name, rel)
            if remove_err:
                # Surfaced to the caller, which rolls the install back: a
                # partially pruned tree is a skill in neither version.
                raise RuntimeError(f"could not remove stale file {rel}: {remove_err}")

    def _snapshot(self, name: str, content: str) -> dict[str, Any]:
        """Capture an existing skill so a failed upgrade can be undone.

        ``include_disabled=True`` throughout, and it has to be: the skill was
        located that way (a disabled skill is still upgradeable), so listing its
        files without the same flag returned nothing and a failed upgrade of a
        disabled skill silently dropped every support file it had.

        The disable state is captured too — restoring content and files while
        leaving the skill enabled would turn a failed upgrade into a silent
        re-enable of something the operator deliberately switched off.
        """
        files: dict[str, bytes] = {}
        skill_dir = self._store.find_skill_dir(name, include_disabled=True)
        if skill_dir is not None:
            for rel in self._store.list_files(name, include_disabled=True):
                try:
                    files[rel] = (skill_dir / rel).read_bytes()
                except OSError:
                    continue
        return {
            "content": content,
            "files": files,
            "disabled": self._store.is_disabled(name),
        }

    def _rollback(self, name: str, backup: dict[str, Any] | None) -> None:
        """Undo a partial install: restore the previous skill, or remove ours.

        Best-effort by nature — but leaving a half-written skill in place is
        strictly worse than a failed restore we logged.
        """
        try:
            if backup is None:
                self._store.delete_skill(name)
                return
            self._store.update_skill(name, backup["content"])
            skill_dir = self._store.find_skill_dir(name, include_disabled=True)
            if skill_dir is not None:
                # include_disabled here as well: without it a disabled skill
                # listed no files, so files the failed install had added were
                # left behind next to the restored ones.
                for rel in self._store.list_files(name, include_disabled=True):
                    if rel not in backup["files"]:
                        self._store.remove_file(name, rel)
            for rel, data in backup["files"].items():
                self._store.write_file_bytes(name, rel, data)
            # Restore the disable flag last. create_skill/update_skill do not
            # touch it, but an install that ran persist_enable would otherwise
            # leave a rolled-back skill enabled.
            if backup.get("disabled") and not self._store.is_disabled(name):
                self._store.persist_disable(name)
        except Exception as e:
            logger.error("Rollback of skill '{}' failed: {}", name, e)

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
            dep_failures: list[str] = []
            if run_install:
                meta = fm.get("metadata", {}) or {}
                echo_meta = meta.get("echo", {}) or {} if isinstance(meta, dict) else {}
                specs = echo_meta.get("install", []) if isinstance(echo_meta, dict) else []
                # Merge the two dependency dialects: requires.pip is the
                # documented one and what skill_view/skill_run precheck, while
                # metadata.echo.install is the older list-of-dicts form. Reading
                # only the latter meant most skills' declared pip deps were
                # never installed at install time.
                specs = _merge_install_specs(specs, echo_meta)
                if specs:
                    install_results, dep_failures = await _run_install_specs(specs)

            output = f"Skill '{skill_name}' installed successfully."
            if self._last_scan_warnings:
                # Surfaced, not swallowed: the user chose to install a skill whose
                # docs contain credential-reading or outbound-with-key commands.
                # Those still need exec/skill_run approval to actually run, but
                # they should know before that prompt arrives.
                output += (
                    "\n\n⚠ 该技能文档包含以下敏感操作模式,运行其脚本前请先审阅 SKILL.md:"
                    + "、".join(self._last_scan_warnings)
                )
                logger.warning(
                    "Skill '{}' installed with scan warnings: {}",
                    skill_name, self._last_scan_warnings,
                )
            if install_results:
                output += "\n\nInstall results:\n" + "\n".join(f"  {r}" for r in install_results)
            if dep_failures:
                # The skill is on disk and readable, but its scripts cannot run.
                # Reporting success=True here told the model everything was
                # fine, and the failure only surfaced later as an ImportError.
                output += (
                    "\n\nThe skill was installed but is not runnable yet: "
                    + "; ".join(dep_failures)
                )
                logger.warning(
                    "Skill '{}' installed with failed dependencies: {}",
                    skill_name, dep_failures,
                )
                return ToolResult(
                    success=False,
                    output=output,
                    error="dependency installation failed: " + "; ".join(dep_failures),
                    error_kind="business",
                )
            logger.info("Installed skill '{}' from {} ({})", skill_name, source, location)
            return ToolResult(success=True, output=output)

        except Exception as e:
            logger.error("Skill install failed: {}", e)
            return ToolResult(success=False, error=f"Install failed: {e}")
        finally:
            # Cleanup can walk a large cloned repo; keep it off the loop too.
            await asyncio.to_thread(shutil.rmtree, tmpdir, ignore_errors=True)
