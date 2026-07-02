import inspect

from echo_agent.agent.pipeline.context_stage import ContextStage
from echo_agent.agent.pipeline.inference_stage import InferenceStage


def test_stages_accept_cognitive_emitter():
    assert "cognitive_emitter" in inspect.signature(InferenceStage.__init__).parameters
    assert "cognitive_emitter" in inspect.signature(ContextStage.__init__).parameters
