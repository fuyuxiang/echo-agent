"""Token stream publisher — adaptive streaming with boundary-aware flushing.

Extracted from loop.py to keep the agent loop focused on orchestration.
"""

from __future__ import annotations

import re
import time
from dataclasses import dataclass, field
from typing import TYPE_CHECKING

from loguru import logger

from echo_agent.bus.delivery import DeliveryResult, DeliveryStage

if TYPE_CHECKING:
    from echo_agent.bus.events import InboundEvent
    from echo_agent.bus.queue import MessageBus


# NOTE: mirror of echo_agent.agent.pipeline.response_stage.ProcessResult;
# loop._process_event bridges between them. Add fields to BOTH or the bridge
# silently drops them.
@dataclass
class ProcessResult:
    response_text: str = ""
    outbound_sent: bool = False
    degraded_notices: list[str] = field(default_factory=list)
    task_incomplete: bool = False


def channel_matches(channel: str, patterns: list[str]) -> bool:
    """Exact name match, or a trailing ':*' prefix wildcard (``gateway:*``
    covers ``gateway:cli``, ``gateway:web``, ...).

    Lives here rather than on AgentLoop because both the loop and the inference
    stage need it, and the stage cannot import the loop (the loop imports the
    stage).
    """
    if channel in patterns:
        return True
    return any(p.endswith(":*") and channel.startswith(p[:-1]) for p in patterns)


