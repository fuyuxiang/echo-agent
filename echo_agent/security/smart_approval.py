"""Smart approval — LLM pre-screening for flagged commands."""

from __future__ import annotations

from typing import TYPE_CHECKING, Literal

from loguru import logger

if TYPE_CHECKING:
    from echo_agent.models.provider import LLMProvider


_PROMPT_TEMPLATE = (
    "You are a security reviewer for an AI agent. A tool call was flagged as potentially dangerous.\n\n"
    "Tool: {tool_name}\n"
    "Command/Arguments: {command}\n"
    "Flagged reason: {description}\n\n"
    "Assess the ACTUAL risk. Many flagged commands are false positives — for example, "
    "`python -c \"print('hello')\"` is flagged as 'script execution' but is completely harmless.\n\n"
    "Rules:\n"
    "- APPROVE if clearly safe (benign scripts, safe file ops, dev tools, git, package installs, listing)\n"
    "- DENY if genuinely dangerous (recursive delete of important paths, system file overwrites, "
    "fork bombs, dropping databases, wiping disks)\n"
    "- ESCALATE if uncertain\n\n"
    "Respond with exactly one word: APPROVE, DENY, or ESCALATE"
)


async def smart_approve(
    tool_name: str,
    command: str,
    description: str,
    provider: "LLMProvider",
    model: str = "",
) -> Literal["approve", "deny", "escalate"]:
    """Use LLM to pre-screen a flagged tool call. Returns approve/deny/escalate."""
    prompt = _PROMPT_TEMPLATE.format(
        tool_name=tool_name,
        command=command[:1000],
        description=description,
    )
    try:
        response = await provider.chat_with_retry(
            messages=[{"role": "user", "content": prompt}],
            model=model or None,
            max_tokens=16,
            temperature=0.0,
        )
        text = (response.content or "").strip().upper()
        if "APPROVE" in text:
            logger.info("Smart approval: APPROVE for '{}' — {}", tool_name, command[:100])
            return "approve"
        if "DENY" in text:
            logger.warning("Smart approval: DENY for '{}' — {}", tool_name, command[:100])
            return "deny"
        logger.info("Smart approval: ESCALATE for '{}' — response: {}", tool_name, text[:50])
        return "escalate"
    except Exception as e:
        logger.warning("Smart approval failed (escalating): {}", e)
        return "escalate"
