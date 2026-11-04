"""Isolated memory acceptance through the real AgentLoop, store and context.

The scripted provider supplies controlled tool calls. Assertions inspect the
records and the actual messages passed to the model, never its final wording.
"""
from __future__ import annotations

import asyncio
import copy
import json
import os
from pathlib import Path
import subprocess
import sys

import pytest

from echo_agent.agent.loop import AgentLoop
from echo_agent.bus.events import InboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.config.schema import Config
from echo_agent.memory.service import ActorContext
from echo_agent.models.provider import LLMProvider, LLMResponse, ToolCallRequest


class AcceptanceProvider(LLMProvider):
    def __init__(self):
        super().__init__()
        self.calls = []
        self.next_tools = []

    def get_default_model(self):
        return "controlled-memory-model"

    async def chat(self, messages, **kwargs):
        self.calls.append(copy.deepcopy(messages))
        tools, self.next_tools = self.next_tools, []
        return LLMResponse(content="completed" if not tools else "", tool_calls=tools)

    async def chat_stream(self, messages, **kwargs):
        return await self.chat(messages, **kwargs)


def make_agent(workspace, *, enabled=True):
    config = Config(workspace=str(workspace))
    config.memory.enabled = enabled
    config.memory.vector_enabled = False
    config.memory.rerank_enabled = False
    config.memory.memory_nudge_interval = 0
    config.memory.consolidation_threshold = 10000
    config.memory.principal_bindings = ["telegram:alice", "slack:U0"]
    config.knowledge.enabled = False
    config.planning.enabled = False
    config.multi_agent.enabled = False
    config.evolution.enabled = False
    config.checkpoint.enabled = False
    config.observability.trace_enabled = False
    config.observability.otel_enabled = False
    provider = AcceptanceProvider()
    return AgentLoop(bus=MessageBus(), config=config, provider=provider, workspace=workspace), provider


async def turn(agent, provider, *, channel="slack", sender="U0", chat="new-conversation", text="请回顾饮品偏好", group=False):
    provider.calls.clear()
    event = InboundEvent.text_message(channel=channel, sender_id=sender, chat_id=chat, text=text, is_group=group)
    await agent._process_event(event, "memory-acceptance")
    # Only the system context is inspected: the probe never contains the value.
    context = "\n".join(str(m.get("content", "")) for m in provider.calls[0] if m["role"] == "system")
    return event, context


@pytest.mark.asyncio
async def test_real_save_share_correct_forget_and_process_restart(tmp_path):
    agent, provider = make_agent(tmp_path)
    provider.next_tools = [ToolCallRequest("save-pref", "memory", {
        "action": "add", "key": "drink-preference-314", "content": "饮品偏好是乌龙茶 M314",
        "source": "user_stated", "pinned": True,
    })]
    saved, _ = await turn(agent, provider, channel="telegram", sender="alice", chat="session-A", text="请记住我的饮品偏好")
    assert saved.memory_scope == "owner"
    entries = agent.memory.list_all(session_key="owner")
    assert len(entries) == 1 and entries[0].source == "user_stated"

    recalled, context = await turn(agent, provider)
    assert recalled.memory_scope == "owner" and recalled.session_key != saved.session_key
    assert "乌龙茶 M314" in context
    for sender, group in [("unbound", False), ("U0", True)]:
        _, isolated = await turn(agent, provider, sender=sender, group=group)
        assert "乌龙茶 M314" not in isolated
    agent.config.memory.cross_channel_owner = False
    _, isolated = await turn(agent, provider, chat="sharing-disabled")
    assert "乌龙茶 M314" not in isolated
    agent.config.memory.cross_channel_owner = True

    # A distinct interpreter loads this checkout and the same isolated data.
    # This verifies both disk persistence and injection, not a cached Store.
    module_root = str(Path(__file__).resolve().parents[1])
    probe = (
        "import asyncio,json,sys; from pathlib import Path; "
        "sys.path.insert(0, sys.argv[2] + '/tests'); "
        "from test_runtime_review_memory_acceptance import make_agent,turn; "
        "a,p=make_agent(Path(sys.argv[1])); "
        "e,c=asyncio.run(turn(a,p)); print(json.dumps({'scope':e.memory_scope,'context':c}))"
    )
    proc = await asyncio.to_thread(subprocess.run, [sys.executable, "-c", probe, str(tmp_path), module_root],
                                   cwd=module_root, env={**os.environ, "HF_HUB_OFFLINE": "1"},
                                   capture_output=True, text=True, timeout=30, check=True)
    restarted = json.loads(proc.stdout.strip().splitlines()[-1])
    assert restarted["scope"] == "owner" and "乌龙茶 M314" in restarted["context"]

    entry = entries[0]
    result = await agent._memory_service.replace(ActorContext(actor="admin", memory_scope="owner"),
                                                entry.id, content="饮品偏好是柠檬水 N628", source="admin", override=True)
    assert result.ok
    _, context = await turn(agent, provider, chat="after-correction")
    assert "柠檬水 N628" in context and "乌龙茶 M314" not in context
    await agent._memory_service.remove(ActorContext(actor="admin", memory_scope="owner"), entry.id, override=True)
    # Replaying the historical extraction (including after a data reopen) must
    # not make the forgotten preference active again.
    reopened, new_provider = make_agent(tmp_path)
    rejected = await reopened._memory_service.promote(ActorContext(actor="consolidation", memory_scope="owner"),
        type=entry.type, key=entry.key, content="饮品偏好是柠檬水 N628", importance=0.8)
    assert not rejected.ok
    reopened.memory.queue_missing_embeds()
    await reopened.memory.flush_pending_embeds()
    _, context = await turn(reopened, new_provider)
    assert "柠檬水 N628" not in context and "乌龙茶 M314" not in context
    assert reopened.memory.search_scored("饮品偏好", session_key="owner") == []

    disabled, disabled_provider = make_agent(tmp_path, enabled=False)
    _, context = await turn(disabled, disabled_provider)
    assert "M314" not in context and "N628" not in context
    assert disabled.tools.get("memory") is None


