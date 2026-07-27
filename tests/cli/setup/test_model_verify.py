"""Tests for dynamic model listing + live verification."""
from __future__ import annotations

from unittest.mock import MagicMock, patch

import httpx

from echo_agent.cli.setup import model_verify as mv
from echo_agent.cli.setup import providers as p

_T = "echo_agent.cli.setup.model_verify"


def _resp(json_body):
    r = MagicMock()
    r.status_code = 200
    r.json.return_value = json_body
    r.raise_for_status.return_value = None
    return r


def test_list_models_openai_shape():
    body = {"data": [{"id": "gpt-4o"}, {"id": "o1"}]}
    with patch(f"{_T}.httpx.get", return_value=_resp(body)):
        got = mv.list_models(p.find("openai"), "sk-x")
    assert got == ["gpt-4o", "o1"]


def test_list_models_no_endpoint_returns_empty():
    assert mv.list_models(p.find("bedrock"), "") == []


def test_list_models_network_error_returns_empty():
    with patch(f"{_T}.httpx.get", side_effect=httpx.ConnectError("boom")):
        assert mv.list_models(p.find("openai"), "sk-x") == []


def test_list_models_timeout_returns_empty():
    with patch(f"{_T}.httpx.get", side_effect=httpx.TimeoutException("slow")):
        assert mv.list_models(p.find("deepseek"), "sk-x") == []


def test_verify_model_ok():
    fake_provider = MagicMock()

    async def _chat(*a, **k):
        resp = MagicMock()
        resp.finish_reason = "stop"
        return resp

    fake_provider.chat_with_retry = _chat
    with patch(f"{_T}.create_provider", return_value=fake_provider):
        res = mv.verify_model("openai", "sk-x", "", "gpt-4o")
    assert res.status == "ok"


def test_verify_model_auth_error():
    with patch(f"{_T}.create_provider", side_effect=Exception("401 Unauthorized")):
        res = mv.verify_model("openai", "bad", "", "gpt-4o")
    assert res.status == "error"
    assert "401" in res.detail


def test_verify_model_unreachable():
    with patch(f"{_T}.create_provider", side_effect=httpx.ConnectError("no route")):
        res = mv.verify_model("openai", "sk-x", "", "gpt-4o")
    assert res.status == "unreachable"


def _fake_provider_returning(content, finish_reason):
    fake_provider = MagicMock()

    async def _chat(*a, **k):
        resp = MagicMock()
        resp.finish_reason = finish_reason
        resp.content = content
        return resp

    fake_provider.chat_with_retry = _chat
    return fake_provider


def test_verify_model_error_response_preserves_detail():
    # chat_with_retry never raises; it returns an error LLMResponse.
    fake_provider = _fake_provider_returning("Error: 401 Unauthorized", "error")
    with patch(f"{_T}.create_provider", return_value=fake_provider):
        res = mv.verify_model("openai", "bad", "", "gpt-4o")
    assert res.status == "error"
    assert "401" in res.detail


def test_verify_model_timeout_response_is_unreachable():
    fake_provider = _fake_provider_returning("Error: request timed out after 8s", "error")
    with patch(f"{_T}.create_provider", return_value=fake_provider):
        res = mv.verify_model("openai", "sk-x", "", "gpt-4o")
    assert res.status == "unreachable"
    assert "timed out" in res.detail


def test_verify_model_stop_response_is_ok():
    fake_provider = _fake_provider_returning("pong", "stop")
    with patch(f"{_T}.create_provider", return_value=fake_provider):
        res = mv.verify_model("openai", "sk-x", "", "gpt-4o")
    assert res.status == "ok"


class _ClosingProvider:
    """Provider double that records whether its client was closed in-loop.

    The real failure mode: verify_model runs the probe under asyncio.run, so an
    SDK client left open outlives the loop that owns its sockets, and the SDK's
    __del__ later schedules aclose() on the wizard's prompt_toolkit loop —
    "Event loop is closed". Closing must therefore happen before asyncio.run
    returns, which is what ``closed_in_loop`` pins down.
    """

    def __init__(self, *, fail: Exception | None = None):
        self._fail = fail
        self.closed = False
        self.closed_in_loop = False

    async def chat_with_retry(self, *a, **k):
        if self._fail:
            raise self._fail
        resp = MagicMock()
        resp.finish_reason = "stop"
        return resp

    async def aclose(self) -> None:
        import asyncio
        self.closed = True
        self.closed_in_loop = asyncio.get_running_loop().is_running()


def test_verify_model_closes_provider_on_success():
    prov = _ClosingProvider()
    with patch(f"{_T}.create_provider", return_value=prov):
        res = mv.verify_model("openai", "sk-x", "", "gpt-4o")
    assert res.status == "ok"
    assert prov.closed and prov.closed_in_loop


def test_verify_model_closes_provider_on_failure():
    prov = _ClosingProvider(fail=httpx.ConnectError("boom"))
    with patch(f"{_T}.create_provider", return_value=prov):
        res = mv.verify_model("openai", "sk-x", "", "gpt-4o")
    assert res.status == "unreachable"
    assert prov.closed and prov.closed_in_loop


def test_verify_model_close_failure_does_not_change_verdict():
    prov = _ClosingProvider()

    async def _boom() -> None:
        raise RuntimeError("Event loop is closed")

    prov.aclose = _boom
    with patch(f"{_T}.create_provider", return_value=prov):
        res = mv.verify_model("openai", "sk-x", "", "gpt-4o")
    assert res.status == "ok"


def test_classify_error_detail():
    assert mv._classify_error_detail("Error: 401 Unauthorized") == "error"
    assert mv._classify_error_detail("Error: request timed out after 8s") == "unreachable"
    assert mv._classify_error_detail("Connection refused") == "unreachable"
    assert mv._classify_error_detail("getaddrinfo failed") == "unreachable"
    assert mv._classify_error_detail("") == "error"
