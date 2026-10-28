from echo_agent.agent.pipeline.response_stage import _is_ephemeral_session


def test_eval_channel_is_ephemeral():
    assert _is_ephemeral_session("eval:case-1", "eval") is True
    assert _is_ephemeral_session("test:x", "test") is True
    assert _is_ephemeral_session("telegram:123", "telegram") is False
    assert _is_ephemeral_session("", "") is False


def test_context_stage_gates_ephemeral_reads():
    # build() 的快照与检索两条读记忆分支都必须带 ephemeral 短路。
    from echo_agent.agent.pipeline import context_stage
    import inspect
    source = inspect.getsource(context_stage.ContextStage.build)
    assert "_is_ephemeral_session" in source, "build() 未对 eval/test 通道短路记忆读取"
    # 快照分支与检索分支都要被 ephemeral 门控
    assert source.count("ephemeral") >= 3


def test_context_stage_imports_ephemeral_helper():
    from echo_agent.agent.pipeline import context_stage
    assert hasattr(context_stage, "_is_ephemeral_session")
