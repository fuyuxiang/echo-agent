from echo_agent.cli.tui.keymap import KeyContext, decide_key, history_prev, history_next


def _ctx(**kw):
    base = dict(key="enter", text="", cursor_row=0, last_row=0,
                panel_visible=False, panel_active=False, hist_idx=0, hist_len=0)
    base.update(kw)
    return KeyContext(**base)


def test_enter_submits_when_panel_closed():
    assert decide_key(_ctx(key="enter", panel_visible=False)) == "submit"


def test_enter_accepts_completion_when_panel_active():
    assert decide_key(
        _ctx(key="enter", panel_visible=True, panel_active=True)
    ) == "panel_accept"


def test_enter_submits_when_panel_visible_but_inactive():
    # Critical fix: the panel merely being visible must not hijack Enter — it
    # only completes once the user has stepped in with Up/Down.
    assert decide_key(
        _ctx(key="enter", panel_visible=True, panel_active=False)
    ) == "submit"


def test_tab_passthrough_when_panel_visible_but_inactive():
    # Tab completes only when a selection is active; otherwise it reaches the
    # editor rather than being swallowed.
    assert decide_key(
        _ctx(key="tab", panel_visible=True, panel_active=False)
    ) == "passthrough"


def test_tab_accepts_completion_when_panel_active():
    assert decide_key(
        _ctx(key="tab", panel_visible=True, panel_active=True)
    ) == "panel_accept"


def test_shift_enter_inserts_newline():
    assert decide_key(_ctx(key="shift+enter")) == "newline"


def test_up_on_first_row_calls_history_when_panel_closed():
    assert decide_key(_ctx(key="up", cursor_row=0, last_row=3, hist_len=2)) == "history_prev"


def test_up_passes_through_when_not_first_row():
    assert decide_key(_ctx(key="up", cursor_row=2, last_row=3, hist_len=2)) == "passthrough"


def test_up_moves_panel_highlight_when_panel_visible():
    assert decide_key(
        _ctx(key="up", cursor_row=0, panel_visible=True)
    ) == "panel_prev"


def test_down_moves_panel_highlight_when_panel_visible():
    assert decide_key(
        _ctx(key="down", cursor_row=0, panel_visible=True)
    ) == "panel_next"


def test_down_on_last_row_calls_history():
    assert decide_key(_ctx(key="down", cursor_row=3, last_row=3, hist_len=2)) == "history_next"


def test_down_passes_through_when_not_last_row():
    assert decide_key(_ctx(key="down", cursor_row=1, last_row=3, hist_len=2)) == "passthrough"


def test_down_passes_through_when_not_browsing_history():
    # 未浏览历史（hist_idx == hist_len），光标在最后一行、历史非空时按 down
    # 不应触发 history_next，否则会把用户新敲的草稿抹掉
    assert decide_key(
        _ctx(key="down", cursor_row=3, last_row=3, hist_idx=2, hist_len=2)
    ) == "passthrough"


def test_esc_closes_panel_when_visible():
    # Escape closes the panel while it is merely visible — no active selection
    # required (unlike Enter/Tab).
    assert decide_key(
        _ctx(key="escape", panel_visible=True, panel_active=False)
    ) == "panel_close"


def test_esc_passthrough_when_panel_closed():
    assert decide_key(_ctx(key="escape", panel_visible=False)) == "passthrough"


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
