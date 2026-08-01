"""P2: gateway admin-token scope and CSRF/Origin protection."""

from echo_agent.config.schema import GatewayAuthConfig
from echo_agent.gateway.auth import GatewayAuth


def _auth(tmp_path, **kw):
    return GatewayAuth(GatewayAuthConfig(**kw), data_dir=tmp_path)


# ── admin token scope ──────────────────────────────────────────────────────

def test_admin_token_falls_back_to_api_tokens(tmp_path):
    auth = _auth(tmp_path, api_tokens=["chat-tok"])
    # No admin_tokens configured → api_tokens authorize admin actions.
    assert auth.authenticate_admin_token("chat-tok") is True
    assert auth.authenticate_admin_token("wrong") is False


def test_admin_token_separates_scope(tmp_path):
    auth = _auth(tmp_path, api_tokens=["chat-tok"], admin_tokens=["admin-tok"])
    # chat token must NOT pass the admin gate once admin_tokens is set.
    assert auth.authenticate_admin_token("chat-tok") is False
    assert auth.authenticate_admin_token("admin-tok") is True
    # chat token still passes the normal gate.
    assert auth.authenticate_token("chat-tok") is True


def test_admin_scope_implies_read_scope(tmp_path):
    """An admin token must also satisfy the read guard.

    The two lists used to be disjoint sets rather than a hierarchy, so a
    deployment with a separate admin_tokens had NO usable dashboard token: the
    api token logged in but every admin control 403'd, while the admin token was
    rejected by the read guard the login probe itself goes through (401)."""
    auth = _auth(tmp_path, api_tokens=["chat-tok"], admin_tokens=["admin-tok"])
    assert auth.authenticate_token("admin-tok") is True
    # The implication is one-way and nothing else is widened.
    assert auth.authenticate_admin_token("chat-tok") is False
    assert auth.authenticate_token("wrong") is False


def test_read_gate_closed_when_only_admin_tokens_configured(tmp_path):
    """admin_tokens alone must still authenticate reads.

    authenticate_token short-circuited on an empty api_tokens list, so a deploy
    that configured only admin_tokens served every read endpoint to anyone."""
    auth = _auth(tmp_path, admin_tokens=["admin-tok"])
    assert auth.authenticate_token("admin-tok") is True
    assert auth.authenticate_token("") is False
    assert auth.authenticate_token("wrong") is False


def test_admin_token_open_when_no_tokens(tmp_path):
    auth = _auth(tmp_path)
    assert auth.authenticate_admin_token("") is True


def test_is_admin_follows_the_admin_token_hierarchy(tmp_path):
    """is_admin 必须和 HTTP 侧的 admin 门用同一套口径。

    原实现门在 self._api_tokens 上、并且调用的是 *读* 级校验,于是两种口径都是错的:
    只配 admin_tokens 时 admin token 拿不到 admin;两个列表都配时只读的 api token
    反而被当成 admin。
    """
    only_admin = _auth(tmp_path, admin_tokens=["admin-tok"])
    assert only_admin.is_admin("cli", "u", token="admin-tok") is True
    assert only_admin.is_admin("cli", "u", token="wrong") is False

    both = _auth(tmp_path, api_tokens=["chat-tok"], admin_tokens=["admin-tok"])
    assert both.is_admin("cli", "u", token="admin-tok") is True
    assert both.is_admin("cli", "u", token="chat-tok") is False

    # 未配 admin_tokens 时沿用 api_tokens(与 authenticate_admin_token 一致)。
    api_only = _auth(tmp_path, api_tokens=["chat-tok"])
    assert api_only.is_admin("cli", "u", token="chat-tok") is True


def test_is_admin_does_not_grant_admin_to_any_string_without_tokens(tmp_path):
    """完全没配 token 的部署下,任意字符串不能变成 admin。

    authenticate_admin_token 在无 token 配置时对一切返回 True(未鉴权部署的语义),
    所以 is_admin 必须自己确认"确实配了某个列表"才走 token 分支。
    """
    auth = _auth(tmp_path, admin_users=["boss"])
    assert auth.is_admin("cli", "u", token="anything") is False
    assert auth.is_admin("cli", "boss") is True


# ── CSRF / Origin (opt-in via allowed_origins) ──────────────────────────────

def test_csrf_disabled_by_default_allows_cross_site(tmp_path):
    # is_origin_allowed is the opt-in primitive: with no allowed_origins it
    # permits everything (used only where a config-gated allowlist is desired).
    # NOTE: admin endpoints do NOT rely on this — they use the default-on
    # is_cross_site_browser via _check_csrf (see admin CSRF gate tests below).
    auth = _auth(tmp_path)
    assert auth.is_origin_allowed("http://evil.example", "cross-site") is True
    assert auth.is_origin_allowed("tauri://localhost", "cross-site") is True


def test_non_browser_request_allowed_when_enabled(tmp_path):
    auth = _auth(tmp_path, allowed_origins=["http://trusted.app"])
    # No Origin, no Sec-Fetch-Site → not a browser → allowed.
    assert auth.is_origin_allowed("", "") is True


def test_same_origin_allowed_when_enabled(tmp_path):
    auth = _auth(tmp_path, allowed_origins=["http://trusted.app"])
    assert auth.is_origin_allowed("http://127.0.0.1:58123", "same-origin") is True


def test_cross_site_rejected_when_enabled(tmp_path):
    auth = _auth(tmp_path, allowed_origins=["http://trusted.app"])
    assert auth.is_origin_allowed("http://evil.example", "cross-site") is False


