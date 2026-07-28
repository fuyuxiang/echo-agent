"""Vertical rhythm for the transcript: which blocks open a blank line.

Every block used to carry its own margin (``AgentReply { margin: 0 0 1 0 }``),
which produced two wrong results. A run of tool/cognitive traces got no
separation from the answer that followed it, so the eye could not find where the
process ended and the conclusion began; and any block that rendered nothing
(collapsed or hidden) still contributed its margin, leaving a floating gap.

Instead, blocks are classified into visual *groups* and exactly one blank line
opens where the group changes. A run of traces reads as one section; the gap
appears only where the KIND of content changes.

  user  — the human turn (owns its own margins)
  model — the agent's prose answer
  trail — tool calls and cognitive traces (the working area)
  note  — client notices and server errors (a quieter band)
  ui    — banner and interactive prompts (own their margins, never gapped here)

Pure functions over group names — no widget imports — so the rule is
unit-testable and the widgets stay free of layout arithmetic.
"""

from __future__ import annotations

GROUPS = ("user", "model", "trail", "note", "ui")

# Groups whose own chrome already paints the space around them: UserTurn has
# margins + a left rule, and the banner/approval/choice blocks have margins in
# app.tcss. Adding a computed gap on top would double it.
_SELF_SPACED = frozenset({"user", "ui"})

# Groups that paint a trailing blank line beneath themselves, so the block that
# follows must not add its own leading gap or the single boundary doubles.
_PAINTS_TRAILING_GAP = frozenset({"user", "ui"})


def lead_gap(prev_group: str | None, group: str) -> bool:
    """Whether a block in ``group`` opens one blank line above itself, given the
    group of the nearest block above it that actually rendered.

    ``prev_group`` is None at the top of the transcript (nothing to separate
    from). Deliberately a function of the PREDECESSOR's group only, never of the
    current block's live content: a streaming reply must compute the same gap
    while it streams as it does once settled, or the view would jump on every
    token.
    """
    if group in _SELF_SPACED:
        return False
    if prev_group is None:
        return False
    if prev_group == group:
        return False
    return prev_group not in _PAINTS_TRAILING_GAP
