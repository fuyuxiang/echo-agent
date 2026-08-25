"""Tests for skill-related WS handlers."""
from __future__ import annotations

from pathlib import Path

import pytest

from echo_agent.gateway.ws_skill import (
    handle_skill_disable,
    handle_skill_enable,
    handle_skill_list,
)
from echo_agent.skills.manager import SkillManager, SkillManifest, SkillStatus


@pytest.fixture
def skills_dir(tmp_path: Path) -> Path:
    return tmp_path / "skills"


@pytest.fixture
def manager(skills_dir: Path) -> SkillManager:
    return SkillManager(skills_dir)


def _make_skill(manager: SkillManager, name: str, status: SkillStatus = SkillStatus.INSTALLED) -> None:
    """向 manager 注入一个测试 skill。"""
    manifest = SkillManifest(name=name, version="1.0.0", description=f"desc-{name}")
    manager._skills[name] = type(manager._skills)()  # noqa: SLF001 — 测试内部状态
    from echo_agent.skills.manager import InstalledSkill
    # 真实 enable/disable 会写 <skills_dir>/<name>/.status,这里必须先建好目录
    (manager._skills_dir / name).mkdir(parents=True, exist_ok=True)
    manager._skills[name] = InstalledSkill(manifest=manifest, status=status, path=str(manager._skills_dir / name))  # noqa: SLF001


def test_handle_skill_list_returns_empty_when_no_skills(manager: SkillManager) -> None:
    """无 skill 时返回空数组。"""
    import asyncio
    result = asyncio.run(handle_skill_list(manager))
    assert result["type"] == "skill.list_result"
    assert result["skills"] == []


def test_handle_skill_list_returns_all_skills(manager: SkillManager) -> None:
    """返回已安装 skill 列表。"""
    _make_skill(manager, "ppt-author", SkillStatus.ENABLED)
    _make_skill(manager, "summarize", SkillStatus.DISABLED)
    import asyncio
    result = asyncio.run(handle_skill_list(manager))
    names = sorted(s["name"] for s in result["skills"])
    assert names == ["ppt-author", "summarize"]


def test_handle_skill_list_includes_status(manager: SkillManager) -> None:
    """每条 skill 携带 status 字段。"""
    _make_skill(manager, "ppt-author", SkillStatus.ENABLED)
    import asyncio
    result = asyncio.run(handle_skill_list(manager))
    assert result["skills"][0]["status"] == "enabled"
    assert result["skills"][0]["name"] == "ppt-author"
    assert result["skills"][0]["version"] == "1.0.0"
    assert result["skills"][0]["description"] == "desc-ppt-author"


def test_handle_skill_enable_success(manager: SkillManager) -> None:
    """启用成功返回 None(走 accepted ack)。"""
    _make_skill(manager, "ppt-author", SkillStatus.INSTALLED)
    import asyncio
    assert asyncio.run(handle_skill_enable(manager, "ppt-author")) is None
    assert manager.get_skill("ppt-author").status == SkillStatus.ENABLED


def test_handle_skill_enable_unknown_returns_error(manager: SkillManager) -> None:
    """启用不存在的 skill 返回 error 帧。"""
    import asyncio
    result = asyncio.run(handle_skill_enable(manager, "ghost"))
    assert result is not None
    assert result["type"] == "error"
    assert "ghost" in result["message"]


def test_handle_skill_disable_success(manager: SkillManager) -> None:
    """禁用成功。"""
    _make_skill(manager, "ppt-author", SkillStatus.ENABLED)
    import asyncio
    assert asyncio.run(handle_skill_disable(manager, "ppt-author")) is None
    assert manager.get_skill("ppt-author").status == SkillStatus.DISABLED


def test_handle_skill_disable_unknown_returns_error(manager: SkillManager) -> None:
    """禁用不存在的 skill 返回 error。"""
    import asyncio
    result = asyncio.run(handle_skill_disable(manager, "ghost"))
    assert result is not None
    assert result["type"] == "error"