"""Pipeline data types shared across stages."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, TYPE_CHECKING

if TYPE_CHECKING:
    from echo_agent.agent.planning.models import Plan
    from echo_agent.bus.events import InboundEvent
    from echo_agent.session.manager import Session


@dataclass
class PipelineContext:
    """Carries state through the pipeline stages."""

    event: InboundEvent
    session: Session
    trace_id: str
    publish_response: bool

    system_prompt: str = ""
    messages: list[dict[str, Any]] = field(default_factory=list)
    tool_defs: list[dict[str, Any]] = field(default_factory=list)
    retrieval: str = ""
    task_type: str = "chat"
    execution_plan: Plan | None = None
    plan_run_id: str = ""
    intro_text: str = ""
    stream_publisher: Any = None
    activity: Any = None  # SharedActivityState | None; updated by inference, read by heartbeat


@dataclass
class InferenceResult:
    """Output of the inference stage."""

    response_text: str = ""
    total_tool_calls: int = 0
    should_review_skills: bool = False
    should_review_memory: bool = False
    degraded_notices: list[str] = field(default_factory=list)
    # True when the turn did NOT cleanly finish the task: the loop hit its
    # iteration ceiling, the cost budget halted it, the final answer was forced,
    # the user interrupted, or the provider returned an error. A dispatched board
    # task must then be written back as FAILED, not SUCCESS. Chat turns ignore
    # this — it only gates the task-outcome safety net in AgentLoop.
    task_incomplete: bool = False
