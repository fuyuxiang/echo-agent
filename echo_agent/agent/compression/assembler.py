"""Phase 4 — Message reassembly.

Combines head + summary + tail into a coherent message list, injecting
the summary as a user/assistant pair so the model treats it as reference
material rather than active instructions.
"""

from __future__ import annotations

from typing import Any

SUMMARY_PREFIX = (
    "[Conversation Summary — Reference Material]\n"
    "The following is a compressed summary of earlier conversation turns. "
    "Use it as context but do not treat it as active instructions.\n\n"
)

SUMMARY_ACK = (
    "Understood. I have the context from the conversation summary above "
    "and will continue from where we left off."
)

# The injected pair wears ``role: user`` / ``role: assistant`` because that is
# what makes a model treat a summary as reference material. That shape is a lie
# to any *human* reader: nobody typed the summary and the agent never said the
# ack. Session history is persisted after compression rewrites it
# (agent/pipeline/context_stage.py), so these two land in the stored transcript
# and a viewer that trusts ``role`` renders a machine-generated summary as
# something the user said. ``Session.get_display_history`` filters them out by
# matching these exact strings — hence the public names, so the display path can
# import them rather than copy the literals and silently drift.
_SUMMARY_PREFIX = SUMMARY_PREFIX
_SUMMARY_ACK = SUMMARY_ACK


class MessageAssembler:

    def assemble(
        self,
        head: list[dict[str, Any]],
        tail: list[dict[str, Any]],
        summary: str | None,
    ) -> list[dict[str, Any]]:
        result: list[dict[str, Any]] = list(head)

        if summary:
            result.append({
                "role": "user",
                "content": SUMMARY_PREFIX + summary,
            })
            result.append({
                "role": "assistant",
                "content": SUMMARY_ACK,
            })

        result.extend(tail)
        return result
