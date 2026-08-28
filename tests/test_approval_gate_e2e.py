"""End-to-end approval-gate ↔ tool tests.

Regression guard for the bug where EXEC tools (execute_code) were silently
blocked on auto-approve channels (CLI) because the gate "approved" the call but
returned an empty approved_actions set, while the tool's own guard re-checks
approved_actions and blocks unless its action is present.

These tests exercise the real chain ApprovalGate.check → ToolExecutionContext →
CodeExecTool.execute, instead of hand-feeding approved_actions.
"""

from __future__ import annotations

import asyncio
from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.agent.approval_gate import (
    APPROVAL_SOURCE_AUTO,
    APPROVAL_SOURCE_HUMAN,
    ApprovalGate,
)
from echo_agent.agent.degraded_notice import notice_for, REASON_APPROVAL_UNAVAILABLE
from echo_agent.tools import ToolExecutionContext
from echo_agent.agent.tools.code_exec import CodeExecTool
from echo_agent.agent.tools.registry import ToolRegistry
from echo_agent.bus.events import ContentBlock, ContentType, InboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.base import SendResult
from echo_agent.config.loader import load_config
from echo_agent.config.schema import Config
from echo_agent.permissions.manager import ApprovalManager
from echo_agent.tools import Tool, ToolResult as _TR


class _FakeInference:
    def needs_confirmation(self, name: str) -> bool:
        return False


def _exec_cfg() -> Config:
    """A config built in-process, for tests whose verdict must not depend on
    whether this machine happens to have a ~/.echo-agent/echo-agent.yaml.

    ``network_policy="allow"`` matches what the code_exec tool needs to run a
    trivial snippet; the packaged default is "deny".
    """
    cfg = Config()
    cfg.execution.network_policy = "allow"
    return cfg


def _make_gate(cfg, bus: MessageBus) -> ApprovalGate:
    async def _deliver_prompt(_event):
        return SendResult(success=True, message_id="approval-prompt")

    # Manual approval is only meaningful after the prompt reaches a channel.
    # Mirror that production receipt instead of leaving this gate/tool test with
    # an outbound bus that has no platform handler at all.
    bus.subscribe_outbound_global(_deliver_prompt)
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
async def test_cli_auto_approve_applies_to_gateway_cli_channel():
    """CLI auto-approve must also cover the attached-cli channel 'gateway:cli'.

    Regression: the cli attaches OVER the gateway, so its channel is
    'gateway:cli', but _should_auto_approve_cli only matched {'cli','direct',''}.
    A flagged EXEC (e.g. the weather skill's `curl`) then skipped auto-approve
    and fell into the smart/manual approval path, stalling the turn. The channel
    set is now shared with _is_interactive_channel, so both spellings pass.
    """
    cfg = load_config()
    result = await _run_through_gate(cfg, "gateway:cli")
    assert result.success is True, f"expected success, got error: {result.error}"
    assert "2" in (result.output or "")


@pytest.mark.asyncio
async def test_weixin_manual_flow_still_runs():
    """The manual-approval path: a human answers, and the tool then runs.

    Rewritten twice over. It used to assert only that the call *succeeded* on
    weixin while nothing ever answered the prompt — which passed for the wrong
    reason. `execute_code` was absent from the machine-local config's
    ``require_approval``, so ApprovalManager fell through to
    ``default_policy="approve"`` and handed back an already-APPROVED request that
    was never pending. The assertion "remote IM can run code with no human in the
    loop" read as "the manual flow works".

    Two things are pinned now. The config is built explicitly rather than read
    from ``load_config()``: an approval-gate test whose verdict depends on
    whether the developer's machine has a ~/.echo-agent/echo-agent.yaml is
    testing the machine. And the approval is actually answered, so the path under
    test is the one named in the title.
    """
    cfg = _exec_cfg()
    bus = MessageBus()
    gate = _make_gate(cfg, bus)
    tool = _make_tool(cfg)
    event = InboundEvent(
        channel="weixin", sender_id="u1", chat_id="c1",
        content=[ContentBlock(type=ContentType.TEXT, text="run code")],
    )
    args = {"code": "print(1+1)", "language": "python"}

    async def approve_when_pending() -> None:
        """Answer as a human would, once the gate has published the prompt."""
        for _ in range(200):
            pending = gate._approval.get_pending()
            if pending:
                gate._approval.approve(pending[0].id, decided_by="u1")
                return
            await asyncio.sleep(0.01)
        raise AssertionError("gate never opened a pending approval request")

    approver = asyncio.create_task(approve_when_pending())
    check = await gate.check(
        "execute_code", args, "u1", channel="weixin", event=event, running=True,
    )
    await approver

    assert check.denial is None, f"expected approval to let the call through: {check.denial}"
    # The one path where a person saw this specific call and said yes.
    assert check.approval_source == APPROVAL_SOURCE_HUMAN
    ctx = ToolExecutionContext(approved_actions=check.approved_actions)
    result = await tool.execute(args, ctx)
    assert result.success is True, f"expected success, got error: {result.error}"
    assert "2" in (result.output or "")