@pytest.mark.asyncio
async def test_reset_during_background_review_does_not_write_old_fact_or_clear_new_counters(tmp_path):
    agent, provider = make_agent(tmp_path)
    session = await agent.sessions.get_or_create("telegram:alice")
    started, release = asyncio.Event(), asyncio.Event()

    async def delayed_review(**kwargs):
        started.set()
        await release.wait()
        return LLMResponse(tool_calls=[ToolCallRequest("late", "memory_manage", {
            "action": "add", "target": "user", "key": "old-fact", "content": "obsolete marker Z987",
        })])

    provider.chat_with_retry = delayed_review
    task = asyncio.create_task(agent._response_stage._background_memory_review(
        [{"role": "user", "content": "old conversation"}], session.key, "owner", context_key=session.key,
    ))
    await started.wait()
    session.metadata["reset_count"] = 1
    session.messages = []
    session.metadata["_nudge_turns_memory"] = 7
    await agent.sessions.save(session)
    release.set()
    await asyncio.wait_for(task, 5)
    assert agent.memory.list_all(session_key="owner") == []
    assert session.metadata["_nudge_turns_memory"] == 7


@pytest.mark.asyncio
@pytest.mark.parametrize("phase", ["summary", "facts"])
async def test_reset_during_sleep_consolidation_does_not_promote_or_advance(tmp_path, phase):
    from echo_agent.agent.consolidation import ConsolidationWorker
    from echo_agent.memory.consolidator import MemoryConsolidator
    from echo_agent.memory.service import MemoryService
    from echo_agent.memory.store import MemoryStore
    from echo_agent.memory.tiers import EpisodicManager, SemanticManager
    from echo_agent.session.manager import SessionManager
    from echo_agent.storage.sqlite import SQLiteBackend

    storage = SQLiteBackend(tmp_path / "sleep.db")
    await storage.initialize()
    try:
        sessions = SessionManager(tmp_path / "sessions", storage=storage)
        session = await sessions.get_or_create("telegram:alice")
        session.add_message("user", "a durable preference from the old conversation")
        session.add_message("assistant", "acknowledged")
        await sessions.save(session)
        store = MemoryStore(tmp_path / "mem", scope_policy="session")
        started, release = asyncio.Event(), asyncio.Event()

        async def llm(**kwargs):
            call_phase = "facts" if kwargs.get("tools") else "summary"
            if call_phase == phase:
                started.set()
                await release.wait()
            if call_phase == "summary":
                return LLMResponse(content="old preference Z987")
            return LLMResponse(tool_calls=[ToolCallRequest("extract", "save_facts", {
                "facts": [{"key": "old-preference", "content": "old preference Z987"}],
            })])

        consolidator = MemoryConsolidator(store, llm)
        consolidator.set_episodic_manager(EpisodicManager(storage))
        consolidator.set_semantic_manager(SemanticManager(MemoryService(store)))
        worker = ConsolidationWorker(sessions, consolidator, sleep_consolidation=True)
        tasks = []
        await worker.schedule(session.key, lambda coro: tasks.append(asyncio.create_task(coro)), memory_scope="owner")
        await started.wait()
        session.messages = []
        session.metadata["reset_count"] = 1
        # Identical new messages must not fool a role/content-only comparison.
        session.add_message("user", "a durable preference from the old conversation")
        session.add_message("assistant", "acknowledged")
        await sessions.save(session)
        release.set()
        await asyncio.wait_for(asyncio.gather(*tasks), 5)
        assert store.list_all(session_key="owner") == []
        assert session.last_consolidated == 0
        assert not worker.is_pending(session.key)
    finally:
        await storage.close()


