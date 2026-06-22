"""Tests for SkillViewTool missing-dependency precheck + approval closed-loop.

Follows the tests/test_approval_degraded_notice.py pattern: mock bus.publish_outbound
to collect outbound events, and mock approval.wait_for_decision to return a fake
ApprovalRequest (or None) without real blocking.
"""

from __future__ import annotations

from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.agent.tools.base import ToolExecutionContext
from echo_agent.agent.tools.skills import SkillViewTool
from echo_agent.permissions.manager import ApprovalRequest, ApprovalStatus


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
    install_mock = MagicMock()
    monkeypatch.setattr(skills_mod, "install_authorized", install_mock)

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
    install_mock = MagicMock(return_value={
        "success": True, "installed": ["openpyxl>=3.1"], "skipped": [],
        "rejected": [], "detail": "ok",
    })
    monkeypatch.setattr(skills_mod, "install_authorized", install_mock)

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
    install_mock = MagicMock()
    monkeypatch.setattr(skills_mod, "install_authorized", install_mock)

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
    install_mock = MagicMock()
    monkeypatch.setattr(skills_mod, "install_authorized", install_mock)

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
    install_mock = MagicMock()
    monkeypatch.setattr(skills_mod, "install_authorized", install_mock)

    tool = SkillViewTool(store=_store())
    result = await tool.execute({"name": "demo"}, _ctx())

    assert result.success is True
    assert "Demo skill" in result.output
    install_mock.assert_not_called()
