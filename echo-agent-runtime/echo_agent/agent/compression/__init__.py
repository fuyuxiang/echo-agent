"""Context compression engine — multi-phase intelligent context management."""

from __future__ import annotations

from echo_agent.agent.compression.compressor import ConversationCompressor
from echo_agent.agent.compression.engine import ContextEngine
from echo_agent.agent.compression.types import CompressionResult, CompressionStats

__all__ = [
    "ContextEngine",
    "ConversationCompressor",
    "CompressionResult",
    "CompressionStats",
]
