"""Tests for transactional skill installs and binary asset support.

The installer used to create the skill, then copy support files one by one with
``read_text``. Any binary asset (PNG, font) raised UnicodeDecodeError partway
through, and nothing rolled back — leaving a half-installed skill that showed up
in skills_list but could not run. Same for the file-count limit: "error, but 2000
files already copied".

Also covered: name overrides now rewrite the frontmatter (a skill installed as
``my-alias`` used to still be *listed* as ``original-name``, answer to both, and
collide with a real ``original-name``), and subdirectory containment.
"""

from __future__ import annotations

import pytest

from echo_agent.agent.tools.skill_install import (
    SkillInstallTool,
    _find_skill_md,
    _plan_support_files,
)
from echo_agent.skills.store import SkillStore

SKILL_MD = """---
name: demo-skill
description: a demo
---
# Demo
body
"""


@pytest.fixture
def store(tmp_path):
    return SkillStore(user_dir=tmp_path / "user")


@pytest.fixture
def source(tmp_path):
    src = tmp_path / "src"
    (src / "scripts").mkdir(parents=True)
    (src / "SKILL.md").write_text(SKILL_MD, encoding="utf-8")
    (src / "scripts" / "run.py").write_text("print('hi')\n", encoding="utf-8")
    return src


class TestBinaryAssets:
    @pytest.mark.asyncio
    async def test_install_with_png_succeeds(self, store, source):
        """The case that used to fail outright and leave a partial install."""
        (source / "assets").mkdir()
        png = b"\x89PNG\r\n\x1a\n" + b"\x00" * 64
        (source / "assets" / "logo.png").write_bytes(png)

        tool = SkillInstallTool(store=store)
        result = await tool.execute(
            {"source": "local", "location": str(source), "run_install": False}, None,
        )
        assert result.success is True, result.error
        installed = store.find_skill_dir("demo-skill")
        assert (installed / "assets" / "logo.png").read_bytes() == png
        assert (installed / "scripts" / "run.py").exists()

    def test_plan_classifies_text_and_binary(self, source):
        (source / "assets").mkdir()
        (source / "assets" / "logo.png").write_bytes(b"\x89PNG\xff\xfe")
        plan, err = _plan_support_files(source)
        assert err == ""
        kinds = {rel: type(data) for rel, data in plan}
        assert kinds["scripts/run.py"] is str
        assert kinds["assets/logo.png"] is bytes

    def test_plan_rejects_symlink_escape(self, source, tmp_path):
        """A symlink out of the source tree would copy arbitrary host files in."""
        outside = tmp_path / "outside.txt"
        outside.write_text("host secret", encoding="utf-8")
        (source / "scripts" / "link.py").symlink_to(outside)
        plan, err = _plan_support_files(source)
        assert plan == []
        assert "outside" in err


class TestRollback:
    @pytest.mark.asyncio
    async def test_failed_copy_leaves_nothing_behind(self, store, source, monkeypatch):
        """A new install that fails mid-copy must not leave a partial skill."""
        import echo_agent.agent.tools.skill_install as mod

        real = store.write_file
        calls = {"n": 0}

        def flaky(name, rel, content):
            calls["n"] += 1
            if calls["n"] > 0 and rel.endswith("run.py"):
                return "disk full"
            return real(name, rel, content)

        monkeypatch.setattr(store, "write_file", flaky)

        tool = SkillInstallTool(store=store)
        result = await tool.execute(
            {"source": "local", "location": str(source), "run_install": False}, None,
        )
        assert result.success is False
        assert "rolled back" in result.error
        # Nothing left in the store, not even the SKILL.md that was written first.
        assert store.find_skill_dir("demo-skill", include_disabled=True) is None

    @pytest.mark.asyncio
    async def test_failed_upgrade_restores_previous_version(self, store, source, monkeypatch):
        """An upgrade that fails must leave the working version in place."""
        tool = SkillInstallTool(store=store)
        first = await tool.execute(
            {"source": "local", "location": str(source), "run_install": False}, None,
        )
        assert first.success is True
        assert "body" in (store.read_skill("demo-skill") or "")

        # Second install: same name, new content, but the copy fails.
        (source / "SKILL.md").write_text(
            SKILL_MD.replace("body", "NEW BODY"), encoding="utf-8",
        )
        real = store.write_file
        monkeypatch.setattr(
            store, "write_file",
            lambda n, rel, c: "boom" if rel.endswith("run.py") else real(n, rel, c),
        )
        second = await tool.execute(
            {"source": "local", "location": str(source), "run_install": False}, None,
        )
        assert second.success is False
        monkeypatch.undo()
        # Original content restored, not the half-applied new one.
        content = store.read_skill("demo-skill") or ""
        assert "NEW BODY" not in content
        assert "body" in content


