from echo_agent.memory.types import MemoryEntry, MemoryType, provenance_guard


def _e(source):
    return MemoryEntry(type=MemoryType.USER, key="k", content="c", source=source)


def test_lower_priority_denied():
    assert provenance_guard("model_inferred", _e("user_stated")) is False


def test_equal_priority_allowed():
    assert provenance_guard("user_stated", _e("user_stated")) is True


def test_higher_priority_allowed():
    assert provenance_guard("user_stated", _e("model_inferred")) is True


def test_unknown_actor_rank_zero_denied_against_known():
    assert provenance_guard("legacy", _e("model_inferred")) is False
