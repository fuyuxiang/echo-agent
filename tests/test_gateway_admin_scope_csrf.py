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
        _csrf_request({"Origin": "http://127.0.0.1:58123", "Sec-Fetch-Site": "same-origin"}),
        action="shutdown",
    ) is None
    assert gw._check_csrf(_csrf_request({}), action="shutdown") is None
