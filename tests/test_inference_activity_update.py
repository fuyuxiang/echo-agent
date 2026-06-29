import time

import pytest
from unittest.mock import AsyncMock

from echo_agent.agent.progress_heartbeat import SharedActivityState
from echo_agent.agent.pipeline.types import PipelineContext
from echo_agent.models.provider import LLMResponse

from test_inference_stage import _make_stage, _make_ctx


def test_pipeline_context_has_activity_field():
    ctx = PipelineContext(event=object(), session=object(), trace_id="t", publish_response=False)
    assert ctx.activity is None
    st = SharedActivityState(started_at=time.monotonic())
    ctx.activity = st
    assert ctx.activity is st


@pytest.mark.asyncio
async def test_set_generating_called_before_final_answer():
    """模型返回无工具调用的最终内容时，activity 进入 generating 收尾里程碑。"""
    provider = AsyncMock()
    provider.chat_stream_with_retry = AsyncMock(
        return_value=LLMResponse(content="done", finish_reason="stop")
    )
    stage, _bus = _make_stage(provider=provider)
    ctx = _make_ctx()
    ctx.activity = SharedActivityState(started_at=time.monotonic())

    await stage.run(ctx)

    assert ctx.activity.phase == "generating"
    assert ctx.activity.milestone_seq >= 1
