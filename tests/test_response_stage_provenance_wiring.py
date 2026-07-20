"""Final-review Fix: prove the production path carries provenance.

The bug: ResponseStage._background_skill_review was constructed without
session_key/channel, so created_from_session/channel were ALWAYS empty under
real traffic even though SkillReviewer/SkillAdmission/SkillCandidate all support
them. These tests are load-bearing — they fail if the wiring regresses.

Two levels:
1. reviewer level — created_from_session/channel actually land on the staged
   candidate (the core of the issue: these values can truly reach the row).
2. finalize wiring — ResponseStage.finalize spawns _background_skill_review with
   event.session_key/event.channel (guards Fix1(b)), plus a signature guard for
   Fix1(a).
"""

import inspect
from unittest.mock import MagicMock

import pytest
import pytest_asyncio

from echo_agent.agent.pipeline.response_stage import ResponseStage
from echo_agent.agent.pipeline.types import InferenceResult, PipelineContext
from echo_agent.bus.events import InboundEvent
from echo_agent.evolution.store import TrajectoryStore
from echo_agent.session.manager import Session
from echo_agent.skills.admission import SkillAdmission
from echo_agent.skills.reviewer import SkillReviewer
from echo_agent.skills.store import SkillStore
from echo_agent.storage.sqlite import SQLiteBackend


@pytest_asyncio.fixture
async def admission(tmp_path):
    backend = SQLiteBackend(tmp_path / "s.db")
    await backend.initialize()
    cstore = TrajectoryStore(backend)
    await cstore.init_schema()
    sstore = SkillStore(user_dir=tmp_path / "skills")
    adm = SkillAdmission(skill_store=sstore, candidate_store=cstore,
                         policy="stage_for_review", auto_write_risk="low")
    yield adm, sstore
    await backend.close()


# --- Level 1: provenance truly reaches the staged candidate -----------------
@pytest.mark.asyncio
async def test_reviewer_provenance_lands_on_candidate(admission):
    adm, _sstore = admission
    reviewer = SkillReviewer(
        provider=None, store=_sstore, admission=adm,
        session_key="tg:123", channel="telegram",
    )
    # create is high-risk → staged; the row must carry provenance.
    await reviewer._handle_skill_manage({
        "action": "create", "name": "newskill",
        "content": "---\nname: newskill\ndescription: d\n---\nbody",
    })
    staged = await adm.list_staged()
    assert len(staged) == 1
    assert staged[0].created_from_session == "tg:123"
    assert staged[0].channel == "telegram"


# --- Level 2a: Fix1(a) signature guard --------------------------------------
def test_background_skill_review_signature_has_provenance_params():
    params = inspect.signature(ResponseStage._background_skill_review).parameters
    assert "session_key" in params
    assert "channel" in params


# --- Level 2b: Fix1(b) finalize spawns review with event provenance ---------
class _FakeSessions:
    async def save(self, session):
        return None


class _FakeMemory:
    def has_pending_embeds(self):
        return False


@pytest.mark.asyncio
async def test_finalize_spawns_skill_review_with_event_provenance():
    captured = {}

    # Stub _background_skill_review to record what finalize passes in.
    def _spy(self, messages, session_key="", channel=""):
        captured["session_key"] = session_key
        captured["channel"] = channel
        return None  # spawn_fn is a no-op; no coroutine needed.

    spawned = []

    def _spawn_fn(item, **kwargs):
        spawned.append(item)

    rs = ResponseStage(
        config=None,
        sessions=_FakeSessions(),
        memory=_FakeMemory(),
        provider=None,
        consolidation_worker=object(),  # no _consolidator → consolidation skipped
        default_model="",
        spawn_fn=_spawn_fn,
        clear_memory_snapshot_fn=lambda *a, **k: None,
        skill_store=object(),
        skill_admission=object(),
    )
    rs._background_skill_review = _spy.__get__(rs, ResponseStage)

    event = InboundEvent.text_message(
        channel="telegram", sender_id="u1", chat_id="123", text="hi",
    )
    session = Session(key=event.session_key)
    ctx = PipelineContext(
        event=event, session=session, trace_id="t", publish_response=False,
        messages=[{"role": "user", "content": "hi"}],
    )
    result = InferenceResult(
        response_text="ok", total_tool_calls=2,
        should_review_skills=True, should_review_memory=False,
    )

    await rs.finalize(ctx, result)

    assert captured.get("session_key") == "telegram:123"
    assert captured.get("channel") == "telegram"


