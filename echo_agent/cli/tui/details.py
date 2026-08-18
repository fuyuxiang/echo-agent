"""How much of the agent's working trace the transcript shows.

Visibility is a per-section setting with four states:

- ``expanded``  — the line is shown, and its detail view opens on mount.
- ``collapsed`` — the line is shown as a one-row summary; detail on demand.
- ``lean``      — (tools only) successful read-only calls suppressed; writes/
                  exec/failures shown. The default for tools.
- ``hidden``    — the section is not mounted at all.

The sections are the three bands of trace that answer different questions:
``thinking`` (why the agent decided this), ``tools`` (what it did to the world),
``activity`` (what it is doing right now).

``hidden`` is deliberately NOT allowed to hide failures. An error is the one
trace the user cannot act on if they never see it. Same rule applies to
``lean``: a failed read is still mounted.
"""

from __future__ import annotations

import os
from dataclasses import dataclass, replace
from typing import Mapping

from echo_agent.security.risk_classifier import RiskLevel, classify_risk

# Section keys, in the order /details lists them.
SECTIONS: tuple[str, ...] = ("thinking", "tools", "activity")

STATES: tuple[str, ...] = ("expanded", "collapsed", "lean", "hidden")

SECTION_DEFAULTS: dict[str, str] = {
    "thinking": "collapsed",
    "tools": "lean",
    "activity": "hidden",
}

# Which cog_type belongs to which section. Types absent here (approval_request,
# clarify_request, evolution, …) are not trace — they are things the user must
# see or act on — so they are never routed through this filter.
_SECTION_OF_COG: dict[str, str] = {
    "thinking": "thinking",
    "tool_call": "tools",
    "memory_recalled": "thinking",
    "memory_written": "thinking",
    "heartbeat": "activity",
    "cost_update": "activity",
}

SECTION_LABELS: dict[str, str] = {
    "thinking": "思考与记忆",
    "tools": "工具调用",
    "activity": "运行状态",
}

STATE_LABELS: dict[str, str] = {
    "expanded": "展开",
    "collapsed": "折叠",
    "lean": "精简",
    "hidden": "隐藏",
}

_ENV_VAR = "ECHO_TUI_DETAILS"

# Typed at the prompt, not written in a config file, so the words shown in the
# UI must be the words that work: /help lists these sections in Chinese, and a
# user copying that text back verbatim should not get "参数无效".
_SECTION_ALIASES: dict[str, str] = {
    "思考": "thinking", "记忆": "thinking", "think": "thinking",
    "工具": "tools", "tool": "tools",
    "状态": "activity", "运行状态": "activity", "act": "activity",
}

_STATE_ALIASES: dict[str, str] = {
    "展开": "expanded", "open": "expanded", "full": "expanded", "all": "expanded",
    "折叠": "collapsed", "收起": "collapsed", "short": "collapsed",
    "精简": "lean", "简": "lean",
    "隐藏": "hidden", "关闭": "hidden", "off": "hidden", "none": "hidden",
}


@dataclass(frozen=True)
class DetailPrefs:
    """Immutable visibility settings. Frozen (and copied via ``with_section``)
    so a preference change is a single assignment the caller can diff against
    the old value — the transcript re-renders mounted blocks from that diff, and
    a half-applied mutation would leave the screen disagreeing with the state."""

    thinking: str = SECTION_DEFAULTS["thinking"]
    tools: str = SECTION_DEFAULTS["tools"]
    activity: str = SECTION_DEFAULTS["activity"]

    def state(self, section: str) -> str:
        """Visibility of ``section``; unknown sections read as ``collapsed``.

        A gateway can ship a new cog_type before this client knows it, and the
        safe default for an unclassified trace is visible-but-quiet rather than
        dropped on the floor.
        """
        return getattr(self, section, "collapsed") if section in SECTIONS else "collapsed"

    def section_of(self, cog_type: str) -> str | None:
        """Which section a cognitive type belongs to, or None when it is not
        trace at all (approvals, clarifies — always shown)."""
        return _SECTION_OF_COG.get(cog_type)

    def shows(self, cog_type: str, *, failed: bool = False, tool_name: str = "") -> bool:
        """Whether a frame of this type gets mounted.

        ``failed`` overrides: a tool that errored is mounted even when its
        section is hidden or lean.  Under ``lean``, only read-only tools that
        succeeded are suppressed.
        """
        if failed:
            return True
        section = self.section_of(cog_type)
        if section is None:
            return True
        st = self.state(section)
        if st == "hidden":
            return False
        if st == "lean" and section == "tools" and tool_name:
            return classify_risk(tool_name) != RiskLevel.READ_ONLY
        return True

    def starts_expanded(self, cog_type: str) -> bool:
        """Whether the block opens its detail view on mount."""
        section = self.section_of(cog_type)
        if section is None:
            return False
        return self.state(section) == "expanded"

    def with_section(self, section: str, state: str) -> "DetailPrefs":
        if section not in SECTIONS:
            raise ValueError(f"unknown section: {section}")
        if state not in STATES:
            raise ValueError(f"unknown state: {state}")
        # activity has no expanded view (heartbeat/cost_update are handled
        # inline and never create transcript blocks), so treat it as collapsed.
        if section == "activity" and state == "expanded":
            state = "collapsed"
        return replace(self, **{section: state})

    def describe(self) -> list[tuple[str, str]]:
        """(section label, state label) pairs for the /details listing."""
        return [
            (SECTION_LABELS[s], STATE_LABELS[self.state(s)])
            for s in SECTIONS
        ]


def parse_env(env: Mapping[str, str] | None = None) -> DetailPrefs:
    """Read defaults from ``ECHO_TUI_DETAILS``, e.g. ``thinking=expanded,tools=collapsed``.

    Exists so a user who always wants the thinking text open does not retype
    /details every session. Malformed pairs are skipped rather than raising: a
    stale value in a shell profile must not stop the TUI from starting.
    """
    raw = (env if env is not None else os.environ).get(_ENV_VAR, "")
    prefs = DetailPrefs()
    for chunk in raw.replace(";", ",").split(","):
        section, sep, state = chunk.partition("=")
        if not sep:
            continue
        section, state = section.strip().lower(), state.strip().lower()
        if section in SECTIONS and state in STATES:
            prefs = prefs.with_section(section, state)
    return prefs


def parse_command(arg: str) -> tuple[str, str] | None:
    """Parse a ``/details`` argument into (section, state).

    Accepts ``<section> <state>`` and ``<section>=<state>``, in English keys or
    the Chinese words the help text shows. Returns None for a bare ``/details``
    (which lists the current settings) or anything unparseable — the caller turns
    that into a usage hint rather than a silent no-op.
    """
    text = arg.strip().lower().replace("=", " ").replace("，", " ").replace(",", " ")
    parts = text.split()
    if len(parts) != 2:
        return None
    section = _SECTION_ALIASES.get(parts[0], parts[0])
    state = _STATE_ALIASES.get(parts[1], parts[1])
    if section not in SECTIONS or state not in STATES:
        return None
    return section, state
