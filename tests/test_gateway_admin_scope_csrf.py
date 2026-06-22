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


def test_admin_token_open_when_no_tokens(tmp_path):
    auth = _auth(tmp_path)
    assert auth.authenticate_admin_token("") is True


# ── CSRF / Origin (opt-in via allowed_origins) ──────────────────────────────

def test_csrf_disabled_by_default_allows_cross_site(tmp_path):
    # No allowed_origins configured → CSRF enforcement off → nothing is blocked,
    # so existing clients (native HTTP, webview desktop) keep working unchanged.
    auth = _auth(tmp_path)
    assert auth.is_origin_allowed("http://evil.example", "cross-site") is True
    assert auth.is_origin_allowed("tauri://localhost", "cross-site") is True


def test_non_browser_request_allowed_when_enabled(tmp_path):
    auth = _auth(tmp_path, allowed_origins=["http://trusted.app"])
    # No Origin, no Sec-Fetch-Site → not a browser → allowed.
    assert auth.is_origin_allowed("", "") is True


def test_same_origin_allowed_when_enabled(tmp_path):
    auth = _auth(tmp_path, allowed_origins=["http://trusted.app"])
    assert auth.is_origin_allowed("http://127.0.0.1:9000", "same-origin") is True


def test_cross_site_rejected_when_enabled(tmp_path):
    auth = _auth(tmp_path, allowed_origins=["http://trusted.app"])
    assert auth.is_origin_allowed("http://evil.example", "cross-site") is False


def test_cross_site_allowed_when_allowlisted(tmp_path):
    auth = _auth(tmp_path, allowed_origins=["http://trusted.app"])
    assert auth.is_origin_allowed("http://trusted.app", "cross-site") is True
    assert auth.is_origin_allowed("http://evil.example", "cross-site") is False
