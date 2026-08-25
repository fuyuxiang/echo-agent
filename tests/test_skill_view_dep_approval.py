"""Tests for SkillViewTool's missing-dependency reporting.

skill_view used to *install* the pip deps a SKILL.md declared — silently on a
trusted CLI, behind an approval prompt elsewhere. That made a tool declaring
``risk_level="read_only"`` (and exposed in every profile, public_gateway
included) into a route to running pip; and because install_authorized
deliberately bypasses the SKILL_DEPS allowlist, ``skills.allow_lazy_installs``
and ``ECHO_AGENT_DISABLE_LAZY_INSTALLS``, an externally authored skill naming a
hostile package got it built (pip runs build code) merely by being looked at.

Now it reports and installs nothing; skill_run owns the install, gated as EXEC.
These tests pin that: the useful notice survives, the install never fires.
"""

from __future__ import annotations

from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.agent.tools.base import ToolExecutionContext
from echo_agent.agent.tools.skills import SkillViewTool


def _config(profile: str = "personal_cli", cli_auto_approve: bool = True,
            trusted_channels: list[str] | None = None) -> SimpleNamespace:
    return SimpleNamespace(
        security=SimpleNamespace(profile=profile),
        permissions=SimpleNamespace(
            approval=SimpleNamespace(
                cli_auto_approve=cli_auto_approve,
                trusted_channels=trusted_channels or [],
            )
        ),
    )


SKILL_MD = """---
name: demo
metadata:
  echo:
    requires:
      pip:
        - openpyxl>=3.1
---
# Demo skill
body text
"""

SKILL_MD_WITH_ENV = """---
name: demo
metadata:
  echo:
    requires:
      env:
        - DEMO_SECRET_TOKEN
---
# Demo skill
body text
"""


def _store(content: str = SKILL_MD, files: list[str] | None = None):
    store = MagicMock()
    store.read_skill.return_value = content
    store.list_files.return_value = files or []
    store.read_file.return_value = "file content"
    store.is_disabled.return_value = False
    return store


def _ctx(channel: str = "weixin") -> ToolExecutionContext:
    return ToolExecutionContext(
        user_id="u1",
        channel=channel,
        chat_id="c1",
        reply_to_id="m1",
    )


@pytest.mark.asyncio
async def test_deps_satisfied_no_notice(monkeypatch):
    """依赖已满足 → 正常返回 SKILL.md,不追加依赖提示。"""
    import echo_agent.agent.tools.skills as skills_mod

    monkeypatch.setattr(skills_mod, "_is_satisfied", lambda spec: True)

    tool = SkillViewTool(store=_store())
    result = await tool.execute({"name": "demo"}, _ctx())

    assert result.success is True
    assert "Demo skill" in result.output
    assert "尚未安装" not in result.output


@pytest.mark.asyncio
async def test_missing_dep_reports_without_installing(monkeypatch):
    """缺依赖 → 提示缺哪些包并指向 skill_run,绝不安装。"""
    import echo_agent.agent.tools.skills as skills_mod

    monkeypatch.setattr(skills_mod, "_is_satisfied", lambda spec: False)

    tool = SkillViewTool(store=_store())
    result = await tool.execute({"name": "demo"}, _ctx())

    assert result.success is True
    assert "Demo skill" in result.output
    assert "openpyxl>=3.1" in result.output
    assert "skill_run" in result.output


@pytest.mark.asyncio
async def test_view_never_installs_even_on_trusted_cli(monkeypatch):
    """可信 CLI 曾经是静默安装路径 — 现在同样只报告。

    这是本次修复的核心回归点:供应链入口必须关闭,可信环境也不例外。
    """
    import echo_agent.agent.tools.skills as skills_mod
    import echo_agent.dependencies.lazy_deps as lazy_deps

    monkeypatch.setattr(skills_mod, "_is_satisfied", lambda spec: False)
    install_mock = AsyncMock(return_value={"success": True, "installed": [], "detail": "ok"})
    monkeypatch.setattr(lazy_deps, "install_authorized_async", install_mock)
    monkeypatch.setattr(lazy_deps, "install_authorized", MagicMock())

    bus = MagicMock()
    bus.publish_outbound = AsyncMock()
    approval = MagicMock()
    approval.wait_for_decision = AsyncMock()

    tool = SkillViewTool(
        store=_store(), approval=approval, bus=bus, config=_config(),
    )
    result = await tool.execute({"name": "demo"}, _ctx(channel="cli"))

    assert result.success is True
    install_mock.assert_not_called()
    approval.request_approval.assert_not_called()
    bus.publish_outbound.assert_not_called()


