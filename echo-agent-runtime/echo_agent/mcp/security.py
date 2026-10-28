"""MCP security — prompt-injection scanning, name derivation and collision guards.

Everything a remote MCP server tells us about its tools is attacker-controlled
text that lands in the model's context: the tool name, its description, and
every ``title``/``description`` inside its ``inputSchema``. This module is the
single gate that text passes through, and it owns tool-name derivation as well,
because a name is only safe once it is known not to collide with another one.

Two lessons are baked into the pattern set below:

* **Precision beats coverage when the policy is ``block``.** The first version
  matched ``you must``, ``always respond`` and ``eval(`` — phrasings so common
  in legitimate tool descriptions that the default policy silently discarded
  working tools ("You must provide a valid API key." was enough). A scanner
  that costs real functionality gets switched off, and then it protects
  nothing. Every pattern here has to be one that a benign description is very
  unlikely to contain.
* **Coverage still has to reach every field the model reads.** Scanning only
  the top-level description while the model also reads parameter descriptions
  is not a partial defence, it is a bypass with an extra step.
"""

from __future__ import annotations

import hashlib
import re
from dataclasses import dataclass
from typing import Any

from loguru import logger

#: Upper bound on a registered tool name. Model providers cap function names
#: (OpenAI at 64 chars); a server naming a tool 200 characters long would
#: otherwise produce a definition the provider rejects — taking down the whole
#: request, not just that tool.
MAX_TOOL_NAME_LENGTH = 64

_INJECTION_PATTERNS: list[tuple[str, re.Pattern]] = [
    # "ignore/disregard/forget all previous instructions" and its near
    # neighbours, including the leetspeak and singular forms that the original
    # literal pattern missed.
    ("instruction_override", re.compile(
        r"(?:ign[o0]re|disregard|forget|override|bypass|skip)\s+"
        r"(?:all\s+|any\s+|the\s+|your\s+)*"
        r"(?:previous|prior|earlier|above|preceding|system|initial)\s+"
        r"(?:instruction|directive|prompt|rule|command|guideline|constraint)s?",
        re.I,
    )),
    # Fake turn boundaries / role headers — an attempt to make injected text
    # look like it came from the harness rather than from tool output.
    ("role_injection", re.compile(
        r"(<\s*/?\s*(?:system|assistant|admin|developer)\s*>"
        r"|<\|\s*(?:im_start|im_end|system|assistant)\s*\|>"
        r"|\[\s*/?INST\s*\]"
        r"|^\s*(?:system|assistant)\s*:\s*you\s+are)",
        re.I | re.M,
    )),
    ("prompt_leak", re.compile(
        r"(?:reveal|show|print|output|repeat|disclose|dump)\s+"
        r"(?:me\s+|us\s+)?(?:your|the|all)\s+"
        r"(?:full\s+|complete\s+|entire\s+|initial\s+|original\s+)*"
        r"(?:system\s+)?(?:prompt|instruction|context|guideline)s?",
        re.I,
    )),
    ("jailbreak_attempt", re.compile(
        r"(?:\bDAN\b|do\s+anything\s+now|developer\s+mode"
        r"|bypass\s+(?:safety|security|filter|restriction|guardrail)"
        r"|without\s+(?:any\s+)?(?:restriction|limitation|refusal)s?)",
        re.I,
    )),
    # Instructions to keep the user out of the loop. A legitimate tool
    # description has no reason to tell the model to conceal its own activity,
    # which makes this both high-precision and high-signal: concealment is a
    # precondition for most damaging tool-mediated attacks.
    ("covert_channel", re.compile(
        r"(?:do\s*n[o']?t|don't|never|avoid)\s+"
        r"(?:\w+\s+){0,3}?"
        r"(?:tell|telling|inform|informing|mention|mentioning|reveal|revealing"
        r"|show|showing|notify|notifying|ask|asking)\s+"
        r"(?:it\s+|this\s+|that\s+|to\s+)*(?:the\s+)?(?:user|human|operator|owner)",
        re.I,
    )),
    # Exfiltration: a transmit verb aimed at a credential-shaped noun. Requiring
    # both halves is what keeps "Uploads data to the endpoint." from matching.
    ("credential_exfil", re.compile(
        r"(?:send|post|upload|forward|transmit|exfiltrate|leak|copy)\s+"
        r"(?:me\s+|us\s+|the\s+|your\s+|all\s+|any\s+)*"
        r"(?:api[\s_-]?key|access[\s_-]?token|refresh[\s_-]?token|password"
        r"|passphrase|credential|secret|private[\s_-]?key|ssh[\s_-]?key"
        r"|\.env|environment\s+variable)s?",
        re.I,
    )),
]

