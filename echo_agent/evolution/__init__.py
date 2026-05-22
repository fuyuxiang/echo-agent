"""Self-evolving skill harness.

Closed-loop pipeline:
  TrajectoryRecorder  → captures runtime experience via plugin hooks
  TrajectoryStore     → SQLite-backed persistence for trajectories/candidates/runs
  Evolver             → LLM-driven candidate skill proposals
  PromotionGate       → A/B evaluation against a baseline dataset before promotion
  EvolutionScheduler  → manual / threshold / cron triggers
  EvolutionEngine     → orchestrates the full record → propose → gate → promote loop
"""

from __future__ import annotations

from echo_agent.evolution.engine import EvolutionEngine
from echo_agent.evolution.evolver import Evolver
from echo_agent.evolution.gate import PromotionGate
from echo_agent.evolution.recorder import TrajectoryRecorder
from echo_agent.evolution.scheduler import EvolutionScheduler
from echo_agent.evolution.store import TrajectoryStore
from echo_agent.evolution.types import (
    EvolutionRun,
    SkillCandidate,
    ToolCall,
    Trajectory,
)

__all__ = [
    "EvolutionEngine",
    "EvolutionRun",
    "EvolutionScheduler",
    "Evolver",
    "PromotionGate",
    "SkillCandidate",
    "ToolCall",
    "Trajectory",
    "TrajectoryRecorder",
    "TrajectoryStore",
]
