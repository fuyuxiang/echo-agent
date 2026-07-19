from echo_agent.memory.store import MemoryStore


def test_snapshot_injects_episode_summaries_as_narrative(tmp_path):
    s = MemoryStore(memory_dir=tmp_path / "mem", scope_policy="session")
    snap, _ = s.get_snapshot_with_ids(
        session_key="x",
        episode_summaries=["用户先在北京、因工作搬到上海", "讨论了部署方案"],
    )
    assert "## Recent Context" in snap
    assert "因工作搬到上海" in snap and "部署方案" in snap


def test_snapshot_no_narrative_when_empty(tmp_path):
    s = MemoryStore(memory_dir=tmp_path / "mem", scope_policy="session")
    snap, _ = s.get_snapshot_with_ids(session_key="x", episode_summaries=None)
    assert "## Recent Context" not in snap  # 无 summary 不注入空段