class TestNameOverride:
    @pytest.mark.asyncio
    async def test_override_rewrites_frontmatter(self, store, source):
        """One skill, one name: the override must reach the frontmatter, since
        _read_meta prefers it and listings would otherwise show the old name."""
        tool = SkillInstallTool(store=store)
        result = await tool.execute(
            {"source": "local", "location": str(source), "name": "my-alias",
             "run_install": False},
            None,
        )
        assert result.success is True, result.error
        assert [m.name for m in store.list_all()] == ["my-alias"]
        assert "name: my-alias" in (store.read_skill("my-alias") or "")

    @pytest.mark.asyncio
    async def test_original_name_no_longer_resolves(self, store, source):
        """The old dual-identity bug: both names used to find the skill."""
        tool = SkillInstallTool(store=store)
        await tool.execute(
            {"source": "local", "location": str(source), "name": "my-alias",
             "run_install": False},
            None,
        )
        assert store.find_skill_dir("demo-skill") is None
        assert store.find_skill_dir("my-alias") is not None


class TestSubdirectoryContainment:
    def test_relative_escape_rejected(self, tmp_path):
        base = tmp_path / "base"
        (base / "inner").mkdir(parents=True)
        (tmp_path / "outside").mkdir()
        (tmp_path / "outside" / "SKILL.md").write_text(SKILL_MD, encoding="utf-8")

        root, err = _find_skill_md(str(base), "../outside")
        assert root is None
        assert "escapes" in err

    def test_absolute_subdirectory_rejected(self, tmp_path):
        base = tmp_path / "base"
        base.mkdir()
        root, err = _find_skill_md(str(base), "/etc")
        assert root is None
        assert "relative" in err

    def test_legitimate_subdirectory_still_works(self, tmp_path):
        base = tmp_path / "base"
        sub = base / "packages" / "my-skill"
        sub.mkdir(parents=True)
        (sub / "SKILL.md").write_text(SKILL_MD, encoding="utf-8")
        root, err = _find_skill_md(str(base), "packages/my-skill")
        assert err == ""
        assert root == sub


class TestDependencyFailureSurfaces:
    @pytest.mark.asyncio
    async def test_failed_pip_makes_install_unsuccessful(self, store, source, monkeypatch):
        """The skill is on disk but unrunnable; reporting success hid that until
        the script later died on ImportError."""
        import echo_agent.agent.tools.skill_install as mod

        (source / "SKILL.md").write_text(
            "---\nname: demo-skill\ndescription: d\nmetadata:\n  echo:\n"
            "    requires:\n      pip:\n        - nonexistent-pkg-xyz\n---\nbody\n",
            encoding="utf-8",
        )

        async def failing(specs, *, source=""):
            return {"success": False, "detail": "no matching distribution"}

        monkeypatch.setattr(mod, "install_authorized_async", failing)
        monkeypatch.setattr(mod, "_is_satisfied", lambda s: False, raising=False)

        tool = SkillInstallTool(store=store)
        result = await tool.execute(
            {"source": "local", "location": str(source), "run_install": True}, None,
        )
        assert result.success is False
        assert "dependency" in result.error
        # Still installed and inspectable — the user can fix deps and retry.
        assert store.find_skill_dir("demo-skill") is not None

    @pytest.mark.asyncio
    async def test_requires_pip_is_honored_not_just_install_specs(self, store, source, monkeypatch):
        """requires.pip is the documented dialect and what skill_run prechecks,
        but the installer only read metadata.echo.install."""
        import echo_agent.agent.tools.skill_install as mod

        (source / "SKILL.md").write_text(
            "---\nname: demo-skill\ndescription: d\nmetadata:\n  echo:\n"
            "    requires:\n      pip:\n        - somepkg>=1.0\n---\nbody\n",
            encoding="utf-8",
        )
        seen = []

        async def record(specs, *, source=""):
            seen.append(tuple(specs))
            return {"success": True, "detail": "ok", "installed": list(specs)}

        monkeypatch.setattr(mod, "install_authorized_async", record)

        tool = SkillInstallTool(store=store)
        result = await tool.execute(
            {"source": "local", "location": str(source), "run_install": True}, None,
        )
        assert result.success is True, result.error
        assert seen and "somepkg>=1.0" in seen[0]
