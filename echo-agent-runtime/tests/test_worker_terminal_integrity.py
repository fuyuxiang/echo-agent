"""Workers must not claim completion for truncated, filtered, or error replies."""

from unittest.mock import AsyncMock

import pytest

from echo_agent.agent.multi_agent.runtime import WorkerExecutor
from echo_agent.agent.tools.delegate import SpawnTool
from echo_agent.models.provider import LLMResponse, ToolCallRequest


@pytest.mark.asyncio
@pytest.mark.parametrize("reason", ["length", "content_filter"])
async def test_worker_does_not_complete_or_execute_truncated_tool_calls(reason):
    provider = AsyncMock()
    provider.chat_with_retry.return_value = LLMResponse(
        content="partial output", finish_reason=reason,
        tool_calls=[ToolCallRequest(id="partial", name="write_file", arguments={"path": "partial"})],
    )
    execute = AsyncMock()
    result = await WorkerExecutor(provider=provider).run(
        task_index=0, goal="work", tool_defs=[], tool_executor=execute,
    )
    assert result.status == "failed"
    assert result.output == "partial output"
    assert reason in result.error
    execute.assert_not_awaited()


@pytest.mark.asyncio
async def test_background_without_tools_reports_model_error():
    provider = AsyncMock()
    provider.chat_with_retry.return_value = LLMResponse(content="provider unavailable", finish_reason="error")
    result = await SpawnTool(provider)._execute_worker("bg_test", "task", "", None)
    assert result.startswith("[failed]")
    assert "provider unavailable" in result


@pytest.mark.asyncio
async def test_worker_preserves_provider_native_tool_roundtrip_content():
    provider = AsyncMock()
    native = {"gemini": {"parts": [{"thoughtSignature": "signature", "functionCall": {"name": "read_file"}}]}}
    provider.chat_with_retry.side_effect = [
        LLMResponse(content="", finish_reason="tool_calls", tool_calls=[ToolCallRequest("one", "read_file", {})],
                    provider_content=native),
        LLMResponse(content="done"),
    ]
    result = await WorkerExecutor(provider=provider).run(
        task_index=0, goal="read", tool_defs=[], tool_executor=AsyncMock(return_value="file"),
    )
    assert result.status == "completed"
    messages = provider.chat_with_retry.call_args.kwargs["messages"]
    assistant = next(message for message in messages if message.get("tool_calls"))
    assert assistant["provider_content"] == native
