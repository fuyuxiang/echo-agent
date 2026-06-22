"""Contract tests for SkillInstallTool — helpers and execute flow (subprocess mocked)."""

from __future__ import annotations

from pathlib import Path
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from echo_agent.agent.tools import skill_install
from echo_agent.agent.tools.skill_install import (
    SkillInstallTool,
    _SAFE_NAME_RE,
    _SAFE_PIP_PKG,
    _SAFE_BREW_FORMULA,
    _GIT_URL_RE,
    _fetch_git,
    _fetch_local,
    _fetch_url,
    _find_skill_md,
    _resolve_name,
    _run,
    _run_install_specs,
)


def _fake_proc(returncode=0, stdout=b"", stderr=b""):
    proc = MagicMock()
    proc.returncode = returncode
    proc.communicate = AsyncMock(return_value=(stdout, stderr))
    proc.wait = AsyncMock()
    proc.kill = MagicMock()
    return proc


class TestRunHelper:
    @pytest.mark.asyncio
    async def test_run_success(self):
        with patch("asyncio.create_subprocess_exec",
                   AsyncMock(return_value=_fake_proc(0, b"hi", b""))):
            code, out, err = await _run(["echo", "hi"])
        assert code == 0
        assert out == "hi"

    @pytest.mark.asyncio
    async def test_run_timeout(self):
        proc = _fake_proc()
        with patch("asyncio.create_subprocess_exec", AsyncMock(return_value=proc)), \
             patch("asyncio.wait_for", AsyncMock(side_effect=__import__("asyncio").TimeoutError())):
            code, out, err = await _run(["sleep", "100"], timeout=1)
        assert code == -1
        assert "timed out" in err
        proc.kill.assert_called_once()


class TestFetchGit:
    @pytest.mark.asyncio
    async def test_invalid_url(self, tmp_path):
        dest, err = await _fetch_git("not-a-git-url", str(tmp_path))
        assert dest is None
        assert "Invalid git URL" in err

    @pytest.mark.asyncio
    async def test_clone_failure(self, tmp_path):
        with patch("echo_agent.agent.tools.skill_install._run",
                   AsyncMock(return_value=(1, "", "auth denied"))):
            dest, err = await _fetch_git("https://github.com/x/y", str(tmp_path))
        assert dest is None
        assert "git clone failed" in err

    @pytest.mark.asyncio
    async def test_clone_success(self, tmp_path):
        with patch("echo_agent.agent.tools.skill_install._run",
                   AsyncMock(return_value=(0, "", ""))):
            dest, err = await _fetch_git("https://github.com/x/y", str(tmp_path))
        assert err == ""
        assert dest.endswith("repo")


class TestFetchUrl:
    @pytest.mark.asyncio
    async def test_invalid_url(self, tmp_path):
        dest, err = await _fetch_url("ftp://x", str(tmp_path))
        assert dest is None
        assert "Invalid URL" in err

    @pytest.mark.asyncio
    async def test_download_failure(self, tmp_path):
        with patch("echo_agent.agent.tools.skill_install._run",
                   AsyncMock(return_value=(1, "", "404"))):
            dest, err = await _fetch_url("https://x/a.tar", str(tmp_path))
        assert dest is None
        assert "Download failed" in err

    @pytest.mark.asyncio
    async def test_tar_extract_success(self, tmp_path):
        with patch("echo_agent.agent.tools.skill_install._run",
                   AsyncMock(return_value=(0, "", ""))):
            dest, err = await _fetch_url("https://x/a.tar.gz", str(tmp_path))
        assert err == ""
        assert dest.endswith("download")

    @pytest.mark.asyncio
    async def test_bad_zip(self, tmp_path):
        # First _run (curl) succeeds; the archive file is not a real zip.
        async def fake_run(cmd, cwd=None, timeout=60):
            (Path(tmp_path) / "archive").write_bytes(b"not a zip")
            return 0, "", ""

        with patch("echo_agent.agent.tools.skill_install._run", fake_run):
            dest, err = await _fetch_url("https://x/a.zip", str(tmp_path))
        assert dest is None
        assert "not a valid zip" in err


SKILL_MD = """---
name: my-skill
category: util
---
# My Skill
Body.
"""