@pytest.mark.asyncio
async def test_forgotten_fact_is_not_reinjected_from_existing_episode_summary(tmp_path):
    from echo_agent.memory.tiers import EpisodicManager
    from echo_agent.memory.types import MemoryType
    from echo_agent.storage.sqlite import SQLiteBackend

    agent, provider = make_agent(tmp_path)
    storage = SQLiteBackend(tmp_path / "episodes.db")
    await storage.initialize()
    try:
        episodic = EpisodicManager(storage)
        agent._context_stage._episodic = episodic
        await episodic.create_episode("slack:new-conversation", [], "饮品偏好是旧值 F246")
        fact = await agent._memory_service.add(ActorContext(actor="model", memory_scope="owner"),
            type=MemoryType.USER, key="drink", content="饮品偏好是旧值 F246", source="user_stated")
        _, before = await turn(agent, provider)
        assert "旧值 F246" in before
        await agent._memory_service.remove(ActorContext(actor="admin", memory_scope="owner"), fact.entry.id, override=True)
        _, after = await turn(agent, provider)
        assert "旧值 F246" not in after
        # The transcript/audit remains inspectable; ordinary recall must stop
        # using this older summary, which can still contain the deleted fact.
        assert len(await episodic.get_session_episodes("slack:new-conversation", 10)) == 1
    finally:
        await storage.close()


@pytest.mark.asyncio
async def test_failed_background_review_keeps_pending_counters_for_retry(tmp_path):
    agent, provider = make_agent(tmp_path)
    session = await agent.sessions.get_or_create("telegram:alice")
    session.metadata["_nudge_turns_memory"] = 7

    async def unavailable(**kwargs):
        raise RuntimeError("controlled extraction outage")

    provider.chat_with_retry = unavailable
    with pytest.raises(RuntimeError):
        await agent._response_stage._background_memory_review([], session.key, "owner", context_key=session.key)
    assert session.metadata["_nudge_turns_memory"] == 7
    provider.chat_with_retry = provider.chat
    await agent._response_stage._background_memory_review([], session.key, "owner", context_key=session.key)
    assert session.metadata["_nudge_turns_memory"] == 0


@pytest.mark.asyncio
async def test_forget_while_retrieving_revalidates_snapshot_and_reranked_results(tmp_path):
    from echo_agent.memory.retrieval import HybridRetriever
    from echo_agent.memory.types import MemoryType

    agent, provider = make_agent(tmp_path)
    saved = await agent._memory_service.add(ActorContext(actor="model", memory_scope="owner"),
        type=MemoryType.USER, key="coffee", content="coffee preference W135", source="user_stated", pinned=True)

    async def forget(**kwargs):
        await agent._memory_service.remove(ActorContext(actor="admin", memory_scope="owner"), saved.entry.id, override=True)

    async def rerank(query, docs):
        await forget()
        return [1.0] * len(docs)

    retriever = HybridRetriever(entries_fn=agent.memory.list_all, visibility_fn=agent.memory.is_visible_in_session,
                                rerank_fn=rerank)
    assert await retriever.retrieve("coffee preference", memory_scope="owner") == []

    saved = await agent._memory_service.add(ActorContext(actor="model", memory_scope="owner"),
        type=MemoryType.USER, key="coffee", content="coffee preference W135", source="user_stated", pinned=True)

    async def bounded(*args):
        await forget()
        return []

    agent._context_stage._bounded_retrieve = bounded
    _, context = await turn(agent, provider, text="coffee preference")
    assert "W135" not in context
    assert all("W135" not in str(m.get("content", "")) for m in provider.calls[0])


