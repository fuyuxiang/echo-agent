import pytest
from echo_agent.memory.tiers import SemanticManager
from echo_agent.memory.types import MemoryEntry


class _FakeStore:
    def __init__(self):
        self.added = []

    def add(self, entry: MemoryEntry) -> MemoryEntry:
        self.added.append(entry)
        return entry


def _make_episode(session_key: str):
    from echo_agent.memory.tiers import Episode
    return Episode(id="ep1", session_key=session_key, summary="s")


@pytest.mark.asyncio
async def test_promote_user_fact_carries_source_session():
    store = _FakeStore()
    mgr = SemanticManager(store=store)
    episode = _make_episode("telegram:room1")
    facts = [{"type": "user", "key": "pref", "content": "likes dark mode"}]
    promoted = await mgr.promote_from_episodic(episode, facts)
    assert promoted[0].source_session == "telegram:room1"


@pytest.mark.asyncio
async def test_promote_environment_fact_no_source_session():
    store = _FakeStore()
    mgr = SemanticManager(store=store)
    episode = _make_episode("telegram:room1")
    facts = [{"type": "environment", "key": "tz", "content": "UTC+8"}]
    promoted = await mgr.promote_from_episodic(episode, facts)
    assert promoted[0].source_session == ""
