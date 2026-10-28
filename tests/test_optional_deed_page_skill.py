"""The optional deed.page skill stays out of the default inventory and imports as instructions only."""

from pathlib import Path
from unittest.mock import AsyncMock

import pytest

from echo_agent.agent.context import build_skills_context
from echo_agent.agent.tools import skill_install
from echo_agent.agent.tools.skills import SkillViewTool
from echo_agent.skills.store import SkillStore

ROOT = Path(__file__).resolve().parents[1]
SKILL_DIR = ROOT / "optional-skills" / "deed-page"


def test_default_inventory_does_not_include_optional_deed_page(tmp_path):
    store = SkillStore(user_dir=tmp_path / "skills", builtin_dir=ROOT / "skills")
    assert len(store.list_all()) >= 35
    assert "deed-page" not in {skill.name for skill in store.list_all()}
    assert "deed.page" not in build_skills_context(store).lower()
    assert store.read_skill("deed-page") is None


def test_skill_requires_authorization_and_masks_the_token():
    text = (SKILL_DIR / "SKILL.md").read_text(encoding="utf-8")
    assert "explicitly asks to publish with deed.page" in text
    assert "get authorization for that specific upload" in text
    # The publish command saves a new owner token privately and masks it in output.
    assert "umask 077" in text
    assert '"token":"<saved>"' in text
    assert "X-Ttl: 604800" in text


@pytest.mark.asyncio
async def test_explicit_install_and_view_only_import_instructions(tmp_path, monkeypatch):
    run = AsyncMock(side_effect=AssertionError("optional instruction import must not run a process"))
    install_deps = AsyncMock(side_effect=AssertionError("optional instruction import must not install dependencies"))
    monkeypatch.setattr(skill_install, "_run", run)
    monkeypatch.setattr(skill_install, "install_authorized_async", install_deps)
    store = SkillStore(user_dir=tmp_path / "skills", builtin_dir=ROOT / "skills")
    tool = skill_install.SkillInstallTool(store)
    result = await tool.execute({"source": "local", "location": str(SKILL_DIR)}, None)
    assert result.success, result.error
    assert "deed-page" in {skill.name for skill in store.list_all()}
    view = await SkillViewTool(store).execute({"name": "deed-page"}, None)
    assert view.success, view.error
    assert "https://deed.page/v1/sites" in view.output
    run.assert_not_called()
    install_deps.assert_not_called()
    assert not (tmp_path / ".config").exists()
