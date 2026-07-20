from echo_agent.cli.tui.status_bar import StatusBar


def test_initial_state_is_disconnected():
    bar = StatusBar()
    text = bar._compose_text()
    assert "○已断开" in text
    assert "●已连接" not in text


def test_connected_with_session():
    bar = StatusBar()
    bar.set_session("cli:local")
    bar.set_connection(True)
    text = bar._compose_text()
    assert "●已连接" in text
    assert "cli:local" in text


def test_set_model_appears_in_rendered_text():
    bar = StatusBar()
    bar.set_model("opus")
    text = bar._compose_text()
    assert "opus" in text


def test_cost_update():
    bar = StatusBar()
    bar.set_cost(0.042)
    text = bar._compose_text()
    assert "$0.0420" in text


def test_context_gauge():
    bar = StatusBar()
    bar.set_context(18200, 65536)
    text = bar._compose_text()
    assert "18.2K" in text
    assert "65.5K" in text
    assert "%" in text


def test_memory_count():
    bar = StatusBar()
    bar.set_memory_count(47)
    text = bar._compose_text()
    assert "47" in text


def test_turn_timer():
    import time
    bar = StatusBar()
    bar.start_turn_timer()
    time.sleep(0.05)
    text = bar._compose_text()
    assert "⏱" in text
    bar.stop_turn_timer()
    text2 = bar._compose_text()
    assert "⏱" in text2


def test_timer_runs_continuously_until_stopped():
    """Regression for the multi-round freeze: the elapsed display must keep
    running for the whole turn. Only stop_turn_timer freezes it — a mid-turn
    cost settle no longer pauses it (app.py stopped calling pause_turn_timer per
    round), so _turn_start stays set and the display keeps advancing."""
    import time
    bar = StatusBar()
    bar.start_turn_timer()
    # Simulate a first LLM round settling: the app no longer pauses here, so the
    # timer is still live (turn_start set) rather than frozen.
    assert bar._turn_start is not None
    assert bar.is_turn_active is True
    time.sleep(0.02)
    # A later round: still running.
    assert bar._turn_start is not None
    # Only the final reply stops it.
    bar.stop_turn_timer()
    assert bar._turn_start is None
    assert bar.is_turn_active is False
