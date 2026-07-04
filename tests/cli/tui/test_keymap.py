from echo_agent.cli.tui.keymap import KeyContext, decide_key, history_prev, history_next


def _ctx(**kw):
    base = dict(key="enter", text="", cursor_row=0, last_row=0,
                panel_open=False, hist_idx=0, hist_len=0)
    base.update(kw)
    return KeyContext(**base)


def test_enter_submits_when_panel_closed():
    assert decide_key(_ctx(key="enter", panel_open=False)) == "submit"


def test_enter_accepts_completion_when_panel_open():
    assert decide_key(_ctx(key="enter", panel_open=True)) == "panel_accept"


def test_shift_enter_inserts_newline():
    assert decide_key(_ctx(key="shift+enter")) == "newline"


def test_up_on_first_row_calls_history_when_panel_closed():
    assert decide_key(_ctx(key="up", cursor_row=0, last_row=3, hist_len=2)) == "history_prev"


def test_up_passes_through_when_not_first_row():
    assert decide_key(_ctx(key="up", cursor_row=2, last_row=3, hist_len=2)) == "passthrough"


def test_up_moves_panel_highlight_when_panel_open():
    assert decide_key(_ctx(key="up", cursor_row=0, panel_open=True)) == "panel_prev"


def test_down_on_last_row_calls_history():
    assert decide_key(_ctx(key="down", cursor_row=3, last_row=3, hist_len=2)) == "history_next"


def test_down_passes_through_when_not_last_row():
    assert decide_key(_ctx(key="down", cursor_row=1, last_row=3, hist_len=2)) == "passthrough"


def test_esc_closes_panel_when_open():
    assert decide_key(_ctx(key="escape", panel_open=True)) == "panel_close"


def test_esc_passthrough_when_panel_closed():
    assert decide_key(_ctx(key="escape", panel_open=False)) == "passthrough"


def test_up_passthrough_when_history_empty():
    assert decide_key(_ctx(key="up", cursor_row=0, last_row=0, hist_len=0)) == "passthrough"


def test_history_prev_clamps_at_zero():
    assert history_prev(0, 3) == 0


def test_history_prev_steps_back():
    assert history_prev(3, 3) == 2


def test_history_next_clamps_at_length():
    assert history_next(3, 3) == 3


def test_history_next_steps_forward():
    assert history_next(1, 3) == 2