class TestSafeRegexes:
    def test_git_url_valid(self):
        assert _GIT_URL_RE.match("https://github.com/foo/bar")
        assert _GIT_URL_RE.match("git@gitlab.com:foo/bar.git")

    def test_git_url_invalid(self):
        assert not _GIT_URL_RE.match("ftp://example.com/x")

    def test_safe_name(self):
        assert _SAFE_NAME_RE.match("my-skill_1.0")
        assert not _SAFE_NAME_RE.match("Bad Name")
        assert not _SAFE_NAME_RE.match("../escape")

    def test_safe_pip_pkg(self):
        assert _SAFE_PIP_PKG.match("requests")
        assert _SAFE_PIP_PKG.match("requests>=2.0")
        assert _SAFE_PIP_PKG.match("pkg[extra]==1.2")
        assert not _SAFE_PIP_PKG.match("requests; rm -rf /")

    def test_safe_brew_formula(self):
        assert _SAFE_BREW_FORMULA.match("wget")
        assert _SAFE_BREW_FORMULA.match("user/tap/formula")
        assert not _SAFE_BREW_FORMULA.match("evil; rm")


class TestFetchLocal:
    def test_missing_path(self, tmp_path):
        dest, err = _fetch_local(str(tmp_path / "nope"))
        assert dest is None
        assert "not found" in err.lower()

    def test_not_a_directory(self, tmp_path):
        f = tmp_path / "file.txt"
        f.write_text("x")
        dest, err = _fetch_local(str(f))
        assert dest is None
        assert "Not a directory" in err

    def test_valid_directory(self, tmp_path):
        dest, err = _fetch_local(str(tmp_path))
        assert err == ""
        assert dest == str(tmp_path.resolve())


class TestFindSkillMd:
    def test_direct_skill_md(self, tmp_path):
        (tmp_path / "SKILL.md").write_text(SKILL_MD)
        root, err = _find_skill_md(str(tmp_path), "")
        assert err == ""
        assert root == tmp_path

    def test_nested_skill_md(self, tmp_path):
        sub = tmp_path / "pkg" / "inner"
        sub.mkdir(parents=True)
        (sub / "SKILL.md").write_text(SKILL_MD)
        root, err = _find_skill_md(str(tmp_path), "")
        assert err == ""
        assert root == sub

    def test_missing_subdirectory(self, tmp_path):
        root, err = _find_skill_md(str(tmp_path), "nope")
        assert root is None
        assert "Subdirectory not found" in err

    def test_no_skill_md(self, tmp_path):
        root, err = _find_skill_md(str(tmp_path), "")
        assert root is None
        assert "No SKILL.md" in err


class TestResolveName:
    def test_override_valid(self, tmp_path):
        name, err = _resolve_name(tmp_path, "override-name")
        assert err == ""
        assert name == "override-name"

    def test_override_invalid(self, tmp_path):
        name, err = _resolve_name(tmp_path, "Bad Name!")
        assert name == ""
        assert "Invalid skill name" in err

    def test_from_frontmatter(self, tmp_path):
        (tmp_path / "SKILL.md").write_text(SKILL_MD)
        name, err = _resolve_name(tmp_path, "")
        assert err == ""
        assert name == "my-skill"

    def test_invalid_frontmatter_name(self, tmp_path):
        (tmp_path / "SKILL.md").write_text("---\nname: Bad Name\n---\nbody")
        name, err = _resolve_name(tmp_path, "")
        assert name == ""
        assert "invalid" in err.lower()


