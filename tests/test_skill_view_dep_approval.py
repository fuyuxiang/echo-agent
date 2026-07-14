"""Tests for SkillViewTool missing-dependency precheck + approval closed-loop.

Follows the tests/test_approval_degraded_notice.py pattern: mock bus.publish_outbound
to collect outbound events, and mock approval.wait_for_decision to return a fake
ApprovalRequest (or None) without real blocking.
"""

from __future__ import annotations

from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.agent.tools.base import ToolExecutionContext
from echo_agent.agent.tools.skills import SkillViewTool
from echo_agent.permissions.manager import ApprovalRequest, ApprovalStatus


def _trusted_config(profile: str = "personal_cli", cli_auto_approve: bool = True,
                    trusted_channels: list[str] | None = None) -> SimpleNamespace:
    """Minimal config exposing the fields _is_trusted_env reads."""
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


def _store(content: str = SKILL_MD, files: list[str] | None = None):
    store = MagicMock()
    store.read_skill.return_value = content
    store.list_files.return_value = files or []
    store.read_file.return_value = "file content"
    return store


def _ctx(channel: str = "weixin") -> ToolExecutionContext:
    return ToolExecutionContext(
        user_id="u1",
        channel=channel,
        chat_id="c1",
        reply_to_id="m1",
    )


@pytest.mark.asyncio
async def test_deps_satisfied_no_approval(monkeypatch):
    """技能依赖已满足 → 正常返回 SKILL.md,不发审批。"""
    import echo_agent.agent.tools.skills as skills_mod

    monkeypatch.setattr(skills_mod, "_is_satisfied", lambda spec: True)
    install_mock = AsyncMock()
    monkeypatch.setattr(skills_mod, "install_authorized_async", install_mock)

    bus = MagicMock()
    bus.publish_outbound = AsyncMock()
    approval = MagicMock()
    approval.wait_for_decision = AsyncMock()

    tool = SkillViewTool(store=_store(), approval=approval, bus=bus)
    result = await tool.execute({"name": "demo"}, _ctx())

    assert result.success is True
    assert "Demo skill" in result.output
    bus.publish_outbound.assert_not_called()
    install_mock.assert_not_called()


@pytest.mark.asyncio
async def test_missing_dep_approved_installs(monkeypatch):
    """缺依赖 + approve → 发带 _dep_install_request 的出站、且调 install_authorized。"""
    import echo_agent.agent.tools.skills as skills_mod

    monkeypatch.setattr(skills_mod, "_is_satisfied", lambda spec: False)
    install_mock = AsyncMock(return_value={
        "success": True, "installed": ["openpyxl>=3.1"], "skipped": [],
        "rejected": [], "detail": "ok",
    })
    monkeypatch.setattr(skills_mod, "install_authorized_async", install_mock)

    bus = MagicMock()
    bus.publish_outbound = AsyncMock()
    approval = MagicMock()
    approval.request_approval.return_value = ApprovalRequest(id="req1", action="dep_install")
    approval.wait_for_decision = AsyncMock(
        return_value=ApprovalRequest(id="req1", action="dep_install", status=ApprovalStatus.APPROVED)
    )

    tool = SkillViewTool(store=_store(), approval=approval, bus=bus)
    result = await tool.execute({"name": "demo"}, _ctx())

    assert result.success is True
    # request approval with action="dep_install"
    args, kwargs = approval.request_approval.call_args
    assert (args[0] if args else kwargs.get("action")) == "dep_install"
    # outbound carries the dep-install marker metadata
    bus.publish_outbound.assert_awaited_once()
    out = bus.publish_outbound.await_args.args[0]
    assert out.metadata.get("_dep_install_request") is True
    assert out.metadata.get("_skill_name") == "demo"
    assert out.metadata.get("_request_id") == "req1"
    assert "openpyxl>=3.1" in out.metadata.get("_missing", [])
    # install actually invoked with the missing specs
    install_mock.assert_called_once()
    inst_args, inst_kwargs = install_mock.call_args
    assert "openpyxl>=3.1" in inst_args[0]


@pytest.mark.asyncio
async def test_missing_dep_denied_no_install(monkeypatch):
    """缺依赖 + deny/超时(None)→ 不装,内容提示依赖未装。"""
    import echo_agent.agent.tools.skills as skills_mod

    monkeypatch.setattr(skills_mod, "_is_satisfied", lambda spec: False)
    install_mock = AsyncMock()
    monkeypatch.setattr(skills_mod, "install_authorized_async", install_mock)

    bus = MagicMock()
    bus.publish_outbound = AsyncMock()
    approval = MagicMock()
    approval.request_approval.return_value = ApprovalRequest(id="req2", action="dep_install")
    approval.wait_for_decision = AsyncMock(return_value=None)

    tool = SkillViewTool(store=_store(), approval=approval, bus=bus)
    result = await tool.execute({"name": "demo"}, _ctx())

    assert result.success is True
    install_mock.assert_not_called()
    assert "依赖" in result.output