@pytest.mark.asyncio
async def test_view_never_installs_on_untrusted_channel(monkeypatch):
    """不可信渠道:既不安装,也不再为安装发审批请求。"""
    import echo_agent.agent.tools.skills as skills_mod
    import echo_agent.dependencies.lazy_deps as lazy_deps

    monkeypatch.setattr(skills_mod, "_is_satisfied", lambda spec: False)
    install_mock = AsyncMock()
    monkeypatch.setattr(lazy_deps, "install_authorized_async", install_mock)

    bus = MagicMock()
    bus.publish_outbound = AsyncMock()
    approval = MagicMock()
    approval.wait_for_decision = AsyncMock()

    config = _config(profile="server", cli_auto_approve=False, trusted_channels=["lark"])
    tool = SkillViewTool(store=_store(), approval=approval, bus=bus, config=config)
    result = await tool.execute({"name": "demo"}, _ctx(channel="weixin"))

    assert result.success is True
    install_mock.assert_not_called()
    approval.request_approval.assert_not_called()
    bus.publish_outbound.assert_not_called()


@pytest.mark.asyncio
async def test_empty_channel_still_reports(monkeypatch):
    """worker 场景(channel 为空)也该给出依赖提示。

    旧实现在这里直接 return "" —— 因为它准备做的是"安装",没有渠道就没法征求同意。
    纯报告没有这个约束,提示对 worker 同样有用。
    """
    import echo_agent.agent.tools.skills as skills_mod

    monkeypatch.setattr(skills_mod, "_is_satisfied", lambda spec: False)

    tool = SkillViewTool(store=_store())
    result = await tool.execute({"name": "demo"}, _ctx(channel=""))

    assert result.success is True
    assert "openpyxl>=3.1" in result.output


@pytest.mark.asyncio
async def test_missing_env_key_reported(monkeypatch):
    """技能声明的凭据环境变量未设置 → 提前告知,而不是等脚本中途退出。"""
    import echo_agent.agent.tools.skills as skills_mod

    monkeypatch.setattr(skills_mod, "_is_satisfied", lambda spec: True)
    monkeypatch.delenv("DEMO_SECRET_TOKEN", raising=False)

    tool = SkillViewTool(store=_store(content=SKILL_MD_WITH_ENV))
    result = await tool.execute({"name": "demo"}, _ctx())

    assert result.success is True
    assert "DEMO_SECRET_TOKEN" in result.output


@pytest.mark.asyncio
async def test_present_env_key_not_reported(monkeypatch):
    """凭据已设置 → 不产生噪音提示。"""
    import echo_agent.agent.tools.skills as skills_mod

    monkeypatch.setattr(skills_mod, "_is_satisfied", lambda spec: True)
    monkeypatch.setenv("DEMO_SECRET_TOKEN", "s3cr3t")

    tool = SkillViewTool(store=_store(content=SKILL_MD_WITH_ENV))
    result = await tool.execute({"name": "demo"}, _ctx())

    assert result.success is True
    # 断言"未设置"提示不出现 —— 键名本身会出现在 SKILL.md 正文里,那是正常回显。
    assert "当前未设置" not in result.output
    # 值本身绝不能出现在输出里
    assert "s3cr3t" not in result.output


@pytest.mark.asyncio
async def test_disabled_skill_says_so(monkeypatch):
    """已禁用技能 → 明确说"已禁用",而不是含糊的 not found。"""
    store = _store()
    store.read_skill.return_value = None
    store.is_disabled.return_value = True

    tool = SkillViewTool(store=store)
    result = await tool.execute({"name": "demo"}, _ctx())

    assert result.success is False
    assert "disabled" in result.error
