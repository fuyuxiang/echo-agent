# tests/test_gateway_dns_rebinding.py
"""Closing DNS rebinding at the Gateway HTTP/WS boundary.

The cross-site Origin/Sec-Fetch-Site check is not enough on its own. A
DNS-rebinding page sends ``Sec-Fetch-Site: same-origin`` — and that is *correct
from the browser's point of view*: the page that made the request did share
the same scheme/host/port as itself. The Origin is also consistent (it is the
attacker's own page origin), and the Host is whatever the page's URL was. After
rebind, that Host resolves to 127.0.0.1 — so the gateway sees a same-origin
browser request from a loopback peer, and grants the loopback exemption.

The defense is that the Host must name a host this gateway was *intended* to
be reached on: a loopback name (``localhost`` / ``127.0.0.1`` / ``[::1]``) when
bound to loopback, an explicitly listed reverse-proxy domain otherwise. Origin
vs Host is not enough — both are attacker-controlled the moment DNS rebinds.
"""
from __future__ import annotations

from pathlib import Path
import tempfile

import pytest
from aiohttp import web
from aiohttp.test_utils import make_mocked_request

from echo_agent.config.schema import GatewayAuthConfig
from echo_agent.gateway import ws_common
from echo_agent.gateway.auth import GatewayAuth


@pytest.fixture
def tmp_data_dir() -> Path:
    return Path(tempfile.mkdtemp())


@pytest.fixture
def loopback_auth(tmp_data_dir: Path) -> GatewayAuth:
    """Gateway bound to a loopback address — the default local install shape."""
    return GatewayAuth(
        GatewayAuthConfig(),
        tmp_data_dir,
        bound_host="127.0.0.1",
    )


@pytest.fixture
def lan_auth(tmp_data_dir: Path) -> GatewayAuth:
    """Gateway bound to a non-loopback address (reverse-proxy or LAN)."""
    return GatewayAuth(
        GatewayAuthConfig(allowed_hosts=["echo.example.com"]),
        tmp_data_dir,
        bound_host="0.0.0.0",
    )


@pytest.mark.parametrize("origin,sec_fetch_site,host", [
    # The actual attack: evil.example page rebinds to 127.0.0.1. Browser sends
    # the same-origin Sec-Fetch-Site (correct from its perspective) plus the
    # attacker's Host. Origin/Host are consistent — this is exactly what made
    # the previous CSRF check alone insufficient.
    ("http://evil.example:58123", "same-origin", "evil.example:58123"),
    ("http://evil.example:58123", "same-site",   "evil.example:58123"),
    # Same host with explicit port on the request — slight variation, same shape.
    ("http://evil.example",      "same-origin", "evil.example"),
])
def test_rebinding_attack_is_blocked(
    loopback_auth: GatewayAuth, origin: str, sec_fetch_site: str, host: str,
) -> None:
    csrf = loopback_auth.is_cross_site_browser(origin, sec_fetch_site, host)
    host_ok = loopback_auth.is_host_allowed(host)
    # Both gates must agree: reject. In every case csrf is False (the rebind
    # looks same-origin) — host_ok is what catches it.
    assert csrf is False
    assert host_ok is False


@pytest.mark.parametrize("host", [
    "127.0.0.1", "127.0.0.1:58123",
    "localhost", "localhost:58123",
    "[::1]", "[::1]:58123",
])
def test_legitimate_loopback_hosts_pass(loopback_auth: GatewayAuth, host: str) -> None:
    """A genuine dashboard page on the loopback gateway must still work."""
    assert loopback_auth.is_host_allowed(host) is True


def test_empty_host_header_is_rejected(loopback_auth: GatewayAuth) -> None:
    """A bare ``Host: `` (or no header at all) cannot pass on its own.

    Native clients typically send Host, so this only affects malformed or
    hostile peers. The point is that the check is independent of cross-site
    routing — a missing Host must not be treated as "no claim, therefore OK".
    """
    assert loopback_auth.is_host_allowed("") is False
    assert loopback_auth.is_host_allowed(None) is False


def test_lan_binding_with_empty_allowlist_refuses_everything(
    tmp_data_dir: Path,
) -> None:
    """Bound to 0.0.0.0 with no allowed_hosts: nothing is host-trusted.

    This is the deployment-mistake guard. Even loopback hostnames are rejected,
    because the gateway is publicly reachable and "127.0.0.1" is no longer the
    only path in. The operator must list their proxy domain.
    """
    auth = GatewayAuth(GatewayAuthConfig(), tmp_data_dir, bound_host="0.0.0.0")
    for host in ("localhost", "127.0.0.1", "echo.example.com", "127.0.0.1:58123"):
        assert auth.is_host_allowed(host) is False, f"{host} unexpectedly passed"