def test_cross_site_allowed_when_allowlisted(tmp_path):
    auth = _auth(tmp_path, allowed_origins=["http://trusted.app"])
    assert auth.is_origin_allowed("http://trusted.app", "cross-site") is True
    assert auth.is_origin_allowed("http://evil.example", "cross-site") is False


# ── admin endpoint CSRF gate (default-on, NOT opt-in) ───────────────────────
#
# _check_csrf guards the highest-risk endpoints (shutdown / skills / knowledge).
# It must use the default-on is_cross_site_browser, so an unauthenticated
# loopback deployment (no tokens, empty allowlist — the default form) still
# rejects a malicious page's cross-site POST to /shutdown. Regression guard for
# the "one channel closed, the other left open" CSRF hole.

def _csrf_request(headers, *, peer=("127.0.0.1", 5555)):
    from unittest.mock import MagicMock
    from aiohttp.test_utils import make_mocked_request
    transport = MagicMock()
    transport.get_extra_info = lambda key, default=None: peer if key == "peername" else default
    return make_mocked_request("POST", "/api/v1/shutdown", headers=headers, transport=transport)


def test_admin_csrf_rejects_cross_site_under_empty_allowlist():
    import sys
    from pathlib import Path
    sys.path.insert(0, str(Path(__file__).parent))
    from test_gateway_server import _make_gateway
    gw, _ = _make_gateway()
    # Empty allowlist (default). A malicious cross-site browser POST must be
    # rejected even though allowed_origins is unset.
    resp = gw._check_csrf(
        _csrf_request({"Origin": "https://evil.example", "Sec-Fetch-Site": "cross-site"}),
        action="shutdown",
    )
    assert resp is not None and resp.status == 403


def test_admin_csrf_allows_same_origin_and_native():
    import sys
    from pathlib import Path
    sys.path.insert(0, str(Path(__file__).parent))
    from test_gateway_server import _make_gateway
    gw, _ = _make_gateway()
    # Same-origin playground fetch and native clients (no browser headers) pass.
    assert gw._check_csrf(
        _csrf_request({
            "Origin": "http://127.0.0.1:58123",
            "Sec-Fetch-Site": "same-origin",
            "Host": "127.0.0.1:58123",
        }),
        action="shutdown",
    ) is None
    assert gw._check_csrf(_csrf_request({}), action="shutdown") is None


# ── "配了哪种 token" 的口径必须处处一致 ─────────────────────────────────────
#
# admin token 隐含读权限之后,"是否配置了 token"就不能再只看 api_tokens。剩下两处
# 漏掉的判断方向相反、但根因相同:绑定安全检查会把"只配 admin_tokens"误判成未鉴权
# 而拒绝启动;WS 握手则会把它误判成未配置 token 而完全跳过校验。

def _gateway_with_tokens(**auth_kw):
    from echo_agent.config.schema import (
        GatewayAuthConfig,
        GatewayConfig,
        GatewaySessionPolicyConfig,
    )
    from echo_agent.gateway.server import GatewayServer
    from unittest.mock import AsyncMock, MagicMock

    from echo_agent.bus.queue import MessageBus

    host = auth_kw.pop("host", "0.0.0.0")
    session_manager = MagicMock()
    session_manager.get_or_create = AsyncMock(return_value=MagicMock(status="active"))
    config = GatewayConfig(
        enabled=True,
        host=host,
        port=19998,
        auth=GatewayAuthConfig(mode="open", **auth_kw),
        session_policy=GatewaySessionPolicyConfig(mode="none"),
    )
    return GatewayServer(
        config=config,
        bus=MessageBus(),
        channel_manager=MagicMock(),
        session_manager=session_manager,
        workspace=MagicMock(),
        agent_loop=MagicMock(),
    )


def test_bind_safety_accepts_admin_tokens_only():
    """只配 admin_tokens 的部署绑 0.0.0.0 不该被拒绝启动。

    admin token 已经能通过读级校验,这样的部署并不是开放的;原判断只看 api_tokens,
    会把它当成"无 token"直接 raise。
    """
    gw = _gateway_with_tokens(admin_tokens=["admin-tok"])
    gw._check_bind_safety()  # 不抛异常即通过


def test_bind_safety_still_rejects_a_tokenless_public_bind():
    import pytest

    gw = _gateway_with_tokens()
    with pytest.raises(RuntimeError, match="without any"):
        gw._check_bind_safety()
    # loopback 上无 token 依然允许。
    _gateway_with_tokens(host="127.0.0.1")._check_bind_safety()


def test_ws_auth_enforced_when_only_admin_tokens_configured():
    """WS 握手的"是否需要校验 token"必须同时看两个列表。

    原条件是 `if config.auth.api_tokens and not authenticate_token(token)`,只配
    admin_tokens 时前半段为假,于是 WebSocket 这条主通道对任何 token 都放行。
    """
    gw = _gateway_with_tokens(admin_tokens=["admin-tok"])
    # 三处判断(读端点守卫 / 绑定安全 / WS 握手)现在共用同一个谓词。
    assert gw._tokens_configured() is True
    assert _gateway_with_tokens(api_tokens=["chat-tok"])._tokens_configured() is True
    assert _gateway_with_tokens()._tokens_configured() is False
    assert gw.auth.authenticate_token("admin-tok") is True
    assert gw.auth.authenticate_token("wrong") is False
