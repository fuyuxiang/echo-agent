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
