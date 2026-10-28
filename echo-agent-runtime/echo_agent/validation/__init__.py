"""Post-write incremental validation feedback (infrastructure, not an LLM tool)."""
from __future__ import annotations

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from echo_agent.validation.validator import Validator

_VALIDATOR: "Validator | None" = None


def set_validator(v: "Validator | None") -> None:
    global _VALIDATOR
    _VALIDATOR = v


def get_validator() -> "Validator | None":
    return _VALIDATOR
