"""End-to-end approval-gate ↔ tool tests.

Regression guard for the bug where EXEC tools (execute_code) were silently
blocked on auto-approve channels (CLI) because the gate "approved" the call but
returned an empty approved_actions set, while the tool's own guard re-checks
approved_actions and blocks unless its action is present.

These tests exercise the real chain ApprovalGate.check → ToolExecutionContext →
CodeExecTool.execute, instead of hand-feeding approved_actions.
"""

from __future__ import annotations

from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.agent.approval_gate import ApprovalGate
from echo_agent.agent.degraded_notice import notice_for, REASON_APPROVAL_UNAVAILABLE
from echo_agent.agent.tools.base import ToolExecutionContext
from echo_agent.agent.tools.code_exec import CodeExecTool
from echo_agent.agent.tools.registry import ToolRegistry
from echo_agent.bus.events import ContentBlock, ContentType, InboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.config.loader import load_config
from echo_agent.permissions.manager import ApprovalManager
from echo_agent.tools.base import Tool, ToolResult as _TR


class _FakeInference:
    def needs_confirmation(self, name: str) -> bool:
        return False


def _make_gate(cfg, bus: MessageBus) -> ApprovalGate:
    appr = ApprovalManager(
        require_approval=cfg.permissions.approval.require_approval,
        auto_approve=cfg.permissions.approval.auto_approve,
    )
    return ApprovalGate(config=cfg, approval=appr, inference=_FakeInference(), bus=bus, provider=None)


def _make_tool(cfg) -> CodeExecTool:
    # No executor → direct subprocess, cross-platform runnable.
    return CodeExecTool(
        ".",
        executor=None,
        allowed_languages=cfg.tools.code_exec.allowed_languages,
        exec_policy=cfg.tools.exec,
        network_policy=cfg.execution.network_policy,
    )


async def _run_through_gate(cfg, channel: str) -> object:
    bus = MessageBus()
    gate = _make_gate(cfg, bus)
    tool = _make_tool(cfg)
    event = InboundEvent(
        channel=channel,
        sender_id="u1",
        chat_id="c1",
        content=[ContentBlock(type=ContentType.TEXT, text="run code")],
    )
    check = await gate.check(
        "execute_code",
        {"code": "print(1+1)", "language": "python"},
        "u1",
        channel=channel,
        event=event,
        running=True,
    )
    if check.denial is not None:
        return check.denial
    ctx = ToolExecutionContext(approved_actions=check.approved_actions)
    return await tool.execute({"code": "print(1+1)", "language": "python"}, ctx)


@pytest.mark.asyncio
async def test_cli_auto_approve_lets_execute_code_run():
    """CLI auto-approve must actually let the EXEC tool run end-to-end.

    Before the fix this returned 'Code blocked by execution policy' because the
    gate approved with an empty approved_actions set.
    """
    cfg = load_config()
    result = await _run_through_gate(cfg, "cli")
    assert result.success is True, f"expected success, got error: {result.error}"
    assert "2" in (result.output or "")


@pytest.mark.asyncio
async def test_weixin_manual_flow_still_runs():
    """The previously-working manual-approval path must keep working."""
    cfg = load_config()
    cfg.permissions.approval.wait_timeout_seconds = 1
    result = await _run_through_gate(cfg, "weixin")
    assert result.success is True, f"expected success, got error: {result.error}"
    assert "2" in (result.output or "")


@pytest.mark.asyncio
async def test_gate_approved_actions_non_empty_on_cli():
    """The gate must hand the tool the approved action, not an empty set."""
    cfg = load_config()
    bus = MessageBus()
    gate = _make_gate(cfg, bus)
    event = InboundEvent(channel="cli", sender_id="u1", chat_id="c1")
    check = await gate.check(
        "execute_code",
        {"code": "print(1)", "language": "python"},
        "u1",
        channel="cli",
        event=event,
        running=True,
    )
    assert check.denial is None
    assert "execute_code" in check.approved_actions


@pytest.mark.asyncio
async def test_mode_off_lets_exec_run():
    """approval.mode == 'off' must also propagate approved_actions."""
    cfg = load_config()
    cfg.permissions.approval.mode = "off"
    # Use a non-CLI channel so we exercise Step 8 rather than the CLI shortcut.
    result = await _run_through_gate(cfg, "weixin")
    assert result.success is True, f"expected success, got error: {result.error}"


class _FakeExecTool(Tool):
    """模拟 MCP destructiveHint→EXEC 的动态工具：不在静态风险表里，自报 exec。"""

    name = "mcp_x_destructive"
    description = "fake destructive mcp tool"
    parameters: dict = {"type": "object", "properties": {}}
    risk_level = "exec"

    async def execute(self, params, ctx=None):
        return _TR(success=True, output="ran")


def _make_gate_with_registry(cfg, bus, registry):
    appr = ApprovalManager(
        require_approval=cfg.permissions.approval.require_approval,
        auto_approve=cfg.permissions.approval.auto_approve,
    )
    return ApprovalGate(
        config=cfg, approval=appr, inference=_FakeInference(),
        bus=bus, provider=None, registry=registry,
    )


@pytest.mark.asyncio
async def test_dynamic_exec_tool_requires_approval_when_unattended():
    """P0-1: 自报 exec 的动态工具在无人值守下必须被拦截，而非按 WRITE 放行。"""
    cfg = load_config()
    cfg.permissions.approval.mode = "manual"
    cfg.permissions.approval.unattended_policy = "deny"
    bus = MessageBus()
    registry = ToolRegistry()
    registry.register(_FakeExecTool())
    gate = _make_gate_with_registry(cfg, bus, registry)

    event = InboundEvent(
        channel="cron",
        sender_id="u1",
        chat_id="c1",
        content=[ContentBlock(type=ContentType.TEXT, text="go")],
        metadata={"_unattended": True},
    )
    check = await gate.check(
        "mcp_x_destructive", {}, "u1", channel="cron", event=event, running=True,
    )
    assert check.denial is not None
    assert "unattended" in (check.denial.error or "").lower()


@pytest.mark.asyncio
async def test_smart_unavailable_sets_notify_user():
    cfg = load_config()
    cfg.permissions.approval.mode = "smart"
    bus = MessageBus()
    appr = ApprovalManager(require_approval=cfg.permissions.approval.require_approval)
    provider = MagicMock()
    provider.chat_with_retry = AsyncMock(return_value=MagicMock(content=""))
    gate = ApprovalGate(
        config=cfg, approval=appr, inference=_FakeInference(), bus=bus, provider=provider,
    )
    event = InboundEvent(
        channel="weixin", sender_id="u1", chat_id="c1",
        content=[ContentBlock(type=ContentType.TEXT, text="research")],
    )
    check = await gate.check("exec", {"command": "curl https://x"}, "u1", channel="weixin", event=event)
    assert check.denial is not None
    assert check.notify_user is True
    assert check.notice == notice_for(REASON_APPROVAL_UNAVAILABLE)
