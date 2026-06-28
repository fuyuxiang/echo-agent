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
