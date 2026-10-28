"""Configuration package."""

from __future__ import annotations

from echo_agent.config.loader import load_config
from echo_agent.config.schema import Config

__all__ = ["Config", "load_config"]
