"""Re-authorizing an EXISTING scheduled job from a chat channel.

`grant()` had four callers — cronjob's `create`, the CLI, and the REST
POST/PUT — and only the first can be reached from a chat channel. It fires once,
at creation. Everything after that (a job stored before the authorization
contract, a job whose fingerprint an edit invalidated, a grant the dashboard
revoked) had no chat-reachable path back to authorized, because:

  * `echo-agent cron authorize` refuses outright while the gateway holds the
    instance lock (cron_cmd._gateway_is_running), which is exactly when a chat
    user is talking to the agent;
  * the REST endpoint and the dashboard are not reachable from inside a
    conversation.

So the denial produced by _resolve_unattended pointed a chat user at two paths
they cannot take, and the tool offered no third one. These tests span
tool schema → approval gate → grant → delivery, which is the only place where
"a chat user can authorize an existing job" is a testable claim.

Config is built explicitly rather than via load_config(): the shipped defaults
are what make this path interesting (personal_cli + cli_auto_approve), and a
machine-local yaml would decide the outcome instead of the code.
"""
from __future__ import annotations

import asyncio

import pytest

from echo_agent.agent.approval_gate import APPROVAL_SOURCE_HUMAN, ApprovalGate
from echo_agent.agent.tools.cronjob import CronjobTool
from echo_agent.agent.tools.registry import ToolRegistry
from echo_agent.bus.events import ContentBlock, ContentType, InboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.base import SendResult
from echo_agent.config.schema import Config
from echo_agent.permissions.manager import ApprovalManager
from echo_agent.scheduler.authorization import grant, verify
from echo_agent.scheduler.delivery import inbound_event_from_job
from echo_agent.scheduler.service import ScheduledJob, Scheduler, TriggerKind
from echo_agent.tools import ToolExecutionContext


class _FakeInference:
    def needs_confirmation(self, name: str) -> bool:
        return False


def _config() -> Config:
    cfg = Config()
    # manual, not the "smart" default: smart approval would consult a provider
    # we do not have, and the question here is whether a human CAN be asked.
    cfg.permissions.approval.mode = "manual"
    cfg.permissions.approval.unattended_policy = "deny"
    return cfg


def _gate(cfg: Config | None = None, scheduler: Scheduler | None = None) -> ApprovalGate:
    """A gate wired the way AgentLoop wires it (loop.py: registry=self.tools).

    The registry is what lets the prompt ask a tool to describe its own call, so
    a gate built without one cannot render the job behind a job_id. Passing a
    real ToolRegistry holding the real CronjobTool keeps that seam under test
    instead of assuming it.
    """
    cfg = cfg or _config()
    approval = ApprovalManager(
        require_approval=cfg.permissions.approval.require_approval,
        auto_approve=cfg.permissions.approval.auto_approve,
    )
    bus = MessageBus()
    delivered: list = []

    async def _deliver(event):
        delivered.append(event)
        return SendResult(success=True, message_id="prompt-1")

    bus.subscribe_outbound_global(_deliver)
    registry = ToolRegistry()
    registry.register(CronjobTool(scheduler))
    gate = ApprovalGate(
        config=cfg, approval=approval, inference=_FakeInference(),
        bus=bus, provider=None, registry=registry,
    )
    # Tests assert on what the user was shown, so keep the outbound events.
    gate._test_delivered = delivered  # type: ignore[attr-defined]
    return gate


def _scheduler(tmp_path, *, authorized: bool = False) -> tuple[Scheduler, ScheduledJob]:
    """A real Scheduler over a temp store — update_job must actually persist.

    A MagicMock would let a tool that never calls update_job pass: the whole
    point of authorize is that the grant survives into the stored job.
    """
    sched = Scheduler(store_path=tmp_path / "scheduler.json")
    job = ScheduledJob(
        name="beijing-morning-weather-voice",
        trigger=TriggerKind.CRON,
        cron_expr="30 6 * * *",
        payload={
            "command": "播报北京天气并发送语音",
            "deliver_channel": "weixin",
            "deliver_chat_id": "room-1",
        },
    )
    if authorized:
        job.authorization = grant(job, operator="alice", source="chat")
    created = sched.add_job(job)
    return sched, created


# ── The tool must expose the action at all ──────────────────────────────────


def test_authorize_is_an_advertised_action():
    """In the enum, not just implemented.

    The inverse mistake is already recorded in cronjob.py: "update" sat in the
    description with no enum value and no implementation, and the model kept
    attempting it. An implementation the schema does not advertise is the same
    defect facing the other way — the model never learns the action exists.
    """
    enum = CronjobTool.parameters["properties"]["action"]["enum"]
    assert "authorize" in enum
    assert "authorize" in CronjobTool.description


# ── Happy path: a human in chat says yes ────────────────────────────────────