# --- E4: memory review dispatched DURABLE (not the default DISCARDABLE) ------
class _RecordingSessions:
    """SessionManager stand-in: hands back one session, records saves, and
    exposes a real per-session lock so the counter-reset path is exercised."""

    def __init__(self, session):
        self._session = session
        self.save_calls = 0

    async def acquire(self, key):
        import asyncio
        return asyncio.Lock()

    async def get_or_create(self, key):
        return self._session

    async def save(self, session):
        self.save_calls += 1


@pytest.mark.asyncio
async def test_memory_review_dispatched_durable():
    """派发 memory review 必须带 tier=Tier.DURABLE:默认 DISCARDABLE 会在调度器
    饱和时被静默丢弃(且不重试),这批记忆的复审就永久丢失。"""
    from echo_agent.agent.background import Tier

    spawned = []

    def _spawn_fn(item, **kwargs):
        spawned.append((item, kwargs.get("tier")))

    called = {}

    def _fake_review(self, messages, session_key, memory_scope=""):
        called["args"] = (session_key, memory_scope)
        return None  # factory result unused by the recording spawn_fn

    rs = ResponseStage(
        config=None,
        sessions=_FakeSessions(),
        memory=_FakeMemory(),  # has_pending_embeds() → False, so no flush spawn
        provider=None,
        consolidation_worker=object(),  # no _consolidator → consolidation skipped
        default_model="",
        spawn_fn=_spawn_fn,
        clear_memory_snapshot_fn=lambda *a, **k: None,
    )
    rs._background_memory_review = _fake_review.__get__(rs, ResponseStage)

    event = InboundEvent.text_message(
        channel="telegram", sender_id="u1", chat_id="123", text="hi",
    )
    session = Session(key=event.session_key)
    ctx = PipelineContext(
        event=event, session=session, trace_id="t", publish_response=False,
        messages=[{"role": "user", "content": "hi"}],
    )
    result = InferenceResult(
        response_text="ok", total_tool_calls=0,
        should_review_skills=False, should_review_memory=True,
    )

    await rs.finalize(ctx, result)

    # Only the memory review should be scheduled here, and it must be DURABLE.
    assert len(spawned) == 1
    item, tier = spawned[0]
    assert tier == Tier.DURABLE
    # DURABLE retry needs a re-callable factory (a bare coroutine cannot be
    # re-awaited). Prove the spawned item is a factory that invokes the review.
    item()
    assert called["args"] == (event.session_key, event.memory_scope)


