# tests/test_approval_require_tier_not_namelist.py
"""The risk tier gates the call; ``require_approval`` only adds to it.

Two layers were adjudicating one question and the weaker answer won.
ApprovalGate step 10 (``_approval_required``) finds EXEC/DANGEROUS calls need
approval and enters the manual flow. Inside it, ApprovalManager.request_approval
re-decided the same question from a *name list* — and for any action absent from
that list fell through to ``default_policy="approve"``, returning an
already-APPROVED request that was never registered as pending. The gate saw
APPROVED and let the call through with no prompt.

So the effective gate was "is this tool's name in require_approval", not "what
is this tool's risk tier". The packaged default list happens to cover the static
EXEC/DANGEROUS tools, which is why the hole was invisible by default. It opens
for anything the list structurally cannot name:

  - runtime-registered tools (MCP: destructiveHint → EXEC at adapter
    construction) — a static default list can never contain them
  - any newly added EXEC-tier tool, until someone remembers the list

The fix is ``require=True``: the gate instructs the manager rather than
consulting it and being overruled. These tests pin that from both ends — a tool
absent from the list still gets a prompt, and the explicit allow rules that
SHOULD win still do.
"""
from __future__ import annotations

import asyncio

import pytest

from echo_agent.agent.approval_gate import (
    APPROVAL_SOURCE_AUTO,
    APPROVAL_SOURCE_HUMAN,
    ApprovalGate,
)
from echo_agent.bus.events import ContentBlock, ContentType, InboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.config.schema import Config
from echo_agent.permissions.allowlist import ApprovalAllowlist, ApprovalLevel
from echo_agent.permissions.manager import ApprovalManager
from echo_agent.security.risk_classifier import RiskLevel


class _FakeInference:
    def needs_confirmation(self, name: str) -> bool:
        return False


class _DeclaredRiskTool:
    """Stands in for an MCP adapter: EXEC by declaration, not by static map."""

    def __init__(self, risk_level: str = "exec"):
        self.risk_level = risk_level


class _Registry:
    def __init__(self, tools: dict[str, object]):
        self._tools = tools

    def get(self, name: str):
        return self._tools.get(name)


def _gate(
    cfg: Config,
    *,
    registry: object | None = None,
    allowlist: ApprovalAllowlist | None = None,
) -> ApprovalGate:
    appr = ApprovalManager(
        require_approval=cfg.permissions.approval.require_approval,
        auto_approve=cfg.permissions.approval.auto_approve,
    )
    return ApprovalGate(
        config=cfg, approval=appr, inference=_FakeInference(),
        bus=MessageBus(), provider=None, registry=registry, allowlist=allowlist,
    )


def _remote_cfg() -> Config:
    """A remote-IM deployment: nobody is at a local keyboard, and the CLI
    auto-approve shortcut (step 7) must not apply."""
    cfg = Config()
    cfg.permissions.approval.mode = "manual"
    cfg.permissions.approval.cli_auto_approve = False
    cfg.permissions.approval.wait_timeout_seconds = 1
    return cfg


def _event(channel: str = "telegram") -> InboundEvent:
    return InboundEvent(
        channel=channel, sender_id="u1", chat_id="c1",
        content=[ContentBlock(type=ContentType.TEXT, text="do it")],
    )


async def _answer(gate: ApprovalGate, decision: str) -> str:
    """Answer the prompt as a human would; returns the request id seen."""
    for _ in range(300):
        pending = gate._approval.get_pending()
        if pending:
            req_id = pending[0].id
            if decision == "approve":
                gate._approval.approve(req_id, decided_by="u1")
            else:
                gate._approval.deny(req_id, reason="no", decided_by="u1")
            return req_id
        await asyncio.sleep(0.01)
    raise AssertionError("gate never opened a pending approval request")


# An EXEC-tier tool that is NOT in the require_approval default list. This is the
# shape of the hole: risk says "ask", the name list says nothing.
_ABSENT_EXEC_TOOL = "mcp_delete_everything"


@pytest.mark.asyncio
async def test_declared_exec_tool_absent_from_namelist_still_prompts() -> None:
    """An MCP-style EXEC tool must reach the prompt, not be waved through."""
    cfg = _remote_cfg()
    assert _ABSENT_EXEC_TOOL not in cfg.permissions.approval.require_approval

    gate = _gate(cfg, registry=_Registry({_ABSENT_EXEC_TOOL: _DeclaredRiskTool("exec")}))
    approver = asyncio.create_task(_answer(gate, "approve"))
    check = await gate.check(
        _ABSENT_EXEC_TOOL, {"target": "/"}, "u1", channel="telegram",
        event=_event(), running=True,
    )
    await approver

    assert check.denial is None
    # Reaching HUMAN provenance proves a real pending request existed and a
    # person answered it — the old path returned AUTO with no prompt at all.
    assert check.approval_source == APPROVAL_SOURCE_HUMAN


@pytest.mark.asyncio
async def test_declared_exec_tool_absent_from_namelist_can_be_denied() -> None:
    """The same call, refused. Previously it could not be refused at all."""
    cfg = _remote_cfg()
    gate = _gate(cfg, registry=_Registry({_ABSENT_EXEC_TOOL: _DeclaredRiskTool("exec")}))
    denier = asyncio.create_task(_answer(gate, "deny"))
    check = await gate.check(
        _ABSENT_EXEC_TOOL, {"target": "/"}, "u1", channel="telegram",
        event=_event(), running=True,
    )
    await denier

    assert check.denial is not None
    assert "denied" in (check.denial.error or "").lower()


