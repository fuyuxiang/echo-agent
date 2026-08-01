# tests/test_worker_security_context_inheritance.py
"""A delegated worker must not reach further than whoever dispatched it.

The worker executor (agent/tools/delegate.py) runs each worker tool call through
the real ApprovalGate, but used to call it with ``channel=""`` and
``event=None``. Both are lies about the caller:

  - ``""`` is in ``ApprovalGate._INTERACTIVE_CHANNELS``, so the gate read it as a
    local human-at-the-keyboard session and applied ``cli_auto_approve``. A
    worker's ``exec`` was therefore auto-approved even when the parent call came
    from telegram and would itself have required consent — delegating a command
    was a cheaper route to running one than running it.
  - ``event=None`` dropped ``unattended`` / ``cron_authorized``, the two typed
    trust fields on InboundEvent. A scheduled job's worker looked interactive
    rather than unattended, so the per-job authorization that gates the job's own
    work did not gate its worker's work.

The dispatch is gated too (delegate_task/spawn_task are EXEC-tier), but that only
bounds who may dispatch; it says nothing about what the worker may then do. These
tests pin the inheritance itself.

They call the gate the way the worker executor does, rather than driving a real
LLM worker loop: the security decision is what is under test, and routing it
through a live provider would test the provider.
"""
from __future__ import annotations

import pytest

from echo_agent.agent.approval_gate import (
    APPROVAL_SOURCE_AUTO,
    ApprovalGate,
    _NESTED_CHANNEL,
)
from echo_agent.bus.events import ContentBlock, ContentType, InboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.config.schema import Config
from echo_agent.permissions.manager import ApprovalManager
from echo_agent.tools.base import ToolExecutionContext


class _FakeInference:
    def needs_confirmation(self, name: str) -> bool:
        return False


def _gate(cfg: Config) -> ApprovalGate:
    appr = ApprovalManager(
        require_approval=cfg.permissions.approval.require_approval,
        auto_approve=cfg.permissions.approval.auto_approve,
    )
    return ApprovalGate(
        config=cfg, approval=appr, inference=_FakeInference(),
        bus=MessageBus(), provider=None,
    )


def _cfg(**approval_overrides) -> Config:
    cfg = Config()
    # Short, so a test that DOES reach a wait cannot hang the suite for 300s.
    cfg.permissions.approval.wait_timeout_seconds = 1
    for key, value in approval_overrides.items():
        setattr(cfg.permissions.approval, key, value)
    return cfg


async def _as_worker(gate: ApprovalGate, parent: ToolExecutionContext, tool: str, args: dict):
    """Exactly the call the worker executor makes — see delegate.py:_execute."""
    return await gate.check(
        tool, args, parent.user_id,
        channel=parent.channel,
        event=None,
        running=True,
        unattended=parent.unattended,
        cron_authorized=parent.cron_authorized,
        nested=True,
    )


@pytest.mark.asyncio
async def test_worker_exec_refused_when_parent_came_from_remote_channel() -> None:
    """The original hole: telegram → delegate → worker exec."""
    gate = _gate(_cfg())
    parent = ToolExecutionContext(user_id="u1", channel="telegram", chat_id="c1")
    check = await _as_worker(gate, parent, "exec", {"command": "id"})
    assert check.denial is not None
    assert check.denial.metadata.get("nested_approval_refused") is True


@pytest.mark.asyncio
async def test_worker_exec_refused_on_local_cli_too() -> None:
    """cli_auto_approve is about a human at a keyboard; a worker has none.

    This is a deliberate tightening: before, a worker inherited the local user's
    auto-approve. It now refuses instead — immediately and with a message that
    says where to run the command, rather than stalling on a prompt nobody can
    see (see test_worker_refusal_is_immediate_not_a_timeout).
    """
    gate = _gate(_cfg())
    for channel in ("cli", "gateway:cli", ""):
        parent = ToolExecutionContext(user_id="me", channel=channel, chat_id="local")
        check = await _as_worker(gate, parent, "exec", {"command": "id"})
        assert check.denial is not None, f"worker exec auto-approved on {channel!r}"


@pytest.mark.asyncio
async def test_worker_refusal_is_immediate_not_a_timeout() -> None:
    """The refusal must not be a 300-second wait dressed up as a decision.

    A nested call has no event, so the gate cannot publish an approval request
    and nobody can answer one. Routing it into _manual_approval_flow anyway would
    park the worker for wait_timeout_seconds and then fail — a long unexplained
    stall. Asserting on the message distinguishes "refused by design" from
    "timed out waiting for a prompt that was never delivered".
    """
    gate = _gate(_cfg(wait_timeout_seconds=300))
    parent = ToolExecutionContext(user_id="u1", channel="telegram", chat_id="c1")
    check = await _as_worker(gate, parent, "exec", {"command": "id"})
    assert check.denial is not None
    assert "timed out" not in (check.denial.error or "").lower()
    assert check.denial.metadata.get("nested_approval_refused") is True


