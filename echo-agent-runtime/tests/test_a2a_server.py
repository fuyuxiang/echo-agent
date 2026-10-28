"""Tests for A2AServer — agent card, JSON-RPC handling, task processing."""

from __future__ import annotations

import json
from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.a2a.models import AgentCard, A2ATask, A2AMessage, TaskState
from echo_agent.a2a.server import A2AServer


def _make_agent_card():
    return AgentCard(
        name="test-agent",
        description="A test agent",
        url="http://localhost:8080",
        version="1.0.0",
    )


def _make_processor(response_text="Hello from agent"):
    processor = AsyncMock()
    processor.process_direct = AsyncMock(return_value=response_text)
    return processor


def _make_request(body=None, *, raw_body=None):
    """Create a mock aiohttp.web.Request."""
    request = MagicMock()
    if raw_body is not None:
        # Simulate invalid JSON
        async def _json_raise():
            raise json.JSONDecodeError("Expecting value", "", 0)
        request.json = _json_raise
    else:
        async def _json():
            return body
        request.json = _json
    return request


class TestHandleAgentCard:
    """_handle_agent_card returns agent card JSON."""

    @pytest.mark.asyncio
    async def test_returns_agent_card(self):
        card = _make_agent_card()
        processor = _make_processor()
        server = A2AServer(agent_loop=processor, agent_card=card)

        request = MagicMock()
        response = await server._handle_agent_card(request)

        # aiohttp.web.json_response returns a Response object
        # But since we're calling the handler directly, it returns web.Response
        assert response.status == 200
        body = json.loads(response.body)
        assert body["name"] == "test-agent"
        assert body["description"] == "A test agent"
        assert body["version"] == "1.0.0"


class TestHandleRpcInvalidJson:
    """_handle_rpc with invalid JSON returns error code -32700."""

    @pytest.mark.asyncio
    async def test_invalid_json_parse_error(self):
        card = _make_agent_card()
        processor = _make_processor()
        server = A2AServer(agent_loop=processor, agent_card=card)

        request = _make_request(raw_body="not json")
        response = await server._handle_rpc(request)

        assert response.status == 400
        body = json.loads(response.body)
        assert body["error"]["code"] == -32700
        assert "parse" in body["error"]["message"].lower()


class TestProcessTaskNoUserMessage:
    """_process_task with no user message -> FAILED."""

    @pytest.mark.asyncio
    async def test_no_user_message_fails(self):
        card = _make_agent_card()
        processor = _make_processor()
        server = A2AServer(agent_loop=processor, agent_card=card)

        # Task with only agent messages, no user message
        task = A2ATask(
            id="task_001",
            state=TaskState.SUBMITTED,
            messages=[A2AMessage.text("agent", "I said something")],
        )

        result = await server._process_task(task)

        assert result.state == TaskState.FAILED
        assert any("no user message" in m.text_content.lower() for m in result.messages)
        processor.process_direct.assert_not_called()


class TestProcessTaskNormal:
    """_process_task with valid user message -> COMPLETED."""

    @pytest.mark.asyncio
    async def test_normal_processing_completed(self):
        card = _make_agent_card()
        processor = _make_processor(response_text="Here is your answer")
        server = A2AServer(agent_loop=processor, agent_card=card)

        task = A2ATask(
            id="task_002",
            state=TaskState.SUBMITTED,
            messages=[A2AMessage.text("user", "What is 2+2?")],
        )

        result = await server._process_task(task)

        assert result.state == TaskState.COMPLETED
        processor.process_direct.assert_called_once_with(
            "What is 2+2?", session_key="a2a:task_002", channel="a2a"
        )
        # Last message should contain the response
        last_msg = result.messages[-1]
        assert last_msg.role == "agent"
        assert "Here is your answer" in last_msg.text_content

    @pytest.mark.asyncio
    async def test_same_task_continuation_processes_latest_user_message(self):
        card = _make_agent_card()
        processor = _make_processor(response_text="answer")
        server = A2AServer(agent_loop=processor, agent_card=card)

        def send(request_id: int, text: str) -> dict:
            return {
                "jsonrpc": "2.0",
                "id": request_id,
                "method": "tasks/send",
                "params": {
                    "id": "continued-task",
                    "message": A2AMessage.text("user", text).to_dict(),
                },
            }

        first = await server._protocol.handle(send(1, "first question"))
        second = await server._protocol.handle(send(2, "follow-up question"))

        assert first["result"]["state"] == "completed"
        assert second["result"]["state"] == "completed"
        assert [call.args[0] for call in processor.process_direct.await_args_list] == [
            "first question", "follow-up question",
        ]

    @pytest.mark.asyncio
    async def test_processing_exception_fails(self):
        card = _make_agent_card()
        processor = AsyncMock()
        processor.process_direct = AsyncMock(side_effect=RuntimeError("LLM down"))
        server = A2AServer(agent_loop=processor, agent_card=card)

        task = A2ATask(
            id="task_003",
            state=TaskState.SUBMITTED,
            messages=[A2AMessage.text("user", "Hello")],
        )

        result = await server._process_task(task)

        assert result.state == TaskState.FAILED
        last_msg = result.messages[-1]
        assert "error" in last_msg.text_content.lower()


class TestProcessTaskAttachments:
    """Non-text parts must not be silently dropped."""

    @pytest.mark.asyncio
    async def test_text_with_attachment_flags_dropped_parts(self):
        card = _make_agent_card()
        processor = _make_processor(response_text="answer")
        server = A2AServer(agent_loop=processor, agent_card=card)

        msg = A2AMessage(role="user", parts=[
            {"type": "text", "text": "describe this"},
            {"type": "file", "file": {"name": "a.png"}},
        ])
        task = A2ATask(id="t_att", state=TaskState.SUBMITTED, messages=[msg])

        result = await server._process_task(task)

        assert result.state == TaskState.COMPLETED
        assert "ignored parts" in result.messages[-1].text_content
        assert "file" in result.messages[-1].text_content

    @pytest.mark.asyncio
    async def test_only_attachment_no_text_fails_with_notice(self):
        card = _make_agent_card()
        processor = _make_processor()
        server = A2AServer(agent_loop=processor, agent_card=card)

        msg = A2AMessage(role="user", parts=[{"type": "file", "file": {"name": "a.png"}}])
        task = A2ATask(id="t_att2", state=TaskState.SUBMITTED, messages=[msg])

        result = await server._process_task(task)

        assert result.state == TaskState.FAILED
        assert "file" in result.messages[-1].text_content
        processor.process_direct.assert_not_called()
