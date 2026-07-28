"""Turn grouping: which turn a transcript block belongs to, and how deep it sits.

P2 deliberately does NOT introduce a nested ``TurnBlock`` container widget, which
was the obvious shape. Three concrete problems killed it:

1. The pending clarify/approval blocks must stay unconditionally visible and
   keyboard-reachable — app.py documents past freezes where a retired prompt went
   inert while still on screen and the next keystroke started a new turn that
   queued forever. Inside a collapsible container those blocks inherit the
   container's visibility, so one stray collapse (or a container removed by
   /clear) hides the thing the disabled input box is telling the user to answer.
2. ``TranscriptView.children`` is walked by /copy, /save, ctrl+r/ctrl+o and the
   focus ring. Nesting turns them into recursive walks — five call sites that
   each have to get the recursion right, for no user-visible gain.
3. Textual's ``anchor()`` auto-follow tracks the scroll container's own content
   height. Mounting into a nested child while streaming makes the anchor fight
   the container's height recalculation.

So the turn is a *label on flat blocks*, not a container of them: every block
carries ``turn_seq`` (which turn it belongs to) and renders its own rail from a
fixed depth. Same visual result — blocks that read as one turn's working area —
with none of the nesting hazards. The transcript stays a flat list, which is also
what makes the group-boundary spacing rule in layout.py work.
"""

from __future__ import annotations

from echo_agent.cli.tui.glyphs import GLYPHS

# A turn's trace lines sit one level in from its title and its answer, so the eye
# can see where the working area starts and stops without a container border.
TRACE_DEPTH = 1


def rail_prefix(depth: int) -> str:
    """Indent for a block nested ``depth`` levels inside its turn.

    A plain repeated rail, NOT a ``├─``/``└─`` pair. Which trace line is "last"
    is only known once the next one arrives (or the turn ends), so an elbow would
    mean re-rendering the previous block on every single frame — and mid-stream
    there is no correct answer to re-render it to. A uniform rail is stable: a
    block's prefix is decided once, when it mounts, and never changes.
    """
    if depth <= 0:
        return ""
    return GLYPHS.rail * depth