@pytest.mark.asyncio
async def test_forget_during_episode_insert_and_retried_old_history_stays_inactive(tmp_path):
    from echo_agent.memory.consolidator import MemoryConsolidator
    from echo_agent.memory.service import MemoryService
    from echo_agent.memory.store import MemoryStore
    from echo_agent.memory.tiers import EpisodicManager, SemanticManager
    from echo_agent.memory.types import MemoryEntry
    from echo_agent.session.manager import Session
    from echo_agent.storage.sqlite import SQLiteBackend

    storage = SQLiteBackend(tmp_path / "episode-race.db")
    await storage.initialize()
    try:
        store = MemoryStore(tmp_path / "memory", scope_policy="session")
        service = MemoryService(store)
        fact = store.add(MemoryEntry(key="drink", content="old coffee", source_session="owner"))
        episodic = EpisodicManager(storage)
        create = episodic.create_episode

        async def insert_after_forget(*args, **kwargs):
            await service.remove(ActorContext(actor="admin", memory_scope="owner"), fact.id, override=True)
            return await create(*args, **kwargs)

        episodic.create_episode = insert_after_forget
        provider = AcceptanceProvider()
        consolidator = MemoryConsolidator(store, provider.chat)
        consolidator.set_episodic_manager(episodic)
        consolidator.set_semantic_manager(SemanticManager(service))
        session = Session(key="conversation")
        session.add_message("user", "old coffee")
        session.add_message("assistant", "acknowledged")
        stats = await consolidator.sleep_consolidate(session.key, session.messages, memory_scope="owner")
        assert stats.get("stale") == 1
        rows = await episodic.get_session_episodes(session.key, 10)
        assert rows and all(not store.is_episode_visible(ep, "owner") for ep in rows)
        stats = await consolidator.sleep_consolidate(session.key, session.messages, memory_scope="owner")
        assert stats["episodes"] == 0
        assert store.list_all(session_key="owner") == []

        # A mixed old/new batch must use its true retained span, rather than
        # aliasing the old (0, 2) idempotency key and reusing that old summary.
        episodic.create_episode = create
        session.add_message("user", "new post-forget input")
        session.add_message("assistant", "new response")
        await consolidator.sleep_consolidate(session.key, session.messages, memory_scope="owner")
        rows = await episodic.get_session_episodes(session.key, 10)
        assert any(ep.message_range_start == 2 and ep.message_range_end == 4
                   and store.is_episode_visible(ep, "owner") for ep in rows)
    finally:
        await storage.close()


@pytest.mark.asyncio
async def test_review_forget_revision_blocks_paraphrase_and_retry_of_old_history(tmp_path):
    from echo_agent.memory.types import MemoryType
    from echo_agent.session.manager import Session

    agent, provider = make_agent(tmp_path)
    session = await agent.sessions.get_or_create("telegram:alice")
    history = Session(key="old-review")
    history.add_message("user", "My preferred beverage is coffee")
    fact = await agent._memory_service.add(ActorContext(actor="model", memory_scope="owner"),
        type=MemoryType.USER, key="drink", content="coffee", source="user_stated")
    entered, release = asyncio.Event(), asyncio.Event()
    calls = []

    async def delayed(**kwargs):
        calls.append(kwargs)
        entered.set()
        await release.wait()
        return LLMResponse(tool_calls=[ToolCallRequest("paraphrase", "memory_manage", {
            "action": "add", "target": "user", "key": "favorite-beverage", "content": "The preferred beverage is coffee.",
        })])

    provider.chat_with_retry = delayed
    review = asyncio.create_task(agent._response_stage._background_memory_review(
        history.messages, session.key, "owner", context_key=session.key))
    await entered.wait()
    await agent._memory_service.remove(ActorContext(actor="admin", memory_scope="owner"), fact.entry.id, override=True)
    release.set()
    await asyncio.wait_for(review, 5)
    assert agent.memory.list_all(session_key="owner") == []
    count = len(calls)
    await agent._response_stage._background_memory_review(history.messages, session.key, "owner", context_key=session.key)
    assert len(calls) == count
    assert agent.memory.list_all(session_key="owner") == []


@pytest.mark.asyncio
async def test_planning_await_cannot_reinject_a_forgotten_snapshot(tmp_path):
    from types import SimpleNamespace
    from echo_agent.memory.types import MemoryType

    agent, provider = make_agent(tmp_path)
    fact = await agent._memory_service.add(ActorContext(actor="model", memory_scope="owner"),
        type=MemoryType.USER, key="coffee", content="coffee marker PLAN-739", source="user_stated", pinned=True)

    async def plan(**kwargs):
        await agent._memory_service.remove(ActorContext(actor="admin", memory_scope="owner"), fact.entry.id, override=True)
        return None

    agent._context_stage._planner = SimpleNamespace(create_plan=plan)
    await turn(agent, provider)
    assert all("PLAN-739" not in str(m.get("content", "")) for m in provider.calls[0])


