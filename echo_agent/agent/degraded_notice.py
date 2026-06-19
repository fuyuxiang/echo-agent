"""User-facing degraded-mode notices (Chinese copy) and helpers.

Single source of truth for the messages the agent sends when a turn cannot
produce a normal answer (approval blocked/timed out, repeated tool failures,
provider outage). Keeping the copy here avoids scattering user-facing strings
across the pipeline.
"""

from __future__ import annotations

REASON_APPROVAL_UNAVAILABLE = "approval_unavailable"
REASON_APPROVAL_TIMEOUT = "approval_timeout"
REASON_REPEAT_BLOCKED = "repeat_blocked"

GENERIC_FALLBACK_TEXT = (
    "⚠️ 处理你的请求时遇到问题,已中止。可以稍后重试或换个说法。"
)


def notice_for(reason: str, *, tool: str = "", request_id: str = "") -> str:
    """Return the Chinese user-facing notice for a degraded-mode reason."""
    if reason == REASON_APPROVAL_UNAVAILABLE:
        return (
            "⚠️ 这步需要执行命令,但安全审批暂时不可用(模型服务异常),已暂停。"
            "请稍后回复让我重试。"
        )
    if reason == REASON_APPROVAL_TIMEOUT:
        tool_label = tool or "该操作"
        rid = request_id or "<id>"
        return (
            f"⚠️ 这步需要你确认执行 `{tool_label}`,等待超时已暂停。"
            f"回复 `/approve {rid}` 继续,或 `/deny {rid}` 取消。"
        )
    if reason == REASON_REPEAT_BLOCKED:
        return (
            "⚠️ 我多次尝试同一操作未成功,已停止。可能是工具或服务异常,请稍后重试。"
        )
    return GENERIC_FALLBACK_TEXT


def combine_notices(notices: list[str]) -> str:
    """Dedupe (preserving first-seen order) and join notices with newlines."""
    seen: set[str] = set()
    ordered: list[str] = []
    for n in notices:
        if n and n not in seen:
            seen.add(n)
            ordered.append(n)
    return "\n".join(ordered)
