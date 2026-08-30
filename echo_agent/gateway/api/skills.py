from __future__ import annotations

import re
import shutil
import stat
import tempfile
import zipfile
from pathlib import Path
from typing import TYPE_CHECKING

from aiohttp import web

if TYPE_CHECKING:
    from echo_agent.gateway.server import GatewayServer

# A single safe path segment. Mirrors SkillStore's own name rule: no separators,
# no "..", no absolute paths — anything joined into a filesystem path must match.
_SAFE_SEGMENT_RE = re.compile(r"^[a-z0-9][a-z0-9._-]{0,63}$")
_MAX_ARCHIVE_BYTES = 10 * 1024 * 1024
_MAX_EXTRACTED_BYTES = 20 * 1024 * 1024
_MAX_ARCHIVE_ENTRIES = 500
_MAX_IMPORTED_FILE_BYTES = 1_048_576
_ALLOWED_IMPORT_DIRS = frozenset({"references", "templates", "scripts", "assets"})


class SkillsAPI:
    def __init__(self, server: GatewayServer):
        self._server = server

    def _store(self):
        return self._server._agent_loop.skill_store

    def _unavailable(self) -> web.Response | None:
        """503 when the skills system is off (skills.enabled=false).

        skill_store is None in that case, and every endpoint here dereferences
        it. Answering with a clear status beats an AttributeError surfacing as a
        500 with no explanation.
        """
        if self._store() is None:
            return web.json_response(
                {"error": "skills system is disabled (skills.enabled=false)"},
                status=503,
            )
        return None

    def _guard(self, request: web.Request, action: str) -> web.Response | None:
        return self._server._require_api_token(request, action=action)

    def _admin_guard(self, request: web.Request, action: str) -> web.Response | None:
        return self._server._require_admin_token(request, action=action)

    def _list_all_with_status(self) -> list[dict]:
        """Return all skills (including disabled) with their enabled status.

        Uses the store's public listing rather than re-walking the roots through
        private helpers, so admin listings stay consistent with what the agent
        resolves.
        """
        store = self._store()
        results: list[dict] = []
        for meta in store.list_all(include_disabled=True):
            d = meta.to_dict()
            d["enabled"] = not store.is_disabled(meta.name)
            results.append(d)
        results.sort(key=lambda m: m["name"])
        return results

    async def list_skills(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "skills_list")
        if guard is not None:
            return guard
        unavailable = self._unavailable()
        if unavailable is not None:
            return unavailable

        skills = self._list_all_with_status()
        return web.json_response({"skills": skills})

    async def get_skill(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "skills_get")
        if guard is not None:
            return guard
        unavailable = self._unavailable()
        if unavailable is not None:
            return unavailable

        name = request.match_info["name"]
        store = self._store()
        # include_disabled: this is the admin detail view, and reading a
        # disabled skill is exactly how an operator decides whether to enable
        # it. Reading is not running.
        content = store.read_skill(name, include_disabled=True)
        if content is None:
            return web.json_response({"error": "not found"}, status=404)

        files = store.list_files(name, include_disabled=True)
        return web.json_response(
            {
                "name": name,
                "content": content,
                "enabled": not store.is_disabled(name),
                "files": files,
            }
        )

    async def toggle_skill(self, request: web.Request) -> web.Response:
        # Enabling a skill changes what the agent can do on its next turn, which
        # is the same class of change as installing or deleting one — those
        # already required an admin token while this did not. Admin-guarded also
        # means CSRF-checked, closing the cross-site POST that could flip a
        # skill on a localhost gateway.
        guard = self._admin_guard(request, "skills_toggle")
        if guard is not None:
            return guard
        unavailable = self._unavailable()
        if unavailable is not None:
            return unavailable

        name = request.match_info["name"]
        store = self._store()

        # Refuse to toggle a name that does not exist on disk, so a typo does not
        # silently create a permanent disable entry for a phantom skill.
        if store.find_skill_dir(name, include_disabled=True) is None:
            return web.json_response({"error": f"skill '{name}' not found"}, status=404)

        if store.is_disabled(name):
            store.persist_enable(name)
            enabled = True
        else:
            store.persist_disable(name)
            enabled = False

        return web.json_response(
            {
                "success": True,
                "skill": {"name": name, "enabled": enabled},
            }
        )

    async def delete_skill(self, request: web.Request) -> web.Response:
        guard = self._admin_guard(request, "skills_delete")
        if guard is not None:
            return guard
        unavailable = self._unavailable()
        if unavailable is not None:
            return unavailable

        name = request.match_info["name"]
        store = self._store()
        # delete_skill now clears the disable entries itself (leaving them behind
        # poisoned the name for any future skill installed under it), so the
        # private-attribute cleanup that used to live here is gone.
        error = store.delete_skill(name)
        if error:
            return web.json_response({"error": error}, status=400)

        return web.json_response({"success": True})

    def _skill_pip_specs(self, name: str) -> list[str] | None:
        """Read a skill's declared pip deps from SKILL.md metadata.echo.requires.pip.
        Returns None if skill not found, [] if it declares none."""
        store = self._store()
        content = store.read_skill(name)
        if content is None:
            return None
        from echo_agent.skills.store import parse_frontmatter

        fm, _ = parse_frontmatter(content)
        echo_meta = (fm.get("metadata", {}) or {}).get("echo", {}) or {}
        requires = echo_meta.get("requires", {}) or {}
        return list(requires.get("pip", []) or [])

    async def get_skill_deps(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "skills_deps_get")
        if guard is not None:
            return guard
        unavailable = self._unavailable()
        if unavailable is not None:
            return unavailable
        name = request.match_info["name"]
        specs = self._skill_pip_specs(name)
        if specs is None:
            return web.json_response({"error": "not found"}, status=404)
        from echo_agent.dependencies.lazy_deps import _is_satisfied

        missing = [s for s in specs if not _is_satisfied(s)]
        return web.json_response(
            {
                "name": name,
                "requires": list(specs),
                "missing": missing,
                "satisfied": not missing,
            }
        )

    async def install_skill_deps(self, request: web.Request) -> web.Response:
        guard = self._admin_guard(request, "skills_deps_install")
        if guard is not None:
            return guard
        unavailable = self._unavailable()
        if unavailable is not None:
            return unavailable
        name = request.match_info["name"]
        specs = self._skill_pip_specs(name)
        if specs is None:
            return web.json_response({"error": "not found"}, status=404)
        from echo_agent.dependencies.lazy_deps import install_authorized_async

        result = await install_authorized_async(tuple(specs), source=f"http:skill:{name}")
        status = 200 if result.get("success") else 400
        return web.json_response(result, status=status)

    async def import_skill(self, request: web.Request) -> web.Response:
        guard = self._admin_guard(request, "skills_import")
        if guard is not None:
            return guard
        unavailable = self._unavailable()
        if unavailable is not None:
            return unavailable

        try:
            body = await request.json()
        except Exception:
            return web.json_response({"error": "invalid JSON body"}, status=400)

        path = body.get("path")
        if not path:
            return web.json_response({"error": "path is required"}, status=400)

        source = Path(path).expanduser()
        if not source.exists() or not (source / "SKILL.md").exists():
            return web.json_response({"error": f"no SKILL.md found at '{path}'"}, status=400)

        return self._import_source(source)

    def _validate_source_tree(self, source: Path) -> str | None:
        total = 0
        for item in source.rglob("*"):
            if item.is_symlink():
                return f"symlinks are not allowed: {item.relative_to(source)}"
            if not item.is_file():
                continue
            relative = item.relative_to(source)
            if relative != Path("SKILL.md") and (
                len(relative.parts) < 2 or relative.parts[0] not in _ALLOWED_IMPORT_DIRS
            ):
                return f"unsupported file location: {relative}"
            size = item.stat().st_size
            if size > _MAX_IMPORTED_FILE_BYTES:
                return f"file exceeds 1 MiB limit: {relative}"
            total += size
            if total > _MAX_EXTRACTED_BYTES:
                return "skill exceeds 20 MiB extracted-size limit"
        return None

    def _import_source(self, source: Path) -> web.Response:
        tree_error = self._validate_source_tree(source)
        if tree_error:
            return web.json_response({"error": tree_error}, status=400)

        store = self._store()
        meta = store._read_meta(source)
        if not meta:
            return web.json_response({"error": "failed to parse SKILL.md"}, status=400)

        # name and category come straight out of the imported SKILL.md, i.e.
        # from a file this endpoint does not control. Unvalidated they were
        # joined into a filesystem path: `name: /tmp/x` or `name: ../../x`
        # relocated the write outside user_dir entirely.
        if not _SAFE_SEGMENT_RE.match(meta.name):
            return web.json_response({"error": f"invalid skill name in SKILL.md: '{meta.name}'"}, status=400)
        category = meta.category or "general"
        if not _SAFE_SEGMENT_RE.match(category):
            return web.json_response({"error": f"invalid category in SKILL.md: '{category}'"}, status=400)

        user_dir = store.user_dir
        target = user_dir / category / meta.name
        # Belt-and-braces after the segment checks: confirm the resolved target
        # is still inside user_dir before creating anything.
        try:
            target.resolve().relative_to(user_dir.resolve())
        except ValueError:
            return web.json_response({"error": "resolved skill path escapes the skill directory"}, status=400)
        if target.exists():
            return web.json_response({"error": f"skill '{meta.name}' already exists"}, status=409)
        # Importing a skill the operator has not reviewed should not also import
        # whatever the source tree happens to contain, and symlinks would copy
        # host files in by reference.
        target.mkdir(parents=True, exist_ok=True)
        try:
            shutil.copytree(
                source,
                target,
                dirs_exist_ok=True,
                symlinks=False,
                ignore=shutil.ignore_patterns(".git", "__pycache__", ".venv", "venv"),
            )
        except Exception as e:
            shutil.rmtree(target, ignore_errors=True)
            return web.json_response({"error": f"import failed: {e}"}, status=400)

        store.persist_disable(meta.name)
        d = meta.to_dict()
        d["enabled"] = False
        return web.json_response({"success": True, "skill": d})

    async def upload_skill(self, request: web.Request) -> web.Response:
        """Import a browser-uploaded ZIP safely; imported skills start disabled."""
        guard = self._admin_guard(request, "skills_upload")
        if guard is not None:
            return guard
        unavailable = self._unavailable()
        if unavailable is not None:
            return unavailable
        try:
            reader = await request.multipart()
        except Exception:
            return web.json_response({"error": "multipart form data required"}, status=400)
        part = await reader.next()
        while part is not None and part.name != "file":
            part = await reader.next()
        if part is None or not (part.filename or "").lower().endswith(".zip"):
            return web.json_response({"error": "a .zip file is required"}, status=400)

        with tempfile.TemporaryDirectory(prefix="echo-skill-upload-") as temporary:
            root = Path(temporary)
            archive = root / "upload.zip"
            size = 0
            with archive.open("wb") as handle:
                while True:
                    chunk = await part.read_chunk(size=64 * 1024)
                    if not chunk:
                        break
                    size += len(chunk)
                    if size > _MAX_ARCHIVE_BYTES:
                        return web.json_response(
                            {"error": "archive exceeds 10 MiB limit"},
                            status=413,
                        )
                    handle.write(chunk)
            try:
                with zipfile.ZipFile(archive) as bundle:
                    entries = [entry for entry in bundle.infolist() if not entry.is_dir()]
                    if len(entries) > _MAX_ARCHIVE_ENTRIES:
                        return web.json_response({"error": "archive has too many files"}, status=400)
                    if sum(entry.file_size for entry in entries) > _MAX_EXTRACTED_BYTES:
                        return web.json_response(
                            {"error": "archive exceeds 20 MiB extracted-size limit"},
                            status=400,
                        )
                    extract_root = root / "extracted"
                    extract_root.mkdir()
                    for entry in entries:
                        relative = Path(entry.filename)
                        if relative.is_absolute() or ".." in relative.parts:
                            return web.json_response({"error": "archive contains an unsafe path"}, status=400)
                        mode = entry.external_attr >> 16
                        if stat.S_ISLNK(mode):
                            return web.json_response({"error": "archive contains a symlink"}, status=400)
                        target = extract_root / relative
                        target.parent.mkdir(parents=True, exist_ok=True)
                        with bundle.open(entry) as source_handle, target.open("wb") as target_handle:
                            shutil.copyfileobj(source_handle, target_handle)
            except (zipfile.BadZipFile, OSError) as exc:
                return web.json_response({"error": f"invalid ZIP archive: {exc}"}, status=400)

            skill_files = [path for path in extract_root.rglob("SKILL.md") if "__MACOSX" not in path.parts]
            if len(skill_files) != 1:
                return web.json_response(
                    {"error": "archive must contain exactly one SKILL.md"},
                    status=400,
                )
            return self._import_source(skill_files[0].parent)