@pytest.mark.asyncio
async def test_unanswered_exec_call_times_out_closed() -> None:
    """With nobody answering, the call is refused rather than run."""
    cfg = _remote_cfg()
    gate = _gate(cfg, registry=_Registry({_ABSENT_EXEC_TOOL: _DeclaredRiskTool("exec")}))
    check = await gate.check(
        _ABSENT_EXEC_TOOL, {"target": "/"}, "u1", channel="telegram",
        event=_event(), running=True,
    )
    assert check.denial is not None
    assert "timed out" in (check.denial.error or "").lower()


@pytest.mark.asyncio
@pytest.mark.parametrize("tool_name", ["delegate_task", "spawn_task"])
async def test_worker_dispatch_prompts_on_remote_channel(tool_name: str) -> None:
    """Dispatching a worker from a remote channel needs a human.

    A worker can call exec, so the dispatch must not be cheaper than exec. Both
    the risk tier (EXEC) and the require_approval default now say so; this pins
    the behaviour rather than either mechanism.
    """
    cfg = _remote_cfg()
    gate = _gate(cfg)
    approver = asyncio.create_task(_answer(gate, "approve"))
    check = await gate.check(
        tool_name, {"goal": "run a command"}, "u1", channel="telegram",
        event=_event(), running=True,
    )
    await approver

    assert check.denial is None
    assert check.approval_source == APPROVAL_SOURCE_HUMAN


@pytest.mark.asyncio
@pytest.mark.parametrize("tool_name", ["delegate_task", "spawn_task"])
async def test_worker_dispatch_still_free_on_local_cli(tool_name: str) -> None:
    """Local interactive work is not made noisier by any of this.

    cli_auto_approve (step 7) sits above the manual flow, so a human at their own
    keyboard sees no new prompts. If this breaks, the fix has leaked into the
    interactive path it was never meant to touch.
    """
    cfg = Config()
    cfg.permissions.approval.wait_timeout_seconds = 1
    gate = _gate(cfg)
    for channel in ("cli", "gateway:cli"):
        check = await gate.check(
            tool_name, {"goal": "x"}, "me", channel=channel,
            event=_event(channel), running=True,
        )
        assert check.denial is None, f"{tool_name} blocked on {channel}"
        assert check.approval_source == APPROVAL_SOURCE_AUTO


@pytest.mark.asyncio
async def test_operator_auto_approve_still_wins() -> None:
    """require=True must not revoke an explicit operator allow rule.

    auto_approve is a stated policy, unlike the default_policy fallback. An
    operator who put a tool there has decided; the gate instructing "this needs
    approval" does not override that.
    """
    cfg = _remote_cfg()
    cfg.permissions.approval.auto_approve = [_ABSENT_EXEC_TOOL]
    gate = _gate(cfg, registry=_Registry({_ABSENT_EXEC_TOOL: _DeclaredRiskTool("exec")}))
    check = await gate.check(
        _ABSENT_EXEC_TOOL, {"target": "/"}, "u1", channel="telegram",
        event=_event(), running=True,
    )
    # Step 6 (explicit auto_approve) passes it before the manual flow is reached.
    assert check.denial is None
    assert check.approval_source == APPROVAL_SOURCE_AUTO


@pytest.mark.asyncio
async def test_persisted_always_grant_still_wins() -> None:
    """A human's earlier "always" for this signature is honoured without
    re-prompting — that consent was real, and re-asking would punish the user
    for having answered."""
    cfg = _remote_cfg()
    allowlist = ApprovalAllowlist()
    gate = _gate(
        cfg,
        registry=_Registry({_ABSENT_EXEC_TOOL: _DeclaredRiskTool("exec")}),
        allowlist=allowlist,
    )
    args = {"target": "/"}
    event = _event()
    from echo_agent.permissions.allowlist import build_pattern_key

    allowlist.approve(
        event.session_key, build_pattern_key(_ABSENT_EXEC_TOOL, args),
        ApprovalLevel.ALWAYS,
    )
    check = await gate.check(
        _ABSENT_EXEC_TOOL, args, "u1", channel="telegram", event=event, running=True,
    )
    assert check.denial is None


@pytest.mark.asyncio
async def test_read_only_and_write_tools_are_unaffected() -> None:
    """Nothing below the EXEC tier acquired a prompt it did not have.

    The change is scoped to calls that already required approval; a WRITE tool on
    a remote channel must still pass without asking, or every IM turn that writes
    a file would now stall.
    """
    cfg = _remote_cfg()
    gate = _gate(cfg)
    for tool_name, args, tier in (
        ("read_file", {"path": "x"}, RiskLevel.READ_ONLY),
        ("write_file", {"path": "x", "content": "y"}, RiskLevel.WRITE),
        ("memory", {}, RiskLevel.WRITE),
    ):
        check = await gate.check(
            tool_name, args, "u1", channel="telegram", event=_event(), running=True,
        )
        assert check.denial is None, f"{tool_name} ({tier.value}) unexpectedly gated"
