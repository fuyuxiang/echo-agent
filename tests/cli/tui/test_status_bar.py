from echo_agent.cli.tui.status_bar import StatusBar


def test_initial_state_is_disconnected_and_has_no_model_segment():
    bar = StatusBar()
    text = bar._compose_text()
    # 握手完成前如实显示未连接，而不是一开就假装"●已连接"
    assert "○已断开" in text
    assert "●已连接" not in text
    # session 尚未填入
    assert text.count(" · ") >= 1


def test_connected_with_session_shows_no_model_placeholder():
    bar = StatusBar()
    bar.set_session("cli:local")
    bar.set_connection(True)
    text = bar._compose_text()
    assert "●已连接" in text
    assert "cli:local" in text
    assert "累计 $0.0" in text
    # 去掉 model 段后：conn·session·累计 之间恰好两个中点分隔，
    # 中间不再有 model 造成的空段 " ·  · "
    assert " ·  · " not in text


def test_set_model_does_not_appear_in_rendered_text():
    # set_model 仍可调用（接口保留），但模板不再渲染它
    bar = StatusBar()
    bar.set_session("cli:local")
    bar.set_model("opus")
    assert "opus" not in bar._compose_text()
    assert bar._model == "opus"  # 值仍被存下，供将来使用
