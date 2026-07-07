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