@pytest.mark.asyncio
async def test_chat_user_can_authorize_an_existing_job(tmp_path):
    """The path the denial should have pointed at: gate asks, user approves.

    weixin is not in _INTERACTIVE_CHANNELS, so the gate opens a real pending
    request and waits — this is the only branch that yields
    APPROVAL_SOURCE_HUMAN, which is what the tool requires before granting.
    """
    sched, job = _scheduler(tmp_path)
    assert verify(job) is False

    gate = _gate(scheduler=sched)
    event = InboundEvent(
        channel="weixin", sender_id="alice", chat_id="room-1",
        content=[ContentBlock(type=ContentType.TEXT, text=f"授权一下 {job.id}")],
    )
    arguments = {"action": "authorize", "job_id": job.id}

    async def _run():
        check = await gate.check(
            "cronjob", arguments, "alice",
            channel="weixin", event=event, running=True,
        )
        assert check.denial is None, check.denial and check.denial.error
        ctx = ToolExecutionContext(
            session_key="weixin:room-1", user_id="alice",
            approved_actions=check.approved_actions,
            approval_source=check.approval_source,
            channel="weixin",
        )
        return await CronjobTool(sched).execute(arguments, ctx)

    task = asyncio.create_task(_run())
    for _ in range(200):
        await asyncio.sleep(0.01)
        pending = gate._approval.get_pending()
        if pending:
            assert gate._approval.approve(pending[0].id, decided_by="alice") is True
            break
    else:
        pytest.fail("gate never asked a human to confirm the authorization")

    result = await asyncio.wait_for(task, timeout=5)
    assert result.success is True, result.error

    stored = sched.get_job(job.id)
    assert stored is not None
    assert verify(stored) is True
    # Provenance must name the channel that actually consented. "tui-approval"
    # was written for grants minted from weixin/telegram too, so the audit trail
    # named a surface the operator never touched.
    assert stored.authorization is not None
    assert stored.authorization.source == "chat-approval"
    assert stored.authorization.operator == "alice"
    # And the grant is live for the job as delivery will read it, not merely
    # present on the object.
    assert inbound_event_from_job(stored).cron_authorized is True


@pytest.mark.asyncio
async def test_authorize_prompt_shows_what_is_being_authorized(tmp_path):
    """Consent needs the job's content on screen, not just its id.

    _describe_action's key=value fallback rendered "action=authorize,
    job_id=148fb4a4b9" — the user is asked to allow unattended WRITE/EXEC work
    without being told what runs or where it delivers. The CLI has always shown
    instruction + schedule + target before asking (cron_cmd._describe); the chat
    prompt has to clear the same bar or the confirmation is empty.
    """
    sched, job = _scheduler(tmp_path)
    gate = _gate(scheduler=sched)
    event = InboundEvent(
        channel="weixin", sender_id="alice", chat_id="room-1",
        content=[ContentBlock(type=ContentType.TEXT, text="授权")],
    )
    arguments = {"action": "authorize", "job_id": job.id}

    task = asyncio.create_task(
        gate.check("cronjob", arguments, "alice",
                   channel="weixin", event=event, running=True)
    )
    for _ in range(200):
        await asyncio.sleep(0.01)
        if gate._approval.get_pending():
            break
    else:
        pytest.fail("gate never published an approval prompt")

    prompts = [e for e in gate._test_delivered if "需要确认执行" in (e.text or "")]
    assert prompts, "no approval prompt was delivered"
    body = prompts[0].text
    assert "beijing-morning-weather-voice" in body
    assert "播报北京天气并发送语音" in body
    assert "30 6 * * *" in body
    assert "weixin:room-1" in body

    for req in gate._approval.get_pending():
        gate._approval.deny(req.id, reason="test teardown", decided_by="test")
    await asyncio.wait_for(task, timeout=5)


# ── Fail-closed: no human, no grant ─────────────────────────────────────────


def test_describe_hook_tolerates_tools_that_do_not_implement_it():
    """MCP adapters and third-party tools are duck-typed, not Tool subclasses.

    The prompt must degrade to the generic argument rendering for them. Getting
    this wrong took out three unrelated approval tests: a tool without the hook
    raised inside the prompt builder, so an EXEC call that should have been
    *asked about* failed instead — a display concern breaking a security path,
    which is exactly what this seam must never do.
    """
    class _NoHook:
        name = "mcp_delete_everything"
        risk_level = "dangerous"

    gate = _gate()
    gate._registry = type("_R", (), {"get": staticmethod(lambda _n: _NoHook())})()
    assert gate._describe_via_tool("mcp_delete_everything", {"target": "/"}) == ""
    # And the generic path still describes the call.
    rendered = gate._describe_action("mcp_delete_everything", {"target": "/"})
    assert "target=/" in rendered


