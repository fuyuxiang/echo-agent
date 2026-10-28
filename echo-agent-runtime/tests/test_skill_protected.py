"""SkillStore.is_protected 来源判定。"""
from __future__ import annotations

from pathlib import Path

from echo_agent.skills.store import SkillStore


def _write_skill(root: Path, name: str) -> None:
    d = root / name
    d.mkdir(parents=True)
    (d / "SKILL.md").write_text(
        f"---\nname: {name}\ndescription: t\n---\n\nbody\n", encoding="utf-8"
    )


def test_user_dir_skill_not_protected(tmp_path: Path):
    user_dir = tmp_path / "user"
    user_dir.mkdir()
    _write_skill(user_dir, "evolved")
    store = SkillStore(user_dir=user_dir)
    assert store.is_protected("evolved") is False


def test_builtin_skill_is_protected(tmp_path: Path):
    user_dir = tmp_path / "user"
    user_dir.mkdir()
    builtin = tmp_path / "builtin"
    builtin.mkdir()
    _write_skill(builtin, "calculator")
    store = SkillStore(user_dir=user_dir, builtin_dir=builtin)
    assert store.is_protected("calculator") is True


def test_unknown_skill_not_protected(tmp_path: Path):
    user_dir = tmp_path / "user"
    user_dir.mkdir()
    store = SkillStore(user_dir=user_dir)
    assert store.is_protected("does-not-exist") is False
