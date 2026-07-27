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


@pytest.mark.asyncio
async def test_finish_clarify_emits_closed_frame_for_prepared_prompt():
    """工具调用返回后必须发一帧 clarify_closed —— 客户端唯一可信的"该提问已死"信号。

    CLI clarify 没有超时,唯一解除是 resolve(),所以客户端无法从别处推断;过去它
    靠中途的 is_final 帧猜,结果在提问还活着时就把选项弄哑了。"""
    mgr = ClarifyManager()
    cog = _FakeCog()
    stage = _make_stage(mgr, cog)
    tc = _FakeToolCall("clarify", {"question": "选哪个?", "options": ["A", "B"]})

    await stage._prepare_clarify(tc, _FakeEvent())
    cid = tc.arguments["_clarify_id"]
    await stage._finish_clarify(tc, _FakeEvent())

    assert [c for c, _d, _s in cog.emitted] == ["clarify_request", "clarify_closed"]
    _cog_type, data, _summary = cog.emitted[-1]
    assert data == {"clarify_id": cid}


@pytest.mark.asyncio
async def test_finish_clarify_noop_for_non_clarify_and_unprepared():
    """非 clarify 工具、以及没走过 _prepare_clarify(无 _clarify_id)的调用都不发帧。"""
    mgr = ClarifyManager()
    cog = _FakeCog()
    stage = _make_stage(mgr, cog)

    await stage._finish_clarify(_FakeToolCall("read_file", {"path": "x"}), _FakeEvent())
    # clarify 但没有注入过 id(例如 IM 通道走过 _prepare_clarify 的分支)
    await stage._finish_clarify(_FakeToolCall("clarify", {"question": "q"}), _FakeEvent())
    assert cog.emitted == []


@pytest.mark.asyncio
async def test_finish_clarify_noop_on_non_cli_channel():
    """只对 CLI 发,与 _prepare_clarify 的通道条件保持一致。"""
    mgr = ClarifyManager()
    cog = _FakeCog()
    stage = _make_stage(mgr, cog)
    tc = _FakeToolCall("clarify", {"question": "q", "_clarify_id": "c1"})

    class _IMEvent(_FakeEvent):
        channel = "wecom"
        session_key = "wecom:u1"

    await stage._finish_clarify(tc, _IMEvent())
    assert cog.emitted == []


@pytest.mark.asyncio
async def test_finish_clarify_swallows_emit_failure():
    """这一帧是尽力而为:发送失败不能把一次本来成功的工具调用带崩。"""
    mgr = ClarifyManager()

    class _BoomCog(_FakeCog):
        async def emit(self, event, cog_type, data, summary):
            raise RuntimeError("transport gone")

    stage = _make_stage(mgr, _BoomCog())
    tc = _FakeToolCall("clarify", {"question": "q", "_clarify_id": "c1"})
    await stage._finish_clarify(tc, _FakeEvent())  # 不抛异常即通过
