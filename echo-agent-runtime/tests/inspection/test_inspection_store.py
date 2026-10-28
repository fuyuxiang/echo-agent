from pathlib import Path

from echo_agent.agent.inspection.store import (
    InspectItem,
    InspectStore,
    parse_interval,
)


def test_parse_interval_units():
    assert parse_interval("30s") == 30
    assert parse_interval("10m") == 600
    assert parse_interval("2h") == 7200
    assert parse_interval("1d") == 86400
    assert parse_interval("45") == 45  # bare = seconds
    assert parse_interval("bad") == 0


def test_load_items_parses_sections(tmp_path: Path):
    md = tmp_path / "INSPECT.md"
    md.write_text(
        "# 巡检清单\n\n"
        "## 官网可用性\n- interval: 10m\n- check: 访问 https://x.com 非200报警\n\n"
        "## 竞品价格\n- interval: 1d\n- check: 抓价格有变化就报\n",
        encoding="utf-8",
    )
    store = InspectStore(md, tmp_path / "state.json")
    items = store.load_items()
    assert len(items) == 2
    assert items[0].name == "官网可用性"
    assert items[0].interval_sec == 600
    assert "非200" in items[0].check
    assert items[1].interval_sec == 86400


def test_load_items_missing_file_returns_empty(tmp_path: Path):
    store = InspectStore(tmp_path / "nope.md", tmp_path / "state.json")
    assert store.load_items() == []


def test_state_roundtrip(tmp_path: Path):
    store = InspectStore(tmp_path / "INSPECT.md", tmp_path / "state.json")
    store.save_state({"官网可用性": {"last_checked_at": 100, "last_conclusion": "OK"}})
    state = store.load_state()
    assert state["官网可用性"]["last_conclusion"] == "OK"


def test_load_state_corrupt_returns_empty(tmp_path: Path):
    sp = tmp_path / "state.json"
    sp.write_text("{not json", encoding="utf-8")
    store = InspectStore(tmp_path / "INSPECT.md", sp)
    assert store.load_state() == {}


def test_due_items_selects_expired(tmp_path: Path):
    store = InspectStore(tmp_path / "INSPECT.md", tmp_path / "state.json")
    items = [
        InspectItem(name="a", interval_sec=600, check="x"),
        InspectItem(name="b", interval_sec=600, check="y"),
    ]
    state = {
        "a": {"last_checked_at": 0, "last_conclusion": "OK"},      # old → due
        "b": {"last_checked_at": 1000, "last_conclusion": "OK"},   # recent → not due
    }
    due = store.due_items(items, state, now_sec=1200, max_items=5)
    assert [i.name for i in due] == ["a"]  # only a expired (1200-0>=600, 1200-1000<600)


def test_due_items_no_state_is_due(tmp_path: Path):
    store = InspectStore(tmp_path / "INSPECT.md", tmp_path / "state.json")
    items = [InspectItem(name="a", interval_sec=600, check="x")]
    due = store.due_items(items, {}, now_sec=100, max_items=5)
    assert [i.name for i in due] == ["a"]


def test_due_items_truncates_to_max(tmp_path: Path):
    store = InspectStore(tmp_path / "INSPECT.md", tmp_path / "state.json")
    items = [InspectItem(name=f"i{n}", interval_sec=1, check="x") for n in range(10)]
    due = store.due_items(items, {}, now_sec=100, max_items=3)
    assert len(due) == 3
