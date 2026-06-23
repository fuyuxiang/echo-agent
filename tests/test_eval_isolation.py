from echo_agent.agent.pipeline.response_stage import _is_ephemeral_session


def test_eval_channel_is_ephemeral():
    assert _is_ephemeral_session("eval:case-1", "eval") is True
    assert _is_ephemeral_session("test:x", "test") is True
    assert _is_ephemeral_session("telegram:123", "telegram") is False
    assert _is_ephemeral_session("", "") is False