@pytest.mark.asyncio
async def test_empty_channel_degrades(monkeypatch):
    """ctx.channel 为空(worker 场景)→ 不发审批,降级为纯查看。"""
    import echo_agent.agent.tools.skills as skills_mod

    monkeypatch.setattr(skills_mod, "_is_satisfied", lambda spec: False)
    install_mock = AsyncMock()
    monkeypatch.setattr(skills_mod, "install_authorized_async", install_mock)

    bus = MagicMock()
    bus.publish_outbound = AsyncMock()
    approval = MagicMock()

    tool = SkillViewTool(store=_store(), approval=approval, bus=bus)
    result = await tool.execute({"name": "demo"}, _ctx(channel=""))

    assert result.success is True
    assert "Demo skill" in result.output
    bus.publish_outbound.assert_not_called()
    approval.request_approval.assert_not_called()
    install_mock.assert_not_called()


@pytest.mark.asyncio
async def test_no_approval_injected_degrades(monkeypatch):
    """approval/bus 未注入 → 降级为纯查看(向后兼容)。"""
    import echo_agent.agent.tools.skills as skills_mod

    monkeypatch.setattr(skills_mod, "_is_satisfied", lambda spec: False)
    install_mock = AsyncMock()
    monkeypatch.setattr(skills_mod, "install_authorized_async", install_mock)

    tool = SkillViewTool(store=_store())
    result = await tool.execute({"name": "demo"}, _ctx())

    assert result.success is True
    assert "Demo skill" in result.output
    install_mock.assert_not_called()


@pytest.mark.asyncio
async def test_trusted_cli_installs_without_approval(monkeypatch):
    """可信环境(personal_cli + cli_auto_approve + channel=cli)+ 缺依赖
    → 不发审批,直接 install_authorized,output 含安装结果。"""
    import echo_agent.agent.tools.skills as skills_mod

    monkeypatch.setattr(skills_mod, "_is_satisfied", lambda spec: False)
    install_mock = AsyncMock(return_value={
        "success": True, "installed": ["openpyxl>=3.1"], "skipped": [],
        "rejected": [], "detail": "ok",
    })
    monkeypatch.setattr(skills_mod, "install_authorized_async", install_mock)

    bus = MagicMock()
    bus.publish_outbound = AsyncMock()
    approval = MagicMock()
    approval.wait_for_decision = AsyncMock()

    tool = SkillViewTool(
        store=_store(), approval=approval, bus=bus, config=_trusted_config(),
    )
    result = await tool.execute({"name": "demo"}, _ctx(channel="cli"))

    assert result.success is True
    assert "依赖已安装" in result.output
    bus.publish_outbound.assert_not_called()
    approval.request_approval.assert_not_called()
    approval.wait_for_decision.assert_not_called()
    install_mock.assert_called_once()
    inst_args, inst_kwargs = install_mock.call_args
    assert "openpyxl>=3.1" in inst_args[0]
    assert inst_kwargs.get("source") == "skill_view_trusted:demo"


@pytest.mark.asyncio
async def test_trusted_channel_installs_without_approval(monkeypatch):
    """可信渠道(channel 在 trusted_channels)+ 缺依赖 → 不发审批直装。"""
    import echo_agent.agent.tools.skills as skills_mod

    monkeypatch.setattr(skills_mod, "_is_satisfied", lambda spec: False)
    install_mock = AsyncMock(return_value={
        "success": True, "installed": ["openpyxl>=3.1"], "skipped": [],
        "rejected": [], "detail": "ok",
    })
    monkeypatch.setattr(skills_mod, "install_authorized_async", install_mock)

    bus = MagicMock()
    bus.publish_outbound = AsyncMock()
    approval = MagicMock()
    approval.wait_for_decision = AsyncMock()

    config = _trusted_config(profile="server", cli_auto_approve=False,
                             trusted_channels=["lark"])
    tool = SkillViewTool(store=_store(), approval=approval, bus=bus, config=config)
    result = await tool.execute({"name": "demo"}, _ctx(channel="lark"))

    assert result.success is True
    assert "依赖已安装" in result.output
    bus.publish_outbound.assert_not_called()
    approval.request_approval.assert_not_called()
    install_mock.assert_called_once()


@pytest.mark.asyncio
async def test_untrusted_channel_still_requires_approval(monkeypatch):
    """不可信渠道(weixin 不在 trusted_channels,非 personal_cli)+ 缺依赖
    → 仍走审批(publish_outbound 被调 + wait_for_decision)。"""
    import echo_agent.agent.tools.skills as skills_mod

    monkeypatch.setattr(skills_mod, "_is_satisfied", lambda spec: False)
    install_mock = AsyncMock(return_value={
        "success": True, "installed": ["openpyxl>=3.1"], "skipped": [],
        "rejected": [], "detail": "ok",
    })
    monkeypatch.setattr(skills_mod, "install_authorized_async", install_mock)

    bus = MagicMock()
    bus.publish_outbound = AsyncMock()
    approval = MagicMock()
    approval.request_approval.return_value = ApprovalRequest(id="req3", action="dep_install")
    approval.wait_for_decision = AsyncMock(
        return_value=ApprovalRequest(id="req3", action="dep_install", status=ApprovalStatus.APPROVED)
    )

    config = _trusted_config(profile="server", cli_auto_approve=False,
                             trusted_channels=["lark"])
    tool = SkillViewTool(store=_store(), approval=approval, bus=bus, config=config)
    result = await tool.execute({"name": "demo"}, _ctx(channel="weixin"))

    assert result.success is True
    bus.publish_outbound.assert_awaited_once()
    approval.request_approval.assert_called_once()
    approval.wait_for_decision.assert_awaited_once()
    install_mock.assert_called_once()
    inst_args, inst_kwargs = install_mock.call_args
    assert inst_kwargs.get("source") == "skill_view:demo"

