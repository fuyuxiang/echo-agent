import pytest

from echo_agent.agent.clarify_manager import ClarifyManager


class _FakeCog:
    def __init__(self):
        self.emitted = []

    @staticmethod
    def active(event):
        return event.channel == "gateway:cli"

    async def emit(self, event, cog_type, data, summary):
        self.emitted.append((cog_type, data, summary))


class _FakeToolCall:
    def __init__(self, name, arguments):
        self.name = name
        self.arguments = arguments
        self.id = "tc1"


class _FakeEvent:
    channel = "gateway:cli"
    chat_id = "c"
    event_id = "in_1"
    reply_to_id = ""
    sender_id = "u1"


def _make_stage(clarify_manager, cog):
    # Build a bare InferenceStage without running __init__ (it needs many deps);
    # set only the attributes _prepare_clarify touches.
    from echo_agent.agent.pipeline.inference_stage import InferenceStage
    stage = InferenceStage.__new__(InferenceStage)
    stage._clarify = clarify_manager
    stage._cog = cog
    return stage


@pytest.mark.asyncio
async def test_prepare_clarify_registers_emits_and_injects_id():
    mgr = ClarifyManager()
    cog = _FakeCog()
    stage = _make_stage(mgr, cog)
    tc = _FakeToolCall("clarify", {"question": "选哪个?", "options": ["A", "B"]})

    await stage._prepare_clarify(tc, _FakeEvent())

    # id injected into arguments
    cid = tc.arguments.get("_clarify_id")
    assert cid
    # registered as pending
    assert mgr.get(cid) is not None
    # frame emitted with matching data
    assert len(cog.emitted) == 1
    cog_type, data, _summary = cog.emitted[0]
    assert cog_type == "clarify_request"
    assert data["clarify_id"] == cid
    assert data["question"] == "选哪个?"
    assert data["options"] == ["A", "B"]


@pytest.mark.asyncio
async def test_prepare_clarify_noop_for_non_clarify_tool():
    mgr = ClarifyManager()
    cog = _FakeCog()
    stage = _make_stage(mgr, cog)
    tc = _FakeToolCall("read_file", {"path": "x"})
    await stage._prepare_clarify(tc, _FakeEvent())
    assert "_clarify_id" not in tc.arguments
    assert cog.emitted == []


@pytest.mark.asyncio
async def test_prepare_clarify_noop_on_non_cli_channel():
    mgr = ClarifyManager()
    cog = _FakeCog()
    stage = _make_stage(mgr, cog)
    tc = _FakeToolCall("clarify", {"question": "q", "options": ["A", "B"]})

    class _IMEvent(_FakeEvent):
        channel = "wecom"
        session_key = "wecom:u1"

    await stage._prepare_clarify(tc, _IMEvent())
    # IM channel: no id injected into the tool, no cognitive frame (TUI-only)...
    assert "_clarify_id" not in tc.arguments
    assert cog.emitted == []
    # ...but the question is remembered per session so the next inbound message
    # can be routed to it as the answer (IM follow-up continuation).
    pending = mgr.take_im_pending("wecom:u1", ttl_seconds=300)
    assert pending is not None
    assert pending.question == "q"
    assert pending.options == ["A", "B"]


@pytest.mark.asyncio
async def test_prepare_clarify_im_pending_not_registered_without_session_key():
    mgr = ClarifyManager()
    cog = _FakeCog()
    stage = _make_stage(mgr, cog)
    tc = _FakeToolCall("clarify", {"question": "q"})

    class _IMEvent(_FakeEvent):
        channel = "wecom"
        session_key = ""

    await stage._prepare_clarify(tc, _IMEvent())
    assert mgr.take_im_pending("", ttl_seconds=300) is None
