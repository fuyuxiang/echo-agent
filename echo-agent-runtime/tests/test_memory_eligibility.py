# tests/test_memory_eligibility.py
from echo_agent.memory.eligibility import Audience, LifecycleStatus, lifecycle_status, is_eligible
from echo_agent.memory.types import MemoryEntry, MemoryTier, MemoryType

def _entry(**kw):
    base = dict(type=MemoryType.USER, key="k", content="c")
    base.update(kw)
    return MemoryEntry(**base)

def test_status_priority_superseded_beats_archived():
    e = _entry(superseded_by="x", tier=MemoryTier.ARCHIVAL)
    assert lifecycle_status(e, lambda _id: True) == LifecycleStatus.SUPERSEDED

def test_active_visible_to_retrieval_hidden_none():
    e = _entry()
    assert is_eligible(e, Audience.RETRIEVAL, is_unresolved_fn=lambda _id: False)

def test_archived_hidden_from_retrieval_visible_to_admin():
    e = _entry(tier=MemoryTier.ARCHIVAL)
    assert not is_eligible(e, Audience.RETRIEVAL, is_unresolved_fn=lambda _id: False)
    assert is_eligible(e, Audience.ADMIN, is_unresolved_fn=lambda _id: False)

def test_unresolved_hidden_from_retrieval_visible_to_maintenance():
    e = _entry()
    assert not is_eligible(e, Audience.RETRIEVAL, is_unresolved_fn=lambda _id: True)
    assert is_eligible(e, Audience.MAINTENANCE, is_unresolved_fn=lambda _id: True)

def test_episode_proxy_no_tier_defaults_active():
    class _Proxy:  # 模拟 _EpisodicProxy：无 tier / 无 superseded_by
        id = "ep:1"
        is_superseded = False
    assert is_eligible(_Proxy(), Audience.RETRIEVAL, is_unresolved_fn=lambda _id: False)