def test_lan_binding_with_proxy_domain_in_allowlist(
    lan_auth: GatewayAuth,
) -> None:
    """Reverse-proxy deployment: only the listed proxy domain is trusted.

    A rebound attacker controlling ``evil.example:58123`` cannot reach the gateway
    through this code path, even though they share the loopback peer exemption
    (the actual peer *is* 127.0.0.1; the rebind simply got them past the IP layer).
    """
    assert lan_auth.is_host_allowed("echo.example.com") is True
    assert lan_auth.is_host_allowed("echo.example.com:443") is True
    assert lan_auth.is_host_allowed("evil.example:58123") is False
    assert lan_auth.is_host_allowed("127.0.0.1") is False


def test_proxy_domain_match_is_case_insensitive(lan_auth: GatewayAuth) -> None:
    """Host header casing is a browser quirk, not a signal.

    ``ECHO.example.com`` and ``echo.EXAMPLE.com`` must both match, otherwise
    a reconfigure could reopen the gap for no security gain.
    """
    assert lan_auth.is_host_allowed("ECHO.example.com") is True
    assert lan_auth.is_host_allowed("echo.EXAMPLE.com") is True


def test_origin_vs_host_equality_is_not_the_defense() -> None:
    """Pin the assumption the gate rests on.

    If a future refactor re-introduces the old "Origin matches Host ⇒ safe"
    check, this test documents that it would have failed to stop a rebind: the
    rebinding page deliberately produces consistent Origin/Host/Sec-Fetch-Site,
    so any check that depends on their disagreement is blind to the attack.
    """
    origin, host = "http://evil.example:58123", "evil.example:58123"
    # The attack passes a naive Origin-vs-Host equality test.
    parsed_origin = origin.split("//", 1)[1]
    assert parsed_origin == host, "rebind must look consistent — otherwise it's not a rebind"


@pytest.mark.parametrize("host", [
    "127.0.0.1:58123",
    "echo.example.com:443",
])
def test_host_with_port_normalizes_correctly(
    tmp_data_dir: Path, host: str,
) -> None:
    """``Host: 127.0.0.1:58123`` must compare equal to ``127.0.0.1``.

    Otherwise an operator listing ``localhost`` in allowed_hosts would not
    cover the gateway's actual served port — a subtle false negative.
    """
    auth = GatewayAuth(
        GatewayAuthConfig(allowed_hosts=["127.0.0.1", "echo.example.com"]),
        tmp_data_dir, bound_host="0.0.0.0",
    )
    assert auth.is_host_allowed(host) is True


# ── Integration: the WS handshake path ──────────────────────────────────────
#
# ``is_host_allowed`` alone is necessary but not sufficient. The full defense
# lives in ``reject_cross_site``, which composes the cross-site Origin/Sec-
# Fetch-Site check with the Host allowlist. A unit-level test on
# ``is_host_allowed`` cannot catch a regression where someone removes the
# composition from the WS path (or POST /message), so this section covers the
# integration.


def _request(headers: dict[str, str]) -> web.Request:
    """Build an aiohttp Request carrying only the given headers."""
    return make_mocked_request("GET", "/ws", headers=headers)


def test_reject_cross_site_blocks_rebind_at_ws_handshake(
    loopback_auth: GatewayAuth,
) -> None:
    """The full WS handshake rejection path stops the rebind.

    A rebinding page sends ``Sec-Fetch-Site: same-origin`` (the cross-site
    gate is happy) and a Host that names the attacker's domain. The composed
    check must refuse, which here means returning a 403 Response from
    ``reject_cross_site``.
    """
    req = _request({
        "Origin": "http://evil.example:58123",
        "Sec-Fetch-Site": "same-origin",
        "Host": "evil.example:58123",
    })
    response = ws_common.reject_cross_site(req, loopback_auth, action="ws_test")
    assert response is not None
    assert response.status == 403


def test_reject_cross_site_passes_legitimate_loopback_dashboard(
    loopback_auth: GatewayAuth,
) -> None:
    """A genuine loopback dashboard request still gets a clean pass."""
    req = _request({
        "Origin": "http://127.0.0.1:58123",
        "Sec-Fetch-Site": "same-origin",
        "Host": "127.0.0.1:58123",
    })
    assert ws_common.reject_cross_site(req, loopback_auth, action="ws_test") is None


def test_reject_cross_site_passes_native_client_with_no_browser_headers(
    loopback_auth: GatewayAuth,
) -> None:
    """The CLI sends no Origin/Sec-Fetch-Site; it must not be affected."""
    req = _request({"Host": "127.0.0.1:58123"})
    assert ws_common.reject_cross_site(req, loopback_auth, action="ws_test") is None