@pytest.mark.asyncio
async def test_lineage_cleanup_preserves_learning_and_unrelated_episodes(tmp_path):
    from echo_agent.memory.tiers import ArchivalManager
    from echo_agent.memory.types import Episode, MemoryType

    agent, _ = make_agent(tmp_path)
    actor = ActorContext(actor="consolidation", memory_scope="owner")
    for version in range(6):
        result = await agent._memory_service.add(actor, type=MemoryType.USER,
            key="editor", content=f"editor preference revision {version}", source="consolidated")
        assert result.ok
    episode = Episode(summary="An unrelated durable project decision", session_key="conversation")
    _, old_versions = await agent.memory._forgetting.run_decay_pass(agent.memory.list_all(session_key="owner"))
    assert old_versions and all(entry.is_superseded for entry in old_versions)
    removed = await ArchivalManager(None, agent._memory_service).delete_forgotten(old_versions)
    assert removed == len(old_versions)
    assert agent.memory.forget_revision("owner") == 0
    assert agent.memory.is_episode_visible(episode, "owner")
    updated = await agent._memory_service.add(actor, type=MemoryType.USER,
        key="editor", content="editor preference revision 6", source="consolidated")
    assert updated.ok
    assert agent.memory.find_by_key("editor", session_key="owner").content.endswith("6")


@pytest.mark.asyncio
@pytest.mark.parametrize("actor", ["model", "reviewer", "maintenance"])
async def test_automatic_removal_does_not_act_as_user_forget(tmp_path, actor):
    from echo_agent.memory.types import MemoryType

    agent, _ = make_agent(tmp_path)
    ctx = ActorContext(actor=actor, memory_scope="owner")
    result = await agent._memory_service.add(ctx, type=MemoryType.USER, key="note",
        content="an automatically inferred note", source="model_inferred")
    assert (await agent._memory_service.remove(ctx, result.entry.id)).ok
    assert agent.memory.forget_revision("owner") == 0
    assert (await agent._memory_service.add(ctx, type=MemoryType.USER, key="note",
        content="a new observation", source="model_inferred")).ok


@pytest.mark.asyncio
async def test_admin_removing_a_superseded_version_does_not_forget_the_current_fact(tmp_path):
    from echo_agent.memory.types import MemoryType

    agent, _ = make_agent(tmp_path)
    actor = ActorContext(actor="model", memory_scope="owner")
    old = await agent._memory_service.add(actor, type=MemoryType.USER, key="editor",
        content="old editor preference", source="model_inferred")
    current = await agent._memory_service.add(actor, type=MemoryType.USER, key="editor",
        content="current editor preference", source="model_inferred")
    assert agent.memory.find_by_key("editor", session_key="owner").id == current.entry.id
    assert (await agent._memory_service.remove(ActorContext(actor="admin", memory_scope="owner"),
        old.entry.id, override=True)).ok
    assert agent.memory.forget_revision("owner") == 0
    assert (await agent._memory_service.add(actor, type=MemoryType.USER, key="editor",
        content="new editor preference", source="model_inferred")).ok


@pytest.mark.asyncio
@pytest.mark.parametrize("kind", ["environment", "global_user"])
@pytest.mark.parametrize("operation", ["delete", "replace"])
async def test_global_change_during_planning_rebuilds_the_actual_model_prompt(tmp_path, kind, operation):
    from types import SimpleNamespace
    from echo_agent.memory.types import MemoryType

    agent, provider = make_agent(tmp_path)
    actor = ActorContext(actor="admin", memory_scope="owner")
    fact = await agent._memory_service.add(actor,
        type=MemoryType.ENVIRONMENT if kind == "environment" else MemoryType.USER,
        key="endpoint", content="OBSOLETE-GLOBAL-849", source="user_stated",
        tags=["global"] if kind == "global_user" else [], pinned=True)
    before = agent._scope_version("previously-uncached-scope")

    async def plan(**kwargs):
        if operation == "delete":
            result = await agent._memory_service.remove(actor, fact.entry.id, override=True)
        else:
            result = await agent._memory_service.replace(actor, fact.entry.id,
                content="CORRECT-GLOBAL-527", source="admin", override=True)
        assert result.ok
        return None

    agent._context_stage._planner = SimpleNamespace(create_plan=plan)
    await turn(agent, provider, text="Which endpoint should I use?")
    assert agent._scope_version("previously-uncached-scope") > before
    prompt = json.dumps(provider.calls[0])
    assert "OBSOLETE-GLOBAL-849" not in prompt
    if operation == "replace":
        assert "CORRECT-GLOBAL-527" in prompt