#: Characters a derived tool name may contain. Everything else collapses to
#: ``_``, which is exactly why collision detection below is mandatory.
_NAME_ILLEGAL_RE = re.compile(r"[^a-zA-Z0-9_]")


def scan_text(text: str) -> list[str]:
    """Return the names of every injection pattern matching *text*."""
    if not text:
        return []
    return [name for name, pattern in _INJECTION_PATTERNS if pattern.search(text)]


def scan_identifier(name: str) -> list[str]:
    """Scan a tool name for injection payloads.

    Identifiers cannot contain spaces, so a payload smuggled into a name arrives
    as ``ignore_all_previous_instructions`` or ``ignore-all-previous-…`` or
    camelCase. The patterns all expect whitespace between words, so the name is
    normalised into prose first — scanning it raw finds nothing, which is why a
    name-borne payload used to sail through.
    """
    if not name:
        return []
    spaced = re.sub(r"[_\-.]+", " ", name)
    # camelCase / PascalCase → separate words.
    spaced = re.sub(r"(?<=[a-z0-9])(?=[A-Z])", " ", spaced)
    return scan_text(spaced)


# Kept as the module's original entry point: callers (and tests) that only have
# a description string still work, and it now shares the pattern set with the
# deep scan below.
def scan_tool_description(description: str) -> list[str]:
    return scan_text(description)


#: Schema keys whose *values* are prose the model reads directly.
_SCHEMA_TEXT_KEYS = ("description", "title", "$comment", "deprecationMessage")

#: Schema keys whose values are data the provider still renders into the function
#: definition — an ``enum`` of allowed strings is as model-visible as a
#: description, and used to be the cheapest bypass in this module: the walker
#: recursed into lists but only collected strings found under a text *key*, so
#: string *elements* were dropped on the floor. ``{"enum": ["ignore all previous
#: instructions"]}`` scanned clean.
_SCHEMA_VALUE_KEYS = ("enum", "const", "default", "examples", "pattern", "format")

#: Budgets for walking an untrusted schema. A hostile server can send a schema
#: that is deep, wide, or enormous; scanning must degrade by stopping rather than
#: by burning unbounded CPU inside the connect path.
_SCHEMA_MAX_DEPTH = 12
_SCHEMA_MAX_NODES = 5000
_SCHEMA_MAX_BYTES = 512 * 1024


@dataclass
class _WalkBudget:
    nodes: int = 0
    chars: int = 0

    def exhausted(self) -> bool:
        return self.nodes > _SCHEMA_MAX_NODES or self.chars > _SCHEMA_MAX_BYTES


def _walk_schema_text(
    schema: Any, depth: int = 0, budget: _WalkBudget | None = None,
) -> list[str]:
    """Yield every model-visible string inside a JSON schema.

    Both prose keys (``description``, ``title``) and value keys (``enum``,
    ``const``, ``default``, ``examples``…) reach the model verbatim in the
    function definition, so all of them are attack surface. Bounded on three
    axes — depth, node count and total characters — because the schema is
    untrusted input that may nest arbitrarily or simply be huge.
    """
    if budget is None:
        budget = _WalkBudget()
    if depth > _SCHEMA_MAX_DEPTH or budget.exhausted():
        return []

    found: list[str] = []

    def take(value: Any) -> None:
        """Collect *value* if it is a string, recursing through containers."""
        if isinstance(value, str):
            if value:
                budget.chars += len(value)
                found.append(value)
            return
        if isinstance(value, (dict, list)):
            found.extend(_walk_schema_text(value, depth + 1, budget))

    if isinstance(schema, list):
        for entry in schema:
            budget.nodes += 1
            if budget.exhausted():
                break
            # Strings inside a list are collected, not skipped — this is the
            # enum bypass.
            take(entry)
        return found

    if not isinstance(schema, dict):
        return found

    for key, value in schema.items():
        budget.nodes += 1
        if budget.exhausted():
            break
        if key in _SCHEMA_TEXT_KEYS or key in _SCHEMA_VALUE_KEYS:
            take(value)
        elif isinstance(value, (dict, list)):
            found.extend(_walk_schema_text(value, depth + 1, budget))
        elif isinstance(value, str):
            # An unrecognised key with a string value: still rendered, still
            # scanned. Cheaper to over-scan than to maintain an allowlist of
            # every extension key a server may invent.
            take(value)
    return found