@pytest.mark.asyncio
async def test_worker_inherits_unattended_from_scheduled_parent() -> None:
    """allow_safe + no per-job grant: the worker is refused, like the job itself."""
    gate = _gate(_cfg(unattended_policy="allow_safe"))
    parent = ToolExecutionContext(
        user_id="s", channel="cron", chat_id="cron:j1", unattended=True,
    )
    check = await _as_worker(gate, parent, "exec", {"command": "id"})
    assert check.denial is not None
    assert "unattended" in (check.denial.error or "").lower()


@pytest.mark.asyncio
async def test_worker_inherits_cron_authorization_when_present() -> None:
    """An authorized job's grant DOES extend to its worker's EXEC work.

    The grant is the human's up-front consent for this job's work, and the worker
    is how the job does that work. Refusing here would make authorized jobs
    unable to delegate at all — the fix must not overshoot into that.
    """
    gate = _gate(_cfg())
    parent = ToolExecutionContext(
        user_id="s", channel="cron", chat_id="cron:j1",
        unattended=True, cron_authorized=True,
    )
    check = await _as_worker(gate, parent, "exec", {"command": "id"})
    assert check.denial is None
    assert check.approval_source == APPROVAL_SOURCE_AUTO


@pytest.mark.asyncio
async def test_worker_cannot_manufacture_its_own_authorization() -> None:
    """A blank parent context grants nothing.

    The conservative default matters: a caller that says nothing about trust must
    not be read as authorized. This is the same reason the fields live on the
    typed context rather than in metadata.
    """
    gate = _gate(_cfg(unattended_policy="allow_safe"))
    parent = ToolExecutionContext(user_id="s", channel="cron", chat_id="cron:j1")
    # unattended defaults False here, so the cron *channel* fallback is what
    # still marks it unattended — and with no grant it must be refused.
    check = await _as_worker(gate, parent, "exec", {"command": "id"})
    assert check.denial is not None


@pytest.mark.asyncio
async def test_worker_read_only_and_write_still_pass() -> None:
    """The tightening is scoped to calls that need approval.

    A worker doing research or writing its findings must stay unimpeded, or
    delegation becomes useless for its main purpose. Note web_fetch is absent on
    purpose: the default ``network_policy="deny"`` has the Step-1 static guard
    block it for everyone, nested or not, so it says nothing about inheritance.
    """
    gate = _gate(_cfg())
    parent = ToolExecutionContext(user_id="u1", channel="telegram", chat_id="c1")
    for tool, args in (
        ("read_file", {"path": "x"}),
        ("search_files", {"pattern": "x"}),
        ("write_file", {"path": "x", "content": "y"}),
        ("memory", {}),
    ):
        check = await _as_worker(gate, parent, tool, args)
        assert check.denial is None, f"{tool} unexpectedly refused for a worker"


@pytest.mark.asyncio
async def test_trusted_channel_does_not_extend_to_workers() -> None:
    """trusted_channels is an interactive convenience, not a worker grant."""
    gate = _gate(_cfg(trusted_channels=["telegram"]))
    parent = ToolExecutionContext(user_id="u1", channel="telegram", chat_id="c1")
    check = await _as_worker(gate, parent, "exec", {"command": "id"})
    assert check.denial is not None


@pytest.mark.asyncio
async def test_parent_turn_itself_is_unaffected() -> None:
    """Only nested calls change. A real turn with an event still behaves as before.

    Pinned because the fix threads a new parameter through the whole gate: if it
    leaked into the non-nested path, every interactive exec would start refusing.
    """
    gate = _gate(_cfg())
    event = InboundEvent(
        channel="cli", sender_id="me", chat_id="local",
        content=[ContentBlock(type=ContentType.TEXT, text="run it")],
    )
    check = await gate.check(
        "exec", {"command": "id"}, "me", channel="cli", event=event, running=True,
    )
    assert check.denial is None
    assert check.approval_source == APPROVAL_SOURCE_AUTO


@pytest.mark.asyncio
async def test_nested_channel_sentinel_is_not_interactive_or_trusted() -> None:
    """The sentinel must not accidentally match any pass-through set.

    If someone later adds "worker:nested" to trusted_channels or the interactive
    set, the whole containment silently reopens — so assert the property directly.
    """
    assert _NESTED_CHANNEL not in ApprovalGate._INTERACTIVE_CHANNELS
    cfg = _cfg(trusted_channels=[_NESTED_CHANNEL])
    gate = _gate(cfg)
    # Even if an operator lists the sentinel, a worker exec is still refused:
    # the sentinel is an internal marker, not a configurable channel.
    parent = ToolExecutionContext(user_id="u1", channel="telegram", chat_id="c1")
    check = await _as_worker(gate, parent, "exec", {"command": "id"})
    assert check.denial is not None