@pytest.mark.asyncio
async def test_weixin_manual_flow_denied_blocks_the_tool():
    """The same path, answered "no": the call is refused, not run."""
    cfg = _exec_cfg()
    bus = MessageBus()
    gate = _make_gate(cfg, bus)
    event = InboundEvent(
        channel="weixin", sender_id="u1", chat_id="c1",
        content=[ContentBlock(type=ContentType.TEXT, text="run code")],
    )
    args = {"code": "print(1+1)", "language": "python"}

    async def deny_when_pending() -> None:
        for _ in range(200):
            pending = gate._approval.get_pending()
            if pending:
                gate._approval.deny(pending[0].id, reason="不允许", decided_by="u1")
                return
            await asyncio.sleep(0.01)
        raise AssertionError("gate never opened a pending approval request")

    denier = asyncio.create_task(deny_when_pending())
    check = await gate.check(
        "execute_code", args, "u1", channel="weixin", event=event, running=True,
    )
    await denier

    assert check.denial is not None
    assert "denied" in (check.denial.error or "").lower()


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
        unattended=True,
    )
    check = await gate.check(
        "mcp_x_destructive", {}, "u1", channel="cron", event=event, running=True,
    )
    assert check.denial is not None
    assert "unattended" in (check.denial.error or "").lower()


@pytest.mark.asyncio
async def test_cron_authorized_allows_exec_unattended():
    """P1-6: 携带 per-job 授权的 cron 事件在无人值守下放行 EXEC(全局仍 deny)。"""
    cfg = load_config()
    cfg.permissions.approval.mode = "manual"
    cfg.permissions.approval.unattended_policy = "deny"
    bus = MessageBus()
    gate = _make_gate(cfg, bus)

    event = InboundEvent(
        channel="weixin", sender_id="cron", chat_id="c1",
        content=[ContentBlock(type=ContentType.TEXT, text="生成天气语音")],
        unattended=True, cron_authorized=True,
    )
    check = await gate.check(
        "exec", {"command": "edge-tts --text hi -o a.mp3"},
        "cron", channel="weixin", event=event, running=True,
    )
    assert check.denial is None
    assert check.approved_actions  # 非空,工具自身 guard 可放行


@pytest.mark.asyncio
async def test_cron_authorized_still_denies_dangerous_unattended():
    """P1-6: 即便授权,DANGEROUS(如再建 cron)在无人值守下仍拒,防递归提权。"""
    cfg = load_config()
    cfg.permissions.approval.mode = "manual"
    cfg.permissions.approval.unattended_policy = "deny"
    bus = MessageBus()
    gate = _make_gate(cfg, bus)

    event = InboundEvent(
        channel="weixin", sender_id="cron", chat_id="c1",
        content=[ContentBlock(type=ContentType.TEXT, text="x")],
        unattended=True, cron_authorized=True,
    )
    check = await gate.check(
        "cronjob", {"action": "create", "name": "n", "schedule": "* * * * *", "command": "c"},
        "cron", channel="weixin", event=event, running=True,
    )
    assert check.denial is not None
    assert "unattended" in (check.denial.error or "").lower()


@pytest.mark.asyncio
async def test_cron_unauthorized_denies_exec_unattended():
    """P1-6: 未携带授权标记的无人值守 EXEC 仍被拒(deny 策略下)。"""
    cfg = load_config()
    cfg.permissions.approval.mode = "manual"
    cfg.permissions.approval.unattended_policy = "deny"
    bus = MessageBus()
    gate = _make_gate(cfg, bus)

    event = InboundEvent(
        channel="weixin", sender_id="cron", chat_id="c1",
        content=[ContentBlock(type=ContentType.TEXT, text="x")],
        unattended=True,  # 无 cron_authorized
    )
    check = await gate.check(
        "exec", {"command": "edge-tts --text hi -o a.mp3"},
        "cron", channel="weixin", event=event, running=True,
    )
    assert check.denial is not None


@pytest.mark.asyncio
async def test_cli_auto_approve_does_not_cover_unattended_jobs():
    """Step 6 (cli_auto_approve) must not fire for unattended events.

    A cron job created from a cli session keeps that session's delivery channel,
    so the fired event reaches the gate as channel='cli'. Step 6 sits above the
    unattended check, so it used to approve every risk level for such a job: EXEC
    ran with no per-job grant at all, and DANGEROUS — which stays denied even for
    authorized jobs precisely to stop a job spawning more unattended jobs — was
    approved too.
    """
    cfg = load_config()
    cfg.permissions.approval.mode = "manual"
    cfg.permissions.approval.unattended_policy = "deny"
    # Pin the exact configuration Step 6 keys off, so the test states the
    # preconditions rather than inheriting them from the host's config file.
    cfg.security.profile = "personal_cli"
    cfg.permissions.approval.cli_auto_approve = True
    gate = _make_gate(cfg, MessageBus())

    event = InboundEvent(
        channel="cli", sender_id="cron", chat_id="c1",
        content=[ContentBlock(type=ContentType.TEXT, text="x")],
        unattended=True,  # no cron_authorized: nobody granted this job anything
    )
    exec_check = await gate.check(
        "execute_code", {"language": "python", "code": "print(1)"},
        "cron", channel="cli", event=event, running=True,
    )
    assert exec_check.denial is not None
    assert "unattended" in (exec_check.denial.error or "").lower()

    cron_check = await gate.check(
        "cronjob", {"action": "create", "name": "n", "schedule": "* * * * *", "command": "c"},
        "cron", channel="cli", event=event, running=True,
    )
    assert cron_check.denial is not None
    assert "unattended" in (cron_check.denial.error or "").lower()


