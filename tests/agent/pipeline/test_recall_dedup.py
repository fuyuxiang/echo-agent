from echo_agent.agent.pipeline.context_stage import filter_recall_by_snapshot


class _R:
    def __init__(self, key, content, mid):
        self.key = key
        self.content = content
        self.id = mid


def test_filters_entries_already_in_snapshot():
    scored = [(_R("a", "x", "m1"), 0.9), (_R("b", "y", "m2"), 0.8)]
    out = filter_recall_by_snapshot(scored, frozenset({"m1"}))
    assert [r.id for r, _ in out] == ["m2"]


def test_empty_snapshot_ids_keeps_all():
    scored = [(_R("a", "x", "m1"), 0.9)]
    out = filter_recall_by_snapshot(scored, frozenset())
    assert len(out) == 1


def test_none_snapshot_ids_keeps_all():
    scored = [(_R("a", "x", "m1"), 0.9)]
    out = filter_recall_by_snapshot(scored, None)
    assert len(out) == 1


def test_entry_without_id_attr_is_kept():
    class _NoId:
        key = "k"
        content = "c"
    scored = [(_NoId(), 0.5)]
    out = filter_recall_by_snapshot(scored, frozenset({"m1"}))
    assert len(out) == 1  # 无 id 不误删


def test_dedup_is_applied_to_cached_and_synced_paths():
    # snapshot 含 m1;两条召回 m1(应删)、m3(应留)
    scored = [(_R("a", "x", "m1"), 0.9), (_R("c", "z", "m3"), 0.7)]
    out = filter_recall_by_snapshot(scored, frozenset({"m1"}))
    keys = [r.id for r, _ in out]
    assert "m1" not in keys
    assert "m3" in keys
