"""ConversationCompressor — orchestrates the 5-phase compression pipeline.

Phase 1: Tool output pruning (cheap, no LLM)
Phase 2: Boundary resolution (head / middle / tail)
Phase 3: LLM summary generation
Phase 4: Message reassembly
Phase 5: Structural validation
"""

from __future__ import annotations

import time
from typing import Any

from loguru import logger

from echo_agent.agent.compression.assembler import MessageAssembler
from echo_agent.agent.compression.boundary import BoundaryResolver
from echo_agent.agent.compression.engine import ContextEngine
from echo_agent.agent.compression.pruner import ToolOutputPruner
from echo_agent.agent.compression.summarizer import LLMSummarizer
from echo_agent.agent.compression.types import CompressionResult
from echo_agent.agent.compression.validator import MessageValidator
from echo_agent.config.schema import CompressionConfig
from echo_agent.models.provider import LLMProvider


class ConversationCompressor(ContextEngine):

    def __init__(
        self,
        config: CompressionConfig,
        context_window_tokens: int,
        provider: LLMProvider,
        default_model: str,
    ):
        super().__init__(
            context_window_tokens=context_window_tokens,
            trigger_ratio=config.trigger_ratio,
        )
        self._config = config

        self._pruner = ToolOutputPruner(
            tail_budget_ratio=config.tool_pruning_tail_budget_ratio,
            context_window_tokens=context_window_tokens,
            token_estimator=self.estimate_tokens,
        ) if config.tool_pruning_enabled else None

        self._boundary = BoundaryResolver(
            head_protect_count=config.head_protect_count,
            tail_budget_ratio=config.tail_budget_ratio,
            context_window_tokens=context_window_tokens,
            token_estimator=self.estimate_tokens,
        )

        self._summarizer = LLMSummarizer(
            provider=provider,
            summary_model=config.summary_model,
            default_model=default_model,
            summary_target_ratio=config.summary_target_ratio,
            summary_min_tokens=config.summary_min_tokens,
            summary_max_tokens=config.summary_max_tokens,
            cooldown_seconds=config.summary_cooldown_seconds,
        )

        self._assembler = MessageAssembler()
        self._validator = MessageValidator()

    def should_compress(self, messages: list[dict[str, Any]]) -> bool:
        if not self._config.enabled:
            return False
        return super().should_compress(messages)

    async def compress(
        self,
        messages: list[dict[str, Any]],
        focus_topic: str = "",
    ) -> CompressionResult:
        tokens_before = self.estimate_tokens(messages)
        working = list(messages)

        # Phase 1: Tool output pruning
        pruned_count = 0
        if self._pruner:
            prune_result = self._pruner.prune(working)
            working = prune_result.messages
            pruned_count = prune_result.pruned_count
            if pruned_count:
                logger.debug("Phase 1: pruned {} tool outputs", pruned_count)

        # Phase 2: Boundary resolution
        boundary = self._boundary.resolve(working)
        if boundary.no_compression_needed:
            return CompressionResult(
                messages=working,
                stats=self._stats,
                was_compressed=False,
                tokens_before=tokens_before,
                tokens_after=self.estimate_tokens(working),
            )

        logger.debug(
            "Phase 2: head={} middle={} tail={}",
            len(boundary.head_messages),
            len(boundary.middle_messages),
            len(boundary.tail_messages),
        )

        # Phase 3: LLM summary generation
        summary = await self._summarizer.summarize(
            middle_messages=boundary.middle_messages,
            focus_topic=focus_topic,
            stats=self._stats,
            token_estimator=self.estimate_tokens,
        )

        if summary:
            logger.debug("Phase 3: generated summary ({} chars)", len(summary))
        else:
            logger.debug("Phase 3: summary skipped or failed")

        # Phase 4: Message reassembly
        assembled = self._assembler.assemble(
            head=boundary.head_messages,
            tail=boundary.tail_messages,
            summary=summary,
        )

        # Phase 5: Structural validation
        validated = self._validator.validate(assembled)

        tokens_after = self.estimate_tokens(validated)
        saved = tokens_before - tokens_after

        self._stats.compression_count += 1
        self._stats.last_compressed_at = time.time()
        self._stats.total_tokens_saved += max(saved, 0)

        if self._stats.compression_count >= self._config.max_compression_count:
            warning = (
                f"Context compressed {self._stats.compression_count} times — "
                "summary quality may be degrading"
            )
            if warning not in self._stats.warnings:
                self._stats.warnings.append(warning)
                logger.warning(warning)

        logger.info(
            "Compression complete: {} → {} tokens (saved {}, {:.0f}%)",
            tokens_before, tokens_after, saved,
            (saved / tokens_before * 100) if tokens_before else 0,
        )

        return CompressionResult(
            messages=validated,
            stats=self._stats,
            summary_text=summary,
            was_compressed=True,
            tokens_before=tokens_before,
            tokens_after=tokens_after,
        )