class TokenStreamPublisher:
    _PARAGRAPH_RE = re.compile(r"\n\n")
    _SENTENCE_RE = re.compile(r"[。！？!?]\s|[。！？!?]$")
    _CODE_FENCE_RE = re.compile(r"^```|(?<=\n)```")
    _LIST_ITEM_RE = re.compile(r"^\s*(?:[-*+]|\d+\.)\s", re.MULTILINE)

    def __init__(
        self,
        bus: "MessageBus",
        event: "InboundEvent",
        *,
        enabled: bool,
        flush_chars: int,
        flush_interval_ms: int,
        paragraph_mode: bool = True,
        intro_text: str = "",
    ):
        self._bus = bus
        self._event = event
        self._enabled = enabled
        self._paragraph_mode = paragraph_mode
        self._flush_chars = max(1, flush_chars)
        self._flush_interval = max(0.05, flush_interval_ms / 1000.0)
        if self._paragraph_mode:
            self._flush_chars = max(120, self._flush_chars)
            self._flush_interval = max(1.2, self._flush_interval)
        self._full_text = ""
        self._pending = ""
        self._last_flush = time.monotonic()
        self._sent_nonfinal = False
        self._intro_text = intro_text.strip()
        self._needs_intro_separator = bool(self._intro_text)
        self._in_code_block = False

    # ------------------------------------------------------------------
    # Public API
    # ------------------------------------------------------------------

    async def start(self) -> None:
        if not self._enabled or not self._intro_text:
            return
        self._full_text = self._intro_text
        self._pending = self._intro_text

    async def on_delta(self, delta: str) -> None:
        if not self._enabled or not delta:
            return
        if self._needs_intro_separator:
            self._full_text += "\n\n"
            self._pending += "\n\n"
            self._needs_intro_separator = False
        self._full_text += delta
        self._pending += delta
        self._update_code_block_state(delta)
        now = time.monotonic()

        if self._paragraph_mode:
            if self._in_code_block:
                if self._code_block_just_closed(delta):
                    await self._flush(is_final=False)
                elif len(self._pending) >= self._flush_chars * 3:
                    await self._flush(is_final=False)
                return

            boundary = self._find_paragraph_boundary()
            if boundary > 0 and len(self._pending[:boundary]) >= self._flush_chars:
                await self._flush_up_to(boundary, is_final=False)
                return
            elapsed = now - self._last_flush
            if elapsed >= self._flush_interval:
                sentence_end = self._find_sentence_boundary()
                if sentence_end > 0 and len(self._pending[:sentence_end]) >= self._flush_chars:
                    await self._flush_up_to(sentence_end, is_final=False)
                elif sentence_end > 0 and elapsed >= self._flush_interval * 2:
                    await self._flush_up_to(sentence_end, is_final=False)
                elif elapsed >= self._flush_interval * 3:
                    if not self._is_in_list_block():
                        await self._flush(is_final=False)
        else:
            if len(self._pending) >= self._flush_chars or now - self._last_flush >= self._flush_interval:
                await self._flush(is_final=False)

    async def discard(self) -> None:
        """Retract the text streamed so far for this turn.

        Used when an optimistically-streamed draft turned out to be a pre-tool
        preamble: the real answer comes from a later iteration, so everything
        published so far must be disowned.

        Resetting internal state is not enough on its own. Consumers accumulate
        streamed deltas keyed by the inbound event id (the CLI TUI appends to one
        reply widget, attach_client appends to a printed block), so the next
        iteration's tokens would be concatenated onto the abandoned draft and the
        user would watch a spliced answer until the final frame replaced it. So we
        also publish an explicit reset frame — a streaming frame carrying
        `_stream_reset` — telling consumers to drop what they have for this turn
        and start over. Consumers that don't understand the flag see an empty
        streaming chunk, which is harmless.

        Resetting _sent_nonfinal additionally makes the eventual finalize() take
        the "never streamed" path and publish the answer as one full-text final
        frame. Channels that redraw in place (cli TUI set_markdown, IM edit)
        thereby overwrite it; send-only channels must not use this path at all —
        they get draft_policy="buffer" instead.

        Everything is cleared, including any intro text: response_stage prepends
        ctx.intro_text to the final response_text itself, so re-seeding it here
        would make the introduction appear twice.
        """
        if not self._enabled:
            return
        had_published = self._sent_nonfinal
        self._full_text = ""
        self._pending = ""
        self._sent_nonfinal = False
        self._in_code_block = False
        self._needs_intro_separator = False
        # Only announce a retraction if something was actually shown. A draft that
        # never left the buffer needs no reset frame, and sending one would make
        # channels open an empty reply block.
        if had_published:
            await self._publish("", is_final=False, reset=True)

    async def finalize(self, final_text: str) -> "DeliveryResult":
        # NO_HANDLER signals "streaming path not taken" so the caller falls back
        # to a plain final send; it is not a delivery failure.
        if not self._enabled:
            return DeliveryResult(DeliveryStage.NO_HANDLER, self._event.channel)

        if final_text.startswith(self._full_text):
            self._pending += final_text[len(self._full_text):]
            self._full_text = final_text
        elif not self._sent_nonfinal:
            self._full_text = final_text
            self._pending = final_text
        elif final_text != self._full_text:
            logger.debug("Stream final text diverged from streamed text for channel {}", self._event.channel)
            self._full_text = final_text

        if self._sent_nonfinal:
            self._pending = ""
            return await self._publish(self._full_text, is_final=True, full_text=True)

        return await self._publish(final_text, is_final=True, full_text=True)

    # ------------------------------------------------------------------
    # Boundary detection
    # ------------------------------------------------------------------

    def _find_paragraph_boundary(self) -> int:
        m = None
        for m in self._PARAGRAPH_RE.finditer(self._pending):
            pass
        return m.end() if m else 0

    def _find_sentence_boundary(self) -> int:
        m = None
        for m in self._SENTENCE_RE.finditer(self._pending):
            pass
        return m.end() if m else 0

    def _update_code_block_state(self, delta: str) -> None:
        count = len(self._CODE_FENCE_RE.findall(delta))
        if count % 2 == 1:
            self._in_code_block = not self._in_code_block

    def _code_block_just_closed(self, delta: str) -> bool:
        return "```" in delta and not self._in_code_block

    def _is_in_list_block(self) -> bool:
        lines = self._pending.rsplit("\n", 2)
        if len(lines) < 2:
            return False
        return bool(self._LIST_ITEM_RE.match(lines[-1]))

    # ------------------------------------------------------------------
    # Flush / publish
    # ------------------------------------------------------------------

    async def _flush(self, *, is_final: bool) -> None:
        text = self._pending
        self._pending = ""
        await self._publish(text, is_final=is_final)

    async def _flush_up_to(self, pos: int, *, is_final: bool) -> None:
        text = self._pending[:pos]
        self._pending = self._pending[pos:]
        await self._publish(text, is_final=is_final)

    async def _publish(
        self, text: str, *, is_final: bool, full_text: bool = False, reset: bool = False,
    ) -> "DeliveryResult":
        from echo_agent.bus.events import OutboundEvent

        if is_final and full_text:
            outbound = OutboundEvent.from_text_with_media(
                channel=self._event.channel,
                chat_id=self._event.chat_id,
                text=text,
                reply_to_id=self._event.reply_to_id,
            )
        else:
            outbound = OutboundEvent.text_reply(
                channel=self._event.channel,
                chat_id=self._event.chat_id,
                text=text,
                reply_to_id=self._event.reply_to_id,
            )
        outbound.is_final = is_final
        outbound.message_kind = "final" if is_final else "streaming"
        outbound.metadata = dict(self._event.metadata)
        outbound.metadata["_inbound_event_id"] = self._event.event_id
        outbound.metadata["_token_stream"] = True
        if full_text:
            outbound.metadata["_stream_full_text"] = True
        if reset:
            # "Drop everything streamed for this turn so far." Carried in
            # metadata so channels that ignore it just see an empty chunk.
            outbound.metadata["_stream_reset"] = True
        result = await self._bus.publish_outbound(outbound)
        self._last_flush = time.monotonic()
        if not is_final and text:
            self._sent_nonfinal = True
        return result


# Backward-compatible aliases
_TokenStreamPublisher = TokenStreamPublisher
_ProcessResult = ProcessResult
