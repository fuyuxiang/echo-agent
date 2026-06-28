import time
from echo_agent.agent.progress_heartbeat import SharedActivityState
from echo_agent.agent.pipeline.types import PipelineContext


def test_pipeline_context_has_activity_field():
    ctx = PipelineContext(event=object(), session=object(), trace_id="t", publish_response=False)
    assert ctx.activity is None
    st = SharedActivityState(started_at=time.monotonic())
    ctx.activity = st
    assert ctx.activity is st
