import time
import pytest
from echo_agent.agent.progress_heartbeat import SharedActivityState, ActivitySnapshot


def test_activity_starts_thinking():
    st = SharedActivityState(started_at=100.0)
    snap = st.snapshot(now=105.0)
    assert snap.phase == "thinking"
    assert snap.current_tool is None
    assert snap.elapsed_sec == pytest.approx(5.0)


def test_enter_and_exit_tool_tracks_phase():
    st = SharedActivityState(started_at=0.0)
    st.enter_tool("web_search")
    snap = st.snapshot(now=1.0)
    assert snap.phase == "calling_tool"
    assert snap.current_tool == "web_search"
    st.exit_tool()
    snap2 = st.snapshot(now=2.0)
    assert snap2.phase == "thinking"
    assert snap2.current_tool is None


def test_set_generating_phase():
    st = SharedActivityState(started_at=0.0)
    st.set_generating()
    assert st.snapshot(now=1.0).phase == "generating"


def test_mark_visible_feedback_resets_since():
    st = SharedActivityState(started_at=0.0)
    base = time.monotonic()
    st.mark_visible_feedback(now=base)
    assert st.since_last_feedback(now=base + 7.0) == pytest.approx(7.0)