def test_describe_hook_survives_a_raising_tool():
    """A tool whose describe_for_approval raises must not cost the user a prompt."""
    class _Boom:
        name = "cronjob"

        def describe_for_approval(self, arguments):
            raise RuntimeError("store unreadable")

    gate = _gate()
    gate._registry = type("_R", (), {"get": staticmethod(lambda _n: _Boom())})()
    assert gate._describe_via_tool("cronjob", {"action": "authorize"}) == ""


def test_describe_hook_bounds_a_hostile_description():
    """The instruction is user-supplied text; it cannot be allowed to flood a
    chat message or crowd the /approve lines out of the prompt."""
    class _Flood:
        name = "cronjob"

        def describe_for_approval(self, arguments):
            return "标题\n" + "\n".join(f"字段{i}: " + "x" * 500 for i in range(40))

    gate = _gate()
    gate._registry = type("_R", (), {"get": staticmethod(lambda _n: _Flood())})()
    out = gate._describe_via_tool("cronjob", {})
    assert len(out.splitlines()) <= gate._APPROVAL_FIELDS_MAX
    assert all(len(line) <= gate._APPROVAL_FIELD_MAX + 10 for line in out.splitlines())


@pytest.mark.asyncio
async def test_authorize_refuses_without_human_consent(tmp_path):
    """Defense in depth, below the gate.

    The DANGEROUS tier already routes this to a real prompt (cli_auto_approve
    excludes DANGEROUS), so the gate is the primary control. This second check
    exists for the configurations that can still reach execute() without a
    person — `cronjob` added to auto_approve, mode="off", a trusted channel —
    none of which is consent to hand a job standing unattended privileges.
    """
    sched, job = _scheduler(tmp_path)
    result = await CronjobTool(sched).execute(
        {"action": "authorize", "job_id": job.id},
        ToolExecutionContext(session_key="weixin:room-1", user_id="alice",
                             approval_source="auto"),
    )
    assert result.success is False
    assert result.error_kind == "business"
    assert verify(sched.get_job(job.id)) is False


@pytest.mark.asyncio
async def test_authorize_cannot_be_self_granted_by_a_running_job(tmp_path):
    """A fired job must not be able to authorize itself or any other job.

    This is the recursion/privilege-escalation vector _resolve_unattended
    already refuses DANGEROUS for. Asserted here as well because `authorize` is
    a far more attractive target than `create`: one call converts an
    unauthorized job into a permanently authorized one.
    """
    sched, job = _scheduler(tmp_path, authorized=True)
    _, victim = _scheduler(tmp_path / "other", authorized=False)

    gate = _gate(scheduler=sched)
    fired = inbound_event_from_job(sched.get_job(job.id))
    assert fired.unattended is True
    assert fired.cron_authorized is True

    check = await gate.check(
        "cronjob", {"action": "authorize", "job_id": victim.id}, "cron",
        channel=fired.channel, event=fired, running=True,
    )
    assert check.denial is not None
    assert verify(victim) is False


@pytest.mark.asyncio
async def test_authorize_reports_unknown_job(tmp_path):
    sched, _ = _scheduler(tmp_path)
    result = await CronjobTool(sched).execute(
        {"action": "authorize", "job_id": "does-not-exist"},
        ToolExecutionContext(user_id="alice", approval_source=APPROVAL_SOURCE_HUMAN),
    )
    assert result.success is False
    assert "does-not-exist" in result.error


@pytest.mark.asyncio
async def test_authorize_requires_job_id(tmp_path):
    sched, _ = _scheduler(tmp_path)
    result = await CronjobTool(sched).execute(
        {"action": "authorize"},
        ToolExecutionContext(user_id="alice", approval_source=APPROVAL_SOURCE_HUMAN),
    )
    assert result.success is False
    assert result.error_kind == "validation"


# ── Re-authorization after an edit invalidates the fingerprint ───────────────


@pytest.mark.asyncio
async def test_authorize_revives_a_job_whose_grant_went_stale(tmp_path):
    """The common operational case, and the one with no chat path before.

    Editing a job's instruction/schedule/target deliberately invalidates its
    grant (authorization.compute_fingerprint). That is correct, but it means
    "stale grant" is a routine state, and re-consenting was only possible from
    the CLI (blocked while the gateway runs), REST, or the dashboard.
    """
    sched, job = _scheduler(tmp_path, authorized=True)
    assert verify(sched.get_job(job.id)) is True

    sched.update_job(job.id, payload={
        **job.payload, "command": "播报北京天气并发送语音（改为含空气质量）",
    })
    stale = sched.get_job(job.id)
    assert stale.authorization is not None
    assert verify(stale) is False  # present but no longer valid

    result = await CronjobTool(sched).execute(
        {"action": "authorize", "job_id": job.id},
        ToolExecutionContext(session_key="weixin:room-1", user_id="alice",
                             approval_source=APPROVAL_SOURCE_HUMAN),
    )
    assert result.success is True, result.error
    revived = sched.get_job(job.id)
    assert verify(revived) is True
    # Bound to the NEW content, not re-signed against what was consented before.
    assert revived.authorization.summary.startswith("播报北京天气并发送语音（改为含空气质量）")


