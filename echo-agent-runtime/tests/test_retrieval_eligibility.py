# tests/test_retrieval_eligibility.py
import pytest
from unittest.mock import AsyncMock, MagicMock

from echo_agent.memory.eligibility import Audience, is_eligible, is_transient_task_state
from echo_agent.memory.retrieval import HybridRetriever
from echo_agent.memory.types import MemoryEntry, MemoryTier, MemoryType


def _entry(eid, content, **kw):
    e = MemoryEntry(type=MemoryType.USER, key=eid, content=content, **kw)
    e.id = eid
    return e


@pytest.mark.asyncio
async def test_superseded_not_in_candidate_pool(monkeypatch):
    live = _entry("live", "上海住址", source="user_stated")
    old = _entry("old", "北京住址", source="user_stated", superseded_by="live")
    vec = MagicMock()
    vec.search = AsyncMock(return_value=[])
    r = HybridRetriever(
        entries_fn=lambda: [live, old],
        vector_index=vec,
        embed_fn=AsyncMock(return_value=[0.1]),
        visibility_fn=lambda e, s: True,
        is_unresolved_fn=lambda _id: False,
    )
    hits = await r.retrieve("北京", memory_scope="s1", limit=5)
    assert all(h.id != "old" for h in hits)


@pytest.mark.asyncio
async def test_archived_excluded(monkeypatch):
    arch = _entry("a", "旧档案", source="user_stated", tier=MemoryTier.ARCHIVAL)
    vec = MagicMock()
    vec.search = AsyncMock(return_value=[])
    r = HybridRetriever(
        entries_fn=lambda: [arch], vector_index=vec,
        embed_fn=AsyncMock(return_value=[0.1]),
        visibility_fn=lambda e, s: True, is_unresolved_fn=lambda _id: False,
    )
    hits = await r.retrieve("档案", memory_scope="s1", limit=5)
    assert hits == []


def test_inferred_task_status_is_hidden_from_agent_but_admin_can_audit():
    entry = _entry(
        "ai-agent-whitepaper-status",
        "The white paper has been completed with 8 phase documents.",
        source="consolidated",
    )

    assert is_transient_task_state(entry) is True
    for audience in (Audience.SNAPSHOT, Audience.RETRIEVAL, Audience.TOOL):
        assert not is_eligible(entry, audience, is_unresolved_fn=lambda _id: False)
    assert is_eligible(entry, Audience.ADMIN, is_unresolved_fn=lambda _id: False)
    assert is_eligible(entry, Audience.MAINTENANCE, is_unresolved_fn=lambda _id: False)


def test_explicit_user_status_and_durable_project_fact_remain_eligible():
    explicit = _entry(
        "current_task",
        "The current task is release 2.0.",
        source="user_stated",
    )
    project = _entry(
        "active_project",
        "User works on echo-agent at /srv/echo-agent.",
        source="consolidated",
    )

    assert not is_transient_task_state(explicit)
    assert not is_transient_task_state(project)
    assert is_eligible(explicit, Audience.RETRIEVAL, is_unresolved_fn=lambda _id: False)
    assert is_eligible(project, Audience.RETRIEVAL, is_unresolved_fn=lambda _id: False)


@pytest.mark.asyncio
async def test_inferred_task_status_never_enters_retrieval_candidate_pool():
    transient = _entry(
        "release_status",
        "Implementation is in progress; next steps are tests.",
        source="model_inferred",
    )
    durable = _entry(
        "repository_path",
        "Repository path is /srv/echo-agent.",
        source="model_inferred",
    )
    vec = MagicMock()
    vec.search = AsyncMock(return_value=[])
    retriever = HybridRetriever(
        entries_fn=lambda: [transient, durable],
        vector_index=vec,
        embed_fn=AsyncMock(return_value=[0.1]),
        visibility_fn=lambda e, s: True,
        is_unresolved_fn=lambda _id: False,
    )

    hits = await retriever.retrieve("release repository path", memory_scope="s1", limit=5)
    assert all(entry.id != transient.id for entry, _score in hits)
    assert any(entry.id == durable.id for entry, _score in hits)