class TestRunInstallSpecs:
    @pytest.mark.asyncio
    async def test_pip_unsafe_skipped(self):
        results = await _run_install_specs([{"kind": "pip", "package": "x; rm -rf /"}])
        assert any("skipped unsafe" in r for r in results)

    @pytest.mark.asyncio
    async def test_pip_routes_to_install_authorized(self, monkeypatch):
        calls = []

        def fake_install_authorized(specs, *, source):
            calls.append((specs, source))
            return {"success": True, "detail": "ok"}

        monkeypatch.setattr(skill_install, "install_authorized", fake_install_authorized)
        # _run must NOT be used for pip anymore
        run_mock = AsyncMock(return_value=(0, "out", ""))
        monkeypatch.setattr(skill_install, "_run", run_mock)

        results = await _run_install_specs([{"kind": "pip", "package": "requests"}])

        assert len(calls) == 1
        specs, source = calls[0]
        assert "requests" in specs
        assert source == "tool:skill_install:requests"
        assert any("ok" in r for r in results)
        run_mock.assert_not_called()

    @pytest.mark.asyncio
    async def test_pip_failure_reports_detail(self, monkeypatch):
        def fake_install_authorized(specs, *, source):
            return {"success": False, "detail": "boom"}

        monkeypatch.setattr(skill_install, "install_authorized", fake_install_authorized)
        results = await _run_install_specs([{"kind": "pip", "package": "requests"}])
        assert any("boom" in r for r in results)

    @pytest.mark.asyncio
    async def test_brew_still_uses_run(self, monkeypatch):
        run_mock = AsyncMock(return_value=(0, "out", ""))
        ia_mock = MagicMock()
        monkeypatch.setattr(skill_install, "_run", run_mock)
        monkeypatch.setattr(skill_install, "install_authorized", ia_mock)
        results = await _run_install_specs([{"kind": "brew", "formula": "wget"}])
        run_mock.assert_called_once()
        ia_mock.assert_not_called()
        assert any("ok" in r for r in results)

    @pytest.mark.asyncio
    async def test_shell_does_not_install(self, monkeypatch):
        ia_mock = MagicMock()
        monkeypatch.setattr(skill_install, "install_authorized", ia_mock)
        results = await _run_install_specs([{"kind": "shell", "command": "echo hi"}])
        ia_mock.assert_not_called()
        assert any("skipped for safety" in r for r in results)

    @pytest.mark.asyncio
    async def test_brew_unsafe_skipped(self):
        results = await _run_install_specs([{"kind": "brew", "formula": "x;y"}])
        assert any("skipped unsafe" in r for r in results)

    @pytest.mark.asyncio
    async def test_shell_skipped_for_safety(self):
        results = await _run_install_specs([{"kind": "shell", "command": "echo hi"}])
        assert any("skipped for safety" in r for r in results)

    @pytest.mark.asyncio
    async def test_unknown_kind(self):
        results = await _run_install_specs([{"kind": "wat"}])
        assert any("unknown install kind" in r for r in results)


class TestSkillInstallExecute:
    def _make(self):
        store = MagicMock()
        store.create_skill.return_value = None
        return SkillInstallTool(store=store), store

    @pytest.mark.asyncio
    async def test_unknown_source(self):
        tool, _ = self._make()
        result = await tool.execute({"source": "ftp", "location": "x"}, None)
        assert result.success is False
        assert "Unknown source" in result.error

    @pytest.mark.asyncio
    async def test_local_install_success(self, tmp_path):
        skill_src = tmp_path / "src"
        skill_src.mkdir()
        (skill_src / "SKILL.md").write_text(SKILL_MD)
        tool, store = self._make()
        result = await tool.execute(
            {"source": "local", "location": str(skill_src), "run_install": False},
            None,
        )
        assert result.success is True
        assert "installed successfully" in result.output
        store.create_skill.assert_called_once()

    @pytest.mark.asyncio
    async def test_local_fetch_error_propagated(self, tmp_path):
        tool, _ = self._make()
        result = await tool.execute(
            {"source": "local", "location": str(tmp_path / "missing")}, None
        )
        assert result.success is False
        assert "not found" in result.error.lower()

    @pytest.mark.asyncio
    async def test_existing_skill_updates(self, tmp_path):
        skill_src = tmp_path / "src"
        skill_src.mkdir()
        (skill_src / "SKILL.md").write_text(SKILL_MD)
        tool, store = self._make()
        store.create_skill.return_value = "skill 'my-skill' already exists"
        store.update_skill.return_value = None
        result = await tool.execute(
            {"source": "local", "location": str(skill_src), "run_install": False},
            None,
        )
        assert result.success is True
        store.update_skill.assert_called_once()

    @pytest.mark.asyncio
    async def test_create_error_returned(self, tmp_path):
        skill_src = tmp_path / "src"
        skill_src.mkdir()
        (skill_src / "SKILL.md").write_text(SKILL_MD)
        tool, store = self._make()
        store.create_skill.return_value = "disk full"
        result = await tool.execute(
            {"source": "local", "location": str(skill_src), "run_install": False},
            None,
        )
        assert result.success is False
        assert "disk full" in result.error

    @pytest.mark.asyncio
    async def test_git_source_uses_fetch_git(self, tmp_path):
        skill_src = tmp_path / "repo"
        skill_src.mkdir()
        (skill_src / "SKILL.md").write_text(SKILL_MD)
        tool, store = self._make()

        async def fake_fetch_git(location, tmpdir):
            return str(skill_src), ""

        with patch.object(skill_install, "_fetch_git", fake_fetch_git):
            result = await tool.execute(
                {"source": "git", "location": "https://github.com/x/y",
                 "run_install": False},
                None,
            )
        assert result.success is True
