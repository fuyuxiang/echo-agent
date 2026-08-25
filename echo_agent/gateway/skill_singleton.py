"""SkillManager singleton accessor — module-level lazy singleton.

The WS skill.* handlers and any future gateway-side caller share one
``SkillManager`` rooted at ``~/.echo-agent/skills`` (via
``runtime_paths.echo_home()``). It is created on first access, matching the
existing module-level pattern used by ``runtime_paths.echo_home()`` itself.

The test surface is narrow on purpose: production code calls
``get_skill_manager()``; tests inject via ``set_skill_manager_for_tests()`` or
``reset_skill_manager()``.
"""
from __future__ import annotations

from echo_agent.runtime_paths import echo_home
from echo_agent.skills.manager import SkillManager

_manager: SkillManager | None = None


def get_skill_manager() -> SkillManager:
    """返回进程级 SkillManager 单例,首次调用时按 echo_home() 路径创建。"""
    global _manager
    if _manager is None:
        skills_dir = echo_home() / "skills"
        _manager = SkillManager(skills_dir)
    return _manager


def set_skill_manager_for_tests(manager: SkillManager | None) -> None:
    """测试钩子:替换或清空(``None``)单例,避免污染 ~/.echo-agent。"""
    global _manager
    _manager = manager


def reset_skill_manager() -> None:
    """清空单例,使下一次 ``get_skill_manager()`` 重新创建。"""
    global _manager
    _manager = None