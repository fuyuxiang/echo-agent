from pathlib import Path

from echo_agent.memory.store import MemoryStore


def test_thresholds_flow_into_forgetting_curve(tmp_path: Path):
    store = MemoryStore(
        memory_dir=tmp_path / "mem",
        archival_threshold=0.2,
        forget_threshold=0.1,
    )
    assert store._forgetting._archive_threshold == 0.2
    assert store._forgetting._forget_threshold == 0.1