@pytest.mark.asyncio
async def test_changing_global_tag_invalidates_snapshots_for_other_scopes(tmp_path):
    from echo_agent.memory.types import MemoryType

    agent, _ = make_agent(tmp_path)
    actor = ActorContext(actor="admin", memory_scope="owner")
    result = await agent._memory_service.add(actor, type=MemoryType.USER, key="project",
        content="shared project convention", source="user_stated")
    for tags in (["global"], []):
        before = agent._scope_version("another-user")
        assert (await agent._memory_service.maintenance_update(actor, result.entry.id, tags=tags)).ok
        assert agent._scope_version("another-user") > before


@pytest.mark.asyncio
@pytest.mark.parametrize("reason", ["error", "length", "content_filter"])
async def test_provider_failure_response_keeps_review_pending(tmp_path, reason):
    from unittest.mock import AsyncMock

    agent, provider = make_agent(tmp_path)
    session = await agent.sessions.get_or_create("telegram:alice")
    session.metadata["_nudge_turns_memory"] = 7
    session.add_message("user", "A new preference worth remembering")
    provider.chat_with_retry = AsyncMock(return_value=LLMResponse(content="incomplete", finish_reason=reason))
    with pytest.raises(RuntimeError, match="Memory extraction failed"):
        await agent._response_stage._background_memory_review(
            copy.deepcopy(session.messages), session.key, "owner", context_key=session.key)
    assert session.metadata["_nudge_turns_memory"] == 7
    assert "_memory_review_cursor" not in session.metadata
    provider.chat_with_retry.return_value = LLMResponse(content="No memory changes needed.")
    await agent._response_stage._background_memory_review(
        copy.deepcopy(session.messages), session.key, "owner", context_key=session.key)
    assert session.metadata["_nudge_turns_memory"] == 0
    assert session.metadata["_memory_review_cursor"]["timestamp"] == session.messages[-1]["timestamp"]


@pytest.mark.asyncio
async def test_review_does_not_acknowledge_turns_that_arrived_after_its_snapshot(tmp_path):
    agent, provider = make_agent(tmp_path)
    session = await agent.sessions.get_or_create("telegram:alice")
    session.add_message("user", "Earlier preference")
    session.add_message("assistant", "Earlier answer")
    snapshot = copy.deepcopy(session.messages)
    entered, release = asyncio.Event(), asyncio.Event()

    async def delayed(**kwargs):
        entered.set()
        await release.wait()
        return LLMResponse(content="No memory changes needed.")

    provider.chat_with_retry = delayed
    session.metadata.update(_nudge_turns_memory=7, _nudge_tool_iters_memory=3)
    review = asyncio.create_task(agent._response_stage._background_memory_review(
        snapshot, session.key, "owner", context_key=session.key))
    await entered.wait()
    session.add_message("user", "New preference 1")
    session.add_message("assistant", "", tool_calls=[{"id": "new-call"}])
    session.add_message("tool", "result", tool_call_id="new-call")
    session.add_message("user", "New preference 2")
    session.metadata.update(_nudge_turns_memory=9, _nudge_tool_iters_memory=4)
    release.set()
    await asyncio.wait_for(review, 5)
    assert session.metadata["_nudge_turns_memory"] == 2
    assert session.metadata["_nudge_tool_iters_memory"] == 1
    assert session.metadata["_memory_review_cursor"]["timestamp"] == snapshot[-1]["timestamp"]


@pytest.mark.asyncio
async def test_review_snapshot_excludes_consolidated_and_previously_reviewed_history(tmp_path):
    from echo_agent.agent.pipeline.types import InferenceResult, PipelineContext
    from unittest.mock import AsyncMock

    agent, provider = make_agent(tmp_path)
    event = InboundEvent.text_message(channel="telegram", sender_id="alice", chat_id="alice", text="new")
    event.memory_scope = "owner"
    session = await agent.sessions.get_or_create(event.session_key)
    for _ in range(2000):
        session.add_message("user", "ALREADY-CONSOLIDATED-352 " + "x" * 1000)
    session.last_consolidated = len(session.messages)
    session.add_message("user", "NEW-PREFERENCE-647")
    spawned = []
    agent._response_stage._spawn_fn = lambda work, **kwargs: spawned.append(work)
    provider.chat_with_retry = AsyncMock(return_value=LLMResponse(content="No memory changes needed."))
    context = PipelineContext(event=event, session=session, trace_id="bounded-review",
        publish_response=False, context_key=session.key)
    await agent._response_stage.finalize(context, InferenceResult(response_text="FIRST-ANSWER-963", should_review_memory=True))
    assert len(spawned) == 1
    await spawned.pop()()
    first = json.dumps(provider.chat_with_retry.call_args.kwargs["messages"])
    assert "ALREADY-CONSOLIDATED-352" not in first
    assert "NEW-PREFERENCE-647" in first and "FIRST-ANSWER-963" in first
    session.add_message("user", "SECOND-PREFERENCE-285")
    await agent._response_stage.finalize(context, InferenceResult(response_text="second answer", should_review_memory=True))
    await spawned.pop()()
    second = json.dumps(provider.chat_with_retry.call_args.kwargs["messages"])
    assert "NEW-PREFERENCE-647" not in second and "FIRST-ANSWER-963" not in second
    assert "SECOND-PREFERENCE-285" in second