def scan_mcp_tool(tool: dict[str, Any]) -> dict[str, list[str]]:
    """Scan every model-visible field of an MCP tool declaration.

    Returns ``{field_label: [pattern, ...]}`` for the fields that matched, so a
    warning can say *where* the payload was rather than only that there was one.
    """
    findings: dict[str, list[str]] = {}

    name_hits = scan_identifier(str(tool.get("name", "")))
    if name_hits:
        findings["name"] = name_hits

    desc_hits = scan_text(str(tool.get("description", "")))
    if desc_hits:
        findings["description"] = desc_hits

    schema_hits: list[str] = []
    for text in _walk_schema_text(tool.get("inputSchema")):
        schema_hits.extend(scan_text(text))
    if schema_hits:
        findings["inputSchema"] = sorted(set(schema_hits))

    return findings


def derive_tool_name(server: str, tool: str) -> str:
    """Derive the registered name for *tool* on *server*.

    Non-alphanumerics collapse to ``_``, so ``a-b``, ``a_b`` and ``a.b`` all
    produce the same string. That collapse is unavoidable (the name has to
    satisfy provider function-name rules) which is precisely why the caller
    must run collision detection over a whole batch — see
    :func:`validate_mcp_tools`.

    Over-long names are truncated with a short digest of the *original* name
    appended, so two long names sharing a prefix stay distinguishable instead
    of both truncating to the same string.
    """
    raw = _NAME_ILLEGAL_RE.sub("_", f"mcp_{server}_{tool}")
    if len(raw) <= MAX_TOOL_NAME_LENGTH:
        return raw
    digest = hashlib.sha256(f"{server}/{tool}".encode()).hexdigest()[:8]
    return f"{raw[: MAX_TOOL_NAME_LENGTH - 9]}_{digest}"


def check_tool_collision(tool_name: str, builtin_names: set[str]) -> bool:
    if tool_name in builtin_names:
        logger.warning("MCP tool '{}' collides with built-in tool — skipping", tool_name)
        return True
    return False


@dataclass(frozen=True)
class AcceptedTool:
    """An MCP tool declaration that passed validation, with its final name.

    Carrying ``registered_name`` and ``mcp_name`` together is what keeps the
    two apart at every later step: the model and the registry see the derived
    name, while ``tools/call`` must always carry the server's original name.
    Deriving the name twice in two places is how they drift.
    """

    registered_name: str
    mcp_name: str
    declaration: dict[str, Any]


def validate_input_schema(schema: Any, depth: int = 0) -> str:
    """Return "" when *schema* is usable as a tool parameter schema, else why not.

    This runs at registration time on purpose. ``Tool.to_schema()`` raises
    ``ValueError`` for a malformed schema (an ``array`` without ``items``, say),
    and ``get_definitions()`` catches that and skips the tool — so an unchecked
    bad schema produced a tool that registered successfully, reported as ready,
    and was invisible to the model forever, leaving one error line per turn as
    the only evidence. Rejecting it here makes the failure loud and one-time.

    Mirrors the checks in ``tools/base.py:_validate_json_schema`` and adds the
    top-level shape requirement that MCP itself specifies (``type: "object"``).
    """
    if depth == 0:
        if schema is None or schema == {}:
            return ""  # absent schema is legal; the adapter substitutes an empty object
        if not isinstance(schema, dict):
            return "inputSchema must be an object"
        declared = schema.get("type")
        if declared is not None and declared != "object":
            return f"top-level type must be 'object', got {declared!r}"
        properties = schema.get("properties")
        if properties is not None and not isinstance(properties, dict):
            return "properties must be an object"

    if depth > 8:
        return "schema nests too deeply"
    if not isinstance(schema, dict):
        return ""

    schema_type = schema.get("type")
    if schema_type == "array" and "items" not in schema:
        return "array schema is missing 'items'"

    for key in ("items", "additionalProperties"):
        nested = schema.get(key)
        if isinstance(nested, dict):
            if error := validate_input_schema(nested, depth + 1):
                return f"{key}: {error}"
        elif isinstance(nested, list):
            for entry in nested:
                if error := validate_input_schema(entry, depth + 1):
                    return f"{key}: {error}"

    properties = schema.get("properties")
    if isinstance(properties, dict):
        for prop_name, prop_schema in properties.items():
            if error := validate_input_schema(prop_schema, depth + 1):
                return f"properties.{prop_name}: {error}"

    for key in ("anyOf", "oneOf", "allOf"):
        variants = schema.get(key)
        if isinstance(variants, list):
            for index, entry in enumerate(variants):
                if error := validate_input_schema(entry, depth + 1):
                    return f"{key}[{index}]: {error}"

    return ""