@pytest.mark.asyncio
async def test_cli_auto_approve_still_covers_interactive_sessions():
    """Positive control for the change above: a human-at-the-keyboard cli session
    (no unattended flag) keeps its auto-approve for EXEC-tier work.

    DANGEROUS is deliberately excluded — see the test below."""
    cfg = load_config()
    cfg.permissions.approval.mode = "manual"
    cfg.security.profile = "personal_cli"
    cfg.permissions.approval.cli_auto_approve = True
    gate = _make_gate(cfg, MessageBus())

    event = InboundEvent(
        channel="cli", sender_id="u1", chat_id="c1",
        content=[ContentBlock(type=ContentType.TEXT, text="x")],
    )
    check = await gate.check(
        "execute_code", {"language": "python", "code": "print(1)"},
        "u1", channel="cli", event=event, running=True,
    )
    assert check.denial is None
    assert check.approved_actions
    # Auto-approval is policy, not consent: nothing here may claim a human looked.
    assert check.approval_source == APPROVAL_SOURCE_AUTO


@pytest.mark.asyncio
async def test_cli_auto_approve_does_not_cover_dangerous_tools():
    """cli_auto_approve must not silently approve DANGEROUS tools.

    cronjob mints a persistent unattended grant from a one-off approval, and its
    execute() documented "only reached after the human approved this specific
    call". With the shipped defaults (profile=personal_cli, cli_auto_approve=True)
    that was false: the gate approved every risk level on cli channels, so a
    model could create a job carrying a valid `tui-approval` grant that no human
    had ever seen. A one-off convenience must not buy a persistent privilege, so
    these route to the manual prompt instead.
    """
    cfg = load_config()
    cfg.permissions.approval.mode = "manual"
    cfg.security.profile = "personal_cli"
    cfg.permissions.approval.cli_auto_approve = True
    gate = _make_gate(cfg, MessageBus())

    event = InboundEvent(
        channel="cli", sender_id="u1", chat_id="c1",
        content=[ContentBlock(type=ContentType.TEXT, text="x")],
    )
    # running=False makes the interactive path return the "approval required"
    # handle immediately rather than parking on wait_timeout_seconds.
    check = await gate.check(
        "cronjob", {"action": "create", "name": "n", "schedule": "* * * * *", "command": "c"},
        "u1", channel="cli", event=event, running=False,
    )
    assert check.denial is not None
    assert "approval" in (check.denial.error or "").lower()


@pytest.mark.asyncio
async def test_cli_channel_unattended_authorized_job_still_runs_exec():
    """Routing unattended cli events to Step 11 must not break authorized jobs:
    a job a human did authorize keeps its WRITE/EXEC access."""
    cfg = load_config()
    cfg.permissions.approval.mode = "manual"
    cfg.permissions.approval.unattended_policy = "deny"
    gate = _make_gate(cfg, MessageBus())

    event = InboundEvent(
        channel="cli", sender_id="cron", chat_id="c1",
        content=[ContentBlock(type=ContentType.TEXT, text="x")],
        unattended=True, cron_authorized=True,
    )
    check = await gate.check(
        "execute_code", {"language": "python", "code": "print(1)"},
        "cron", channel="cli", event=event, running=True,
    )
    assert check.denial is None
    assert check.approved_actions


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


# ── "approve all" scope recording ──────────────────────────────────────────────

from echo_agent.permissions.allowlist import ApprovalLevel  # noqa: E402


def test_record_approval_all_grants_exec_family_wildcard():
    cfg = load_config()
    gate = _make_gate(cfg, MessageBus())
    # User replied "/approve <id> all" on an exec:pip prompt.
    gate._record_approval("sess1", "exec:pip", ApprovalLevel.SESSION_ALL)
    # Later, differently-named exec commands in the same session are now allowed.
    assert gate._allowlist.is_approved("sess1", "exec:pip") is True
    assert gate._allowlist.is_approved("sess1", "exec:ffprobe") is True
    # Other families are NOT swept in.
    assert gate._allowlist.is_approved("sess1", "code:python") is False


def test_record_approval_all_on_dangerous_tool_does_not_broaden():
    cfg = load_config()
    gate = _make_gate(cfg, MessageBus())
    # "all" chosen on a DANGEROUS tool:cronjob prompt must NOT grant tool:* —
    # only the exact key, at session scope (no privilege-escalation blanket).
    gate._record_approval("sess1", "tool:cronjob", ApprovalLevel.SESSION_ALL)
    assert gate._allowlist.is_approved("sess1", "tool:cronjob") is True
    assert gate._allowlist.is_approved("sess1", "tool:skill_install") is False
