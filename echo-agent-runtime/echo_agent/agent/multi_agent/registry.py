"""Worker registry — stores worker profile templates for delegation."""

from __future__ import annotations

from collections.abc import Iterable

from echo_agent.agent.multi_agent.models import WorkerProfile


class WorkerRegistry:
    """Stores worker profile templates that DelegateTool can reference."""

    def __init__(self, profiles: Iterable[WorkerProfile]):
        self._profiles = {p.id: p for p in profiles if p.id}

    @classmethod
    def from_config(cls, config) -> "WorkerRegistry":
        profiles = []
        for cfg in getattr(config, "worker_profiles", []):
            profiles.append(WorkerProfile(
                id=cfg.id,
                name=cfg.name or cfg.id,
                description=cfg.description,
                instructions=cfg.instructions,
                default_tools=tuple(cfg.default_tools),
                model=cfg.model,
                max_iterations=cfg.max_iterations,
                max_tokens=cfg.max_tokens,
                temperature=cfg.temperature,
            ))
        return cls(profiles)

    def get(self, profile_id: str) -> WorkerProfile | None:
        return self._profiles.get(profile_id)

    def list(self) -> list[WorkerProfile]:
        return list(self._profiles.values())

    def list_ids(self) -> list[str]:
        return list(self._profiles.keys())