@pytest.mark.asyncio
@pytest.mark.parametrize("unit", ["🧪中文", "\\\x00\"\n"])
async def test_review_batches_preserve_long_unicode_tail_and_quote_orphaned_tool_results(tmp_path, unit):
    from unittest.mock import AsyncMock
    from echo_agent.memory.reviewer import MemoryReviewer

    agent, provider = make_agent(tmp_path)
    provider.chat_with_retry = AsyncMock(return_value=LLMResponse(content="No memory changes needed."))
    reviewer = MemoryReviewer(provider, agent._memory_service, model=provider.get_default_model(), session_key="owner")
    long_text = unit * 10_000 + "IMPORTANT-TAIL-681"
    await reviewer.review([
        {"role": "user", "content": long_text},
        {"role": "tool", "name": "read_file", "content": "post-forget result", "tool_call_id": "pre-forget-call"},
    ])
    assert not reviewer.failed
    calls = provider.chat_with_retry.call_args_list
    assert len(calls) > 1
    transcript = "".join(call.kwargs["messages"][0]["content"] for call in calls)
    assert transcript.count("🧪" if unit.startswith("🧪") else "\x00") == 10_000
    assert "IMPORTANT-TAIL-681" in transcript and "post-forget result" in transcript
    for call in calls:
        assert all(message["role"] != "tool" for message in call.kwargs["messages"])
        request = json.dumps([call.kwargs["messages"], call.kwargs["tools"]], ensure_ascii=False).encode()
        assert len(request) <= reviewer._request_budget


@pytest.mark.asyncio
async def test_global_forget_cannot_be_recreated_under_a_local_scope(tmp_path):
    from echo_agent.memory.types import MemoryType

    agent, _ = make_agent(tmp_path)
    admin = ActorContext(actor="admin", memory_scope="owner")
    fact = await agent._memory_service.add(admin, type=MemoryType.USER, key="shared-preference",
        content="a shared fact", source="user_stated", tags=["global"])
    assert (await agent._memory_service.remove(admin, fact.entry.id, override=True)).ok
    local = ActorContext(actor="reviewer", memory_scope="another-user")
    result = await agent._memory_service.add(local, type=MemoryType.USER, key="shared-preference",
        content="a shared fact", source="model_inferred")
    assert not result.ok and result.reason == "rejected_forgotten"
    assert (await agent._memory_service.add(ActorContext(actor="model", memory_scope="another-user"),
        type=MemoryType.USER, key="shared-preference", content="a new explicit preference", source="user_stated")).ok


@pytest.mark.asyncio
async def test_review_progress_is_not_a_wall_clock_watermark(tmp_path):
    from echo_agent.agent.pipeline.types import InferenceResult, PipelineContext
    from unittest.mock import AsyncMock

    agent, provider = make_agent(tmp_path)
    event = InboundEvent.text_message(channel="telegram", sender_id="alice", chat_id="alice", text="new")
    session = await agent.sessions.get_or_create(event.session_key)
    event.memory_scope = "owner"
    session.add_message("user", "REVIEWED-OLD-492", timestamp="2030-01-01T00:00:00")
    spawned = []
    agent._response_stage._spawn_fn = lambda work, **kwargs: spawned.append(work)
    provider.chat_with_retry = AsyncMock(return_value=LLMResponse(content="No memory changes needed."))
    context = PipelineContext(event=event, session=session, trace_id="clock-review", publish_response=False, context_key=session.key)
    await agent._response_stage.finalize(context, InferenceResult(response_text="reviewed answer", should_review_memory=True))
    await spawned.pop()()
    session.add_message("user", "NEW-AFTER-CLOCK-CHANGE-268", timestamp="2020-01-01T00:00:00")
    await agent._response_stage.finalize(context, InferenceResult(response_text="new answer", should_review_memory=True))
    await spawned.pop()()
    latest = json.dumps(provider.chat_with_retry.call_args.kwargs["messages"])
    assert "NEW-AFTER-CLOCK-CHANGE-268" in latest
    assert "REVIEWED-OLD-492" not in latest


