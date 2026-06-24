"""表征测试 — security/smart_approval.py smart_approve()

覆盖目标：行 75-88 判定矩阵五条出口
  - first_word == "APPROVE" → "approve"
  - first_word == "DENY"    → "deny"
  - first_word == "ESCALATE" → "escalate"
  - 无法识别响应（如 "MAYBE"）→ "escalate"（兜底升级）
  - provider 抛异常          → "unavailable"
  - 空响应（raw_text 为空）  → "unavailable"
"""

from __future__ import annotations

import pytest
from unittest.mock import AsyncMock, MagicMock

from echo_agent.security.smart_approval import smart_approve


def _make_provider(content: str) -> MagicMock:
    """创建一个 mock LLMProvider，chat_with_retry 返回给定 content。"""
    provider = MagicMock()
    response = MagicMock()
    response.content = content
    provider.chat_with_retry = AsyncMock(return_value=response)
    return provider


@pytest.mark.asyncio
class TestSmartApproveDecisionMatrix:
    async def test_approve_branch(self):
        provider = _make_provider("APPROVE\nThis is safe.")
        result = await smart_approve("exec", "ls -la", "shell execution", provider)
        assert result == "approve"

    async def test_approve_case_insensitive_upper(self):
        # APPROVE 全大写、后跟说明
        provider = _make_provider("APPROVE - listing files is safe")
        result = await smart_approve("exec", "ls", "shell", provider)
        assert result == "approve"

    async def test_deny_branch(self):
        provider = _make_provider("DENY\nDangerous recursive delete.")
        result = await smart_approve("exec", "rm -rf /", "shell execution", provider)
        assert result == "deny"

    async def test_escalate_branch(self):
        provider = _make_provider("ESCALATE\nNot sure about this one.")
        result = await smart_approve("exec", "curl https://example.com", "network", provider)
        assert result == "escalate"

    async def test_unrecognized_response_falls_back_to_escalate(self):
        # "MAYBE" 不是有效词 → 兜底 escalate
        provider = _make_provider("MAYBE this is fine")
        result = await smart_approve("exec", "echo hi", "test", provider)
        assert result == "escalate"

    async def test_empty_response_returns_unavailable(self):
        provider = _make_provider("")
        result = await smart_approve("exec", "echo hi", "test", provider)
        assert result == "unavailable"

    async def test_whitespace_only_response_returns_unavailable(self):
        provider = _make_provider("   ")
        result = await smart_approve("exec", "echo hi", "test", provider)
        assert result == "unavailable"

    async def test_exception_returns_unavailable(self):
        provider = MagicMock()
        provider.chat_with_retry = AsyncMock(side_effect=RuntimeError("connection error"))
        result = await smart_approve("exec", "echo hi", "test", provider)
        assert result == "unavailable"

    async def test_exception_with_timeout_error_returns_unavailable(self):
        provider = MagicMock()
        provider.chat_with_retry = AsyncMock(side_effect=TimeoutError("timeout"))
        result = await smart_approve("exec", "cmd", "desc", provider)
        assert result == "unavailable"


@pytest.mark.asyncio
class TestSmartApproveRouterWiring:
    async def test_no_router_uses_passed_provider(self):
        """router=None 时直接用传入的 provider。"""
        provider = _make_provider("DENY\n")
        result = await smart_approve("exec", "rm -rf /", "danger", provider, router=None)
        assert result == "deny"
        provider.chat_with_retry.assert_called_once()

    async def test_router_without_resolve_attr_ignored(self):
        """router 对象没有 resolve 属性时直接用原 provider。"""
        provider = _make_provider("APPROVE\n")
        router = object()  # 无 resolve 方法
        result = await smart_approve("exec", "ls", "list", provider, router=router)
        assert result == "approve"
        provider.chat_with_retry.assert_called_once()

    async def test_router_resolve_returns_none_provider_uses_original(self):
        """router.resolve 返回 (None, model)，降级回原 provider。"""
        provider = _make_provider("ESCALATE\n")
        router = MagicMock()
        router.resolve.return_value = (None, "")
        result = await smart_approve("exec", "curl x", "net", provider, router=router)
        assert result == "escalate"
        provider.chat_with_retry.assert_called_once()

    async def test_router_resolve_replaces_provider(self):
        """router.resolve 返回新 provider 时，使用新 provider。"""
        original_provider = MagicMock()
        original_provider.chat_with_retry = AsyncMock()  # 不应被调用

        new_provider = _make_provider("DENY\n")
        router = MagicMock()
        router.resolve.return_value = (new_provider, "fast-model")

        result = await smart_approve("exec", "rm /etc/hosts", "danger", original_provider, router=router)
        assert result == "deny"
        original_provider.chat_with_retry.assert_not_called()
        new_provider.chat_with_retry.assert_called_once()