@pytest.mark.asyncio
async def test_revoke_from_chat_clears_the_grant(tmp_path):
    """Withdrawing consent must be reachable wherever granting it is.

    An authorize-only tool makes the grant one-way from chat: the CLI refuses
    while the gateway runs, so a user who authorized a job by mistake would have
    to stop the service to take it back.
    """
    sched, job = _scheduler(tmp_path, authorized=True)
    assert verify(sched.get_job(job.id)) is True

    result = await CronjobTool(sched).execute(
        {"action": "revoke", "job_id": job.id},
        ToolExecutionContext(session_key="weixin:room-1", user_id="alice",
                             approval_source=APPROVAL_SOURCE_HUMAN),
    )
    assert result.success is True, result.error
    cleared = sched.get_job(job.id)
    assert cleared.authorization is None
    assert verify(cleared) is False


# ── The denial must not advertise paths the user cannot take ─────────────────


def test_unattended_denial_offers_the_chat_path_first():
    """The hint is produced inside the approval gate, which cannot know whether
    the gateway holds the instance lock — and it must not learn: coupling a
    security decision to process-state probing is worse than the wrong advice.

    So the fix is for every listed path to be true unconditionally. `echo-agent
    cron authorize` is only true with the service stopped, and the denial
    itself proves the service is running, so it may only appear with that
    precondition attached.
    """
    from echo_agent.security.risk_classifier import RiskLevel

    gate = _gate()
    event = InboundEvent(
        channel="cron", sender_id="cron", chat_id="cron",
        content=[ContentBlock(type=ContentType.TEXT, text="播报")],
        metadata={"job_id": "148fb4a4b9", "job_name": "beijing-morning-weather-voice"},
    )
    hint = gate._unattended_hint(RiskLevel.WRITE, event, authorized=False)

    assert "148fb4a4b9" in hint
    # The chat path exists and comes first: it is the only one that works while
    # the agent is running, which is always the case when this hint is emitted.
    assert "授权" in hint
    chat_pos = hint.find("对话")
    cli_pos = hint.find("echo-agent cron authorize")
    assert chat_pos != -1, "denial does not mention the in-conversation path"
    assert cli_pos == -1 or chat_pos < cli_pos, "CLI advice precedes the working path"
    # If the CLI is still mentioned it must carry its precondition.
    if cli_pos != -1:
        assert "停止" in hint


def test_stale_grant_denial_also_offers_the_chat_path():
    """`authorized=True` means a grant exists but no longer verifies — the exact
    state an edit produces, and the one most likely to need re-consent."""
    from echo_agent.security.risk_classifier import RiskLevel

    gate = _gate()
    event = InboundEvent(
        channel="cron", sender_id="cron", chat_id="cron",
        content=[ContentBlock(type=ContentType.TEXT, text="播报")],
        metadata={"job_id": "148fb4a4b9", "job_name": "morning"},
    )
    hint = gate._unattended_hint(RiskLevel.WRITE, event, authorized=True)
    assert "148fb4a4b9" in hint
    assert "对话" in hint


def test_dangerous_denial_still_refuses_to_suggest_authorizing():
    """An authorized job is still denied DANGEROUS work, so pointing at any
    authorization path there would be false advice. Unchanged behaviour, pinned
    because the copy around it is being rewritten."""
    from echo_agent.security.risk_classifier import RiskLevel

    gate = _gate()
    event = InboundEvent(
        channel="cron", sender_id="cron", chat_id="cron",
        content=[ContentBlock(type=ContentType.TEXT, text="播报")],
        metadata={"job_id": "148fb4a4b9", "job_name": "morning"},
    )
    hint = gate._unattended_hint(RiskLevel.DANGEROUS, event, authorized=False)
    assert "echo-agent cron authorize" not in hint
    assert "对话" not in hint


def test_create_without_consent_points_at_the_chat_path_too():
    """cronjob.create's own fallback text had the same two dead ends.

    Asserted on the class docstring-level constant rather than by running
    create, so the message stays reviewable in one place.
    """
    from echo_agent.agent.tools.cronjob import UNAUTHORIZED_HINT

    assert "对话" in UNAUTHORIZED_HINT
    cli_pos = UNAUTHORIZED_HINT.find("echo-agent cron authorize")
    if cli_pos != -1:
        assert "停止" in UNAUTHORIZED_HINT