@pytest.mark.asyncio
async def test_review_respects_configured_private_model_window_and_keeps_partial_failure_pending(tmp_path):
    from unittest.mock import AsyncMock

    agent, provider = make_agent(tmp_path)
    agent._response_stage._default_model = "private-small-model"
    agent.config.models.model_windows["private-small-model"] = 8192
    session = await agent.sessions.get_or_create("telegram:alice")
    session.add_message("user", "小窗口模型的长期偏好" * 2000 + "SMALL-WINDOW-TAIL-927")
    session.metadata["_nudge_turns_memory"] = 7
    calls = []
    fail_second = True

    async def strict_model(**kwargs):
        calls.append(copy.deepcopy(kwargs))
        wire_size = len(json.dumps([kwargs["messages"], kwargs["tools"]], ensure_ascii=False).encode())
        assert wire_size + kwargs["max_tokens"] + 512 <= 8192
        if fail_second and len(calls) == 2:
            return LLMResponse(content="Error: temporary outage", finish_reason="error")
        return LLMResponse(content="No memory changes needed.")

    provider.chat_with_retry = strict_model
    snapshot = copy.deepcopy(session.messages)
    with pytest.raises(RuntimeError, match="Memory extraction failed"):
        await agent._response_stage._background_memory_review(snapshot, session.key, "owner", context_key=session.key)
    assert session.metadata["_nudge_turns_memory"] == 7
    assert "_memory_review_cursor" not in session.metadata
    fail_second = False
    calls.clear()
    await agent._response_stage._background_memory_review(snapshot, session.key, "owner", context_key=session.key)
    assert session.metadata["_nudge_turns_memory"] == 0
    assert "SMALL-WINDOW-TAIL-927" in json.dumps(calls)

    old_cursor = copy.deepcopy(session.metadata["_memory_review_cursor"])
    session.add_message("user", "new preference after review")
    session.metadata["_nudge_turns_memory"] = 2
    agent.sessions.save = AsyncMock(side_effect=OSError("disk full"))
    await agent._response_stage._reset_memory_nudge_counters(
        session.key, context_key=session.key, reviewed_message=session.messages[-1])
    assert session.metadata["_memory_review_cursor"] == old_cursor
    assert session.metadata["_nudge_turns_memory"] == 2


@pytest.mark.asyncio
async def test_forget_during_last_consolidation_guard_cannot_promote_a_paraphrase(tmp_path):
    from echo_agent.memory.consolidator import MemoryConsolidator
    from echo_agent.memory.tiers import EpisodicManager, SemanticManager
    from echo_agent.memory.types import MemoryType
    from echo_agent.session.manager import Session
    from echo_agent.storage.sqlite import SQLiteBackend

    agent, _ = make_agent(tmp_path)
    admin = ActorContext(actor="admin", memory_scope="owner")
    original = await agent._memory_service.add(admin, type=MemoryType.USER,
        key="drink", content="coffee", source="user_stated")
    storage = SQLiteBackend(tmp_path / "guard.db")
    await storage.initialize()
    try:
        async def llm(**kwargs):
            if kwargs.get("tools"):
                return LLMResponse(tool_calls=[ToolCallRequest("facts", "save_facts", {"facts": [
                    {"key": "beverage", "content": "The favorite drink is coffee, paraphrased."},
                ]})])
            return LLMResponse(content="The user likes coffee.")

        checks = 0

        async def guard():
            nonlocal checks
            checks += 1
            if checks == 3:
                await agent._memory_service.remove(admin, original.entry.id, override=True)
            return True

        consolidator = MemoryConsolidator(agent.memory, llm)
        consolidator.set_episodic_manager(EpisodicManager(storage))
        consolidator.set_semantic_manager(SemanticManager(agent._memory_service))
        session = Session(key="conversation")
        session.add_message("user", "I like coffee")
        stats = await consolidator.sleep_consolidate(session.key, session.messages,
            memory_scope="owner", can_commit=guard)
        assert checks >= 3
        assert stats.get("stale") == 1
        assert agent.memory.list_all(session_key="owner") == []
    finally:
        await storage.close()
