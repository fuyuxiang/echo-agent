"""User-facing error messages — never expose technical details to messaging channels."""

from __future__ import annotations

from echo_agent.agent.multi_agent.error_types import ToolErrorType


_USER_MESSAGES: dict[ToolErrorType, str] = {
    ToolErrorType.CONFIG: "抱歉，该功能所需的服务暂未配置。我将尝试用已有知识为您回答。",
    ToolErrorType.AUTH: "抱歉，相关服务的认证信息已过期，请联系管理员。我将尝试用已有知识为您回答。",
    ToolErrorType.RATE_LIMIT: "当前请求较多，相关服务暂时受限。请稍后再试，或者我可以用已有知识为您回答。",
    ToolErrorType.TRANSIENT: "处理过程中遇到临时网络问题。我将尝试用已有知识为您回答。",
    ToolErrorType.UNKNOWN: "处理您的请求时遇到了问题。我将尝试用已有知识为您回答。",
}


def get_user_friendly_message(error_type: str | ToolErrorType) -> str:
    """Map an error type to a user-friendly Chinese message."""
    if isinstance(error_type, str):
        try:
            error_type = ToolErrorType(error_type)
        except ValueError:
            error_type = ToolErrorType.UNKNOWN
    return _USER_MESSAGES.get(error_type, _USER_MESSAGES[ToolErrorType.UNKNOWN])


def sanitize_error_for_user(error: str, output: str) -> str:
    """Produce a clean user message from a failed agent result.

    If the agent produced usable partial output, return that.
    Otherwise return a generic friendly message.
    """
    if output and not output.startswith(("Agent ", "FAILED:", "Error:", "Agent '")):
        return output
    return "抱歉，我暂时无法完成这个任务。请稍后再试或换一种方式描述您的需求。"
