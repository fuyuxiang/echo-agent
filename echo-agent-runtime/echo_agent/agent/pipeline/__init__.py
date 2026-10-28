"""Pipeline package — decomposes AgentLoop into discrete stages."""

from echo_agent.agent.pipeline.types import PipelineContext, InferenceResult
from echo_agent.agent.pipeline.context_stage import ContextStage
from echo_agent.agent.pipeline.inference_stage import InferenceStage
from echo_agent.agent.pipeline.response_stage import ResponseStage

__all__ = [
    "PipelineContext",
    "InferenceResult",
    "ContextStage",
    "InferenceStage",
    "ResponseStage",
]