# --- E4: nudge counters cleared only after a review SUCCEEDS -----------------
@pytest.mark.asyncio
async def test_memory_counter_cleared_only_on_review_success():
    """成功回调才清 review 计数:成功后清零并写回 session;失败(抛异常,交给
    DURABLE 重试)则计数保留、下轮仍触发,避免这批记忆永不复审。"""
    from unittest.mock import AsyncMock, patch

    # --- success path: counters reset to 0, session persisted ---
    session = Session(key="tg:9")
    session.metadata["_nudge_turns_memory"] = 5
    session.metadata["_nudge_tool_iters_memory"] = 3
    sessions = _RecordingSessions(session)

    rs = ResponseStage(
        config=None,
        sessions=sessions,
        memory=_FakeMemory(),
        provider=None,
        consolidation_worker=object(),
        default_model="",
        spawn_fn=lambda *a, **k: None,
        clear_memory_snapshot_fn=AsyncMock(),
        memory_service=object(),  # non-None → skip inline MemoryService build
    )

    ok_reviewer = MagicMock()
    ok_reviewer.review = AsyncMock(return_value=["memory: added x"])
    with patch("echo_agent.memory.reviewer.MemoryReviewer", return_value=ok_reviewer):
        await rs._background_memory_review([{"role": "user", "content": "hi"}], "tg:9", "tg:9")

    assert session.metadata["_nudge_turns_memory"] == 0
    assert session.metadata["_nudge_tool_iters_memory"] == 0
    assert sessions.save_calls >= 1

    # --- failure path: review raises → counters untouched, exception propagates ---
    session2 = Session(key="tg:9")
    session2.metadata["_nudge_turns_memory"] = 5
    session2.metadata["_nudge_tool_iters_memory"] = 3
    sessions2 = _RecordingSessions(session2)

    rs2 = ResponseStage(
        config=None,
        sessions=sessions2,
        memory=_FakeMemory(),
        provider=None,
        consolidation_worker=object(),
        default_model="",
        spawn_fn=lambda *a, **k: None,
        clear_memory_snapshot_fn=AsyncMock(),
        memory_service=object(),
    )

    bad_reviewer = MagicMock()
    bad_reviewer.review = AsyncMock(side_effect=RuntimeError("review boom"))
    with patch("echo_agent.memory.reviewer.MemoryReviewer", return_value=bad_reviewer):
        with pytest.raises(RuntimeError):
            await rs2._background_memory_review([{"role": "user", "content": "hi"}], "tg:9", "tg:9")

    assert session2.metadata["_nudge_turns_memory"] == 5
    assert session2.metadata["_nudge_tool_iters_memory"] == 3


# --- R4 Task6 Important 1: reset/save 失败不得触发昂贵的 review 重跑 -----------
class _SaveFailsSessions:
    """SessionManager 替身:提供真锁与固定 session,但 save 恒抛异常——模拟
    metadata 持久化失败(磁盘满/存储抖动)。"""

    def __init__(self, session):
        self._session = session
        self.save_calls = 0

    async def acquire(self, key):
        import asyncio
        return asyncio.Lock()

    async def get_or_create(self, key):
        return self._session

    async def save(self, session):
        self.save_calls += 1
        raise RuntimeError("disk full")


@pytest.mark.asyncio
async def test_reset_save_failure_does_not_rerun_review():
    """计数清零后的 save 失败绝不能把整轮 review 当作失败重跑。

    review 成功后 _reset_memory_nudge_counters 里的 save 若抛异常并向上传播,
    会被 DURABLE _run_durable 当成 review 失败,按 factory 重跑整段 review
    (昂贵的 LLM 调用 + 可能重复写入记忆)。修法:reset 内部吞掉 save 失败,
    只 warning,不向上抛——所以 DURABLE 不会重试,review 只跑一次。"""
    from unittest.mock import AsyncMock, patch

    from echo_agent.agent.background import BackgroundScheduler, Tier

    session = Session(key="tg:9")
    session.metadata["_nudge_turns_memory"] = 5
    session.metadata["_nudge_tool_iters_memory"] = 3
    sessions = _SaveFailsSessions(session)

    rs = ResponseStage(
        config=None,
        sessions=sessions,
        memory=_FakeMemory(),
        provider=None,
        consolidation_worker=object(),
        default_model="",
        spawn_fn=lambda *a, **k: None,
        clear_memory_snapshot_fn=AsyncMock(),
        memory_service=object(),  # non-None → skip inline MemoryService build
    )

    reviewer = MagicMock()
    reviewer.review = AsyncMock(return_value=["memory: added x"])

    scheduler = BackgroundScheduler(max_concurrency=2)
    with patch("echo_agent.memory.reviewer.MemoryReviewer", return_value=reviewer):
        scheduler.spawn(
            lambda: rs._background_memory_review(
                [{"role": "user", "content": "hi"}], "tg:9", "tg:9",
            ),
            tier=Tier.DURABLE,
        )
        await scheduler.aclose()

    # save 确实被调到并失败,但昂贵的 review 只允许跑一次——不因 save 失败重跑。
    assert reviewer.review.await_count == 1
    assert sessions.save_calls >= 1


