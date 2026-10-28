"""Characterization tests for GatewayAuth failure paths.

补测覆盖缺口（优先级4，gateway/auth.py）：
- authenticate_token 失败路径：token 不匹配 / 为空时返回 False
- verify_pairing 失败路径：code 未知 / 已过期 / 平台不符时返回 False

性质：表征测试，不改源码；以实际行为为准。
"""

from __future__ import annotations

import time

from echo_agent.config.schema import GatewayAuthConfig
from echo_agent.gateway.auth import GatewayAuth


def _make_auth(tmp_path, **overrides) -> GatewayAuth:
    config = GatewayAuthConfig(**overrides)
    return GatewayAuth(config, tmp_path)


# ── authenticate_token：失败路径 ──────────────────────────────────────────────


def test_authenticate_token_no_tokens_configured_passes(tmp_path) -> None:
    # 未配置 api_tokens → 无鉴权部署，一律放行。
    auth = _make_auth(tmp_path)
    assert auth.authenticate_token("anything") is True


def test_authenticate_token_empty_token_fails(tmp_path) -> None:
    # 配置了 token 但请求未携带 token → 拒绝。
    auth = _make_auth(tmp_path, api_tokens=["secret-token"])
    assert auth.authenticate_token("") is False


def test_authenticate_token_mismatch_fails(tmp_path) -> None:
    # token 不匹配 → 拒绝。
    auth = _make_auth(tmp_path, api_tokens=["secret-token"])
    assert auth.authenticate_token("wrong-token") is False


def test_authenticate_token_match_passes(tmp_path) -> None:
    auth = _make_auth(tmp_path, api_tokens=["secret-token"])
    assert auth.authenticate_token("secret-token") is True


# ── verify_pairing：失败路径 ──────────────────────────────────────────────────


def test_verify_pairing_unknown_code_fails(tmp_path) -> None:
    # 未知 code（没有对应 pending 记录）→ 拒绝。
    auth = _make_auth(tmp_path, mode="pairing")
    assert auth.verify_pairing("telegram", "u1", "DEADBEEF99") is False


def test_verify_pairing_expired_code_fails(tmp_path) -> None:
    # code 已过期（created_at 超出 pairing_ttl）→ 拒绝。
    auth = _make_auth(tmp_path, mode="pairing", pairing_ttl_seconds=300)
    code = "ABCDE12345"
    auth._pending_codes[code] = {
        "platform": "telegram",
        "created_at": time.time() - 600,  # 600s 前生成，已超过 300s TTL
    }
    assert auth.verify_pairing("telegram", "u1", code) is False
    # 过期 code 应被清理。
    assert code not in auth._pending_codes


def test_verify_pairing_platform_mismatch_fails(tmp_path) -> None:
    # code 有效但平台不符 → 拒绝。
    auth = _make_auth(tmp_path, mode="pairing", pairing_ttl_seconds=300)
    code = auth.generate_pairing_code("telegram")
    assert auth.verify_pairing("discord", "u1", code) is False


def test_verify_pairing_success_path(tmp_path) -> None:
    # 平台匹配且未过期 → 成功，并将用户写入 approved。
    auth = _make_auth(tmp_path, mode="pairing", pairing_ttl_seconds=300)
    code = auth.generate_pairing_code("telegram")
    assert auth.verify_pairing("telegram", "u1", code) is True
    assert auth.is_authorized("telegram", "u1") is True