def validate_mcp_tools(
    server_name: str,
    tools: list[dict[str, Any]],
    builtin_names: set[str],
    include_filter: list[str] | None = None,
    exclude_filter: list[str] | None = None,
    policy: str = "block",
) -> list[AcceptedTool]:
    """Filter, scan and name a server's tool declarations.

    Rejects, in order: nameless tools, tools excluded by config, tools whose
    derived name collides with a built-in **or with another tool in this same
    batch**, tools with an unusable ``inputSchema``, and (under ``block``) tools
    carrying injection payloads.

    The intra-batch collision check is the one that was missing. ``builtin_names``
    only grew *after* the whole batch was validated, so a server exposing both
    ``a-b`` and ``a_b`` had both accepted, and the second silently overwrote the
    first in the registry — the model would see one name and reach the other
    tool. On a surface where the tool set is supplied by a third party, that is
    a substitution primitive, not a cosmetic bug. Rejecting both members of a
    collision (rather than keeping the first) is deliberate: which declaration
    arrives first is the server's choice, so "first wins" would still let a
    hostile server decide which tool a name resolves to.
    """
    accepted: list[AcceptedTool] = []
    claimed: dict[str, str] = {}

    for tool in tools:
        if not isinstance(tool, dict):
            logger.warning("MCP server '{}' exposed a non-object tool entry — skipping", server_name)
            continue

        name = tool.get("name", "")
        if not isinstance(name, str) or not name:
            logger.warning("MCP server '{}' exposed a tool without a name — skipping", server_name)
            continue

        if include_filter and name not in include_filter:
            continue
        if exclude_filter and name in exclude_filter:
            continue

        registered = derive_tool_name(server_name, name)

        if check_tool_collision(registered, builtin_names):
            continue

        if registered in claimed:
            # Drop the earlier one too — see the docstring.
            previous = claimed[registered]
            logger.error(
                "MCP server '{}' exposes '{}' and '{}', which both derive the tool name "
                "'{}'. Rejecting both: a name that resolves to an ambiguous tool must "
                "never reach the model. Rename them on the server, or use tools_exclude.",
                server_name, previous, name, registered,
            )
            accepted = [a for a in accepted if a.registered_name != registered]
            continue
        claimed[registered] = name

        schema_error = validate_input_schema(tool.get("inputSchema"))
        if schema_error:
            logger.error(
                "MCP tool '{}/{}' has an unusable inputSchema ({}) — skipping. Accepting it "
                "would register a tool that every get_definitions() call then drops.",
                server_name, name, schema_error,
            )
            continue

        findings = scan_mcp_tool(tool)
        if findings:
            rendered = "; ".join(f"{field}: {', '.join(pats)}" for field, pats in findings.items())
            logger.warning(
                "MCP tool '{}/{}' carries suspicious patterns [{}] — {}",
                server_name, name, rendered,
                "rejecting (tools.mcpSecurityPolicy=block)" if policy == "block"
                else "allowing (tools.mcpSecurityPolicy=warn)",
            )
            if policy == "block":
                continue

        accepted.append(AcceptedTool(registered_name=registered, mcp_name=name, declaration=tool))

    return accepted