@pytest.mark.asyncio
async def test_reset_save_failure_restores_in_memory_counters():
    """F: save 失败时内存计数须回滚为旧值,而非停留在 0。

    Session 是缓存共享对象,若清零后 save 失败仍留 0,下一 turn 读到 0 永不重派
    review,计数在内存丢失却从未持久化。修法:save 失败回滚内存计数。"""
    from unittest.mock import AsyncMock

    session = Session(key="tg:rollback")
    session.metadata["_nudge_turns_memory"] = 5
    session.metadata["_nudge_tool_iters_memory"] = 3
    sessions = _SaveFailsSessions(session)

    rs = ResponseStage(
        config=None,
        sessions=sessions,
        memory=_FakeMemory(),
        provider=None,
        consolidation_worker=object(),
        default_model="",
        spawn_fn=lambda *a, **k: None,
        clear_memory_snapshot_fn=AsyncMock(),
        memory_service=object(),
    )

    await rs._reset_memory_nudge_counters("tg:rollback")

    assert sessions.save_calls == 1
    # 回滚:计数恢复旧值,下一 turn 仍会重新触发 review。
    assert session.metadata["_nudge_turns_memory"] == 5
    assert session.metadata["_nudge_tool_iters_memory"] == 3


# --- R4 Task6 Important 2: review 进行中禁止重复派发 --------------------------
@pytest.mark.asyncio
async def test_review_inflight_blocks_duplicate_dispatch():
    """清零挪到成功回调后,触发条件是 ">= 阈值" 且每轮持久化。达阈值后、在后台
    review 成功回调落 0 之前,若无 in-flight 去重,则每轮 turn 都重新判定 ≥ 阈值
    → 重复派发新 review。此处校验:review 进行中,后续 turn 不再重复派发;回调
    清掉 in-flight 标记后,下一轮才允许再次派发。"""
    spawned = []

    def _spawn_fn(item, **kwargs):
        spawned.append(item)

    rs = ResponseStage(
        config=None,
        sessions=_FakeSessions(),
        memory=_FakeMemory(),
        provider=None,
        consolidation_worker=object(),
        default_model="",
        spawn_fn=_spawn_fn,
        clear_memory_snapshot_fn=lambda *a, **k: None,
    )
    # spawn_fn 只记录不执行,review 永不落地 → 模拟 review 仍在进行中。
    rs._background_memory_review = (
        lambda self, *a, **k: None
    ).__get__(rs, ResponseStage)

    event = InboundEvent.text_message(
        channel="telegram", sender_id="u1", chat_id="123", text="hi",
    )
    session = Session(key=event.session_key)

    def _ctx():
        return PipelineContext(
            event=event, session=session, trace_id="t", publish_response=False,
            messages=[{"role": "user", "content": "hi"}],
        )

    result = InferenceResult(
        response_text="ok", total_tool_calls=0,
        should_review_skills=False, should_review_memory=True,
    )

    # Turn 1:达阈值,派发一次,标记 in-flight。
    await rs.finalize(_ctx(), result)
    assert len(spawned) == 1
    assert session.key in rs._memory_review_inflight

    # Turn 2:review 仍进行中(标记未清),同样 should_review_memory=True,
    # 但必须被去重,不得派发第二次。
    await rs.finalize(_ctx(), result)
    assert len(spawned) == 1

    # 成功回调清掉 in-flight 标记后,下一轮允许再次派发。
    rs._memory_review_inflight.discard(session.key)
    await rs.finalize(_ctx(), result)
    assert len(spawned) == 2
