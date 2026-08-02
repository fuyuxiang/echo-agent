"""Media download hardening regressions.

Pins the guard that ``MediaCache.download`` gained after review: media URLs are
attacker-controlled (a POST /message body, an inbound chat attachment), so the
download path must enforce the same SSRF policy as web_fetch plus a real size
ceiling. Before this, ``download`` did a bare ``session.get`` + ``resp.read()``:
any URL, any address, any size.

The size tests matter most: ``Content-Length`` is a claim, so a hostile endpoint
that lies about it (or omits it) must still be cut off by the stream check.
"""

from __future__ import annotations

from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from echo_agent.config.schema import Config
from echo_agent.gateway.media import MediaCache
from echo_agent.security.net_guard import (
    FetchLimits,
    GuardedFetchError,
    guarded_download,
)


def _addrinfo(*ips: str):
    import socket
    return [(socket.AF_INET, socket.SOCK_STREAM, 6, "", (ip, 0)) for ip in ips]


def _response(*, status=200, headers=None, chunks=(b"data",)):
    """A stand-in aiohttp response whose body arrives via iter_chunked.

    Deliberately also supports ``await resp.read()``, the call the *unguarded*
    implementation used. Without that, a mock incompatible with the old code
    would make every "blocked" assertion below pass for the wrong reason (the
    mock broke, not the policy) — the trap that hid a meaningless green suite
    the first time these were written.
    """
    resp = MagicMock()
    resp.status = status
    resp.reason = ""
    resp.headers = headers or {}
    body = b"".join(chunks)
    resp.read = AsyncMock(return_value=body)

    async def _iter(_size):
        for chunk in chunks:
            yield chunk

    resp.content.iter_chunked = _iter
    resp.__aenter__ = AsyncMock(return_value=resp)
    resp.__aexit__ = AsyncMock(return_value=False)
    return resp


def _session_for(*responses):
    """A ClientSession whose successive .get() calls yield *responses*."""
    session = MagicMock()
    session.get = MagicMock(side_effect=list(responses))
    session.__aenter__ = AsyncMock(return_value=session)
    session.__aexit__ = AsyncMock(return_value=False)
    return session


# ── config defaults ──────────────────────────────────────────────────────────


def test_media_guard_defaults_are_fail_closed():
    """Built explicitly, never via load_config(): a machine-local yaml would
    make this assert the environment instead of the code."""
    gw = Config().gateway
    assert gw.media_allow_private_addresses is False
    assert gw.media_max_file_mb > 0
    assert gw.media_max_urls_per_message > 0
    assert gw.media_download_concurrency > 0


# ── address / scheme policy ──────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_download_blocks_private_address(tmp_path):
    cache = MediaCache(tmp_path)
    # The transport is mocked to *succeed*: if the address policy were absent,
    # the download would complete and write a file. Only the guard prevents it.
    ok = _response(headers={"Content-Type": "image/png"})
    with patch("socket.getaddrinfo", return_value=_addrinfo("10.0.0.5")), \
         patch("aiohttp.ClientSession", return_value=_session_for(ok)), \
         patch("aiohttp.TCPConnector", return_value=MagicMock()):
        result = await cache.download("http://internal.test/x.png", "api")
    assert result is None
    assert list(tmp_path.rglob("*.png")) == []


@pytest.mark.asyncio
async def test_download_blocks_cloud_metadata(tmp_path):
    """169.254.169.254 serves instance credentials — the payoff for an SSRF."""
    cache = MediaCache(tmp_path)
    ok = _response(chunks=(b"AWS_SECRET",))
    with patch("socket.getaddrinfo", return_value=_addrinfo("169.254.169.254")), \
         patch("aiohttp.ClientSession", return_value=_session_for(ok)), \
         patch("aiohttp.TCPConnector", return_value=MagicMock()):
        result = await cache.download(
            "http://metadata.test/latest/meta-data/iam/security-credentials/", "api",
        )
    assert result is None
    assert list(tmp_path.rglob("*")) == [tmp_path / "api"]


@pytest.mark.asyncio
async def test_download_rejects_file_scheme(tmp_path):
    """file:// would turn an attachment URL into a local-file read."""
    cache = MediaCache(tmp_path)
    ok = _response(chunks=(b"root:x:0:0:",))
    with patch("aiohttp.ClientSession", return_value=_session_for(ok)), \
         patch("aiohttp.TCPConnector", return_value=MagicMock()):
        result = await cache.download("file:///etc/passwd", "api")
    assert result is None


@pytest.mark.asyncio
async def test_scheme_gate_holds_even_when_private_allowed(tmp_path):
    """allow_private is an opt-in to internal *HTTP*, not to reading files."""
    cache = MediaCache(tmp_path, allow_private=True)
    ok = _response(chunks=(b"root:x:0:0:",))
    with patch("aiohttp.ClientSession", return_value=_session_for(ok)), \
         patch("aiohttp.TCPConnector", return_value=MagicMock()):
        result = await cache.download("file:///etc/passwd", "api")
    assert result is None


@pytest.mark.asyncio
async def test_redirect_to_private_is_revalidated_and_blocked(tmp_path):
    """A public URL that 302s inward must be blocked on the second hop."""
    cache = MediaCache(tmp_path)
    redirect = _response(status=302, headers={"Location": "http://internal.test/secret"})

    def fake_getaddrinfo(host, *a, **k):
        return _addrinfo("93.184.216.34") if host == "cdn.test" else _addrinfo("10.0.0.5")

    with patch("socket.getaddrinfo", side_effect=fake_getaddrinfo), \
         patch("aiohttp.ClientSession", return_value=_session_for(redirect)), \
         patch("aiohttp.TCPConnector", return_value=MagicMock()):
        result = await cache.download("https://cdn.test/a.png", "api")

    assert result is None


@pytest.mark.asyncio
async def test_credentials_dropped_on_cross_origin_redirect(tmp_path):
    """A 30x to another host must not carry the first host's Authorization."""
    hop1 = _response(status=302, headers={"Location": "https://other.test/b.png"})
    hop2 = _response(headers={"Content-Type": "image/png"})
    session = _session_for(hop1, hop2)

    with patch("socket.getaddrinfo", return_value=_addrinfo("93.184.216.34")), \
         patch("aiohttp.ClientSession", return_value=session), \
         patch("aiohttp.TCPConnector", return_value=MagicMock()):
        await guarded_download(
            "https://cdn.test/a.png",
            tmp_path / "out.png",
            headers={"Authorization": "Bearer secret", "Accept": "*/*"},
        )

    second_headers = session.get.call_args_list[1].kwargs["headers"]
    assert "Authorization" not in second_headers
    assert second_headers["Accept"] == "*/*"


# ── size ceiling ─────────────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_declared_oversize_is_rejected(tmp_path):
    """Cheap path: reject on Content-Length before reading a byte."""
    resp = _response(headers={"Content-Length": str(50 * 1024 * 1024)})

    with patch("socket.getaddrinfo", return_value=_addrinfo("93.184.216.34")), \
         patch("aiohttp.ClientSession", return_value=_session_for(resp)), \
         patch("aiohttp.TCPConnector", return_value=MagicMock()):
        with pytest.raises(GuardedFetchError, match="declares"):
            await guarded_download(
                "https://cdn.test/big.bin",
                tmp_path / "big.bin",
                limits=FetchLimits(max_bytes=1024),
            )


@pytest.mark.asyncio
async def test_lying_content_length_still_cut_off_by_stream(tmp_path):
    """Content-Length is a claim. A body that exceeds the limit despite a small
    (or absent) header must still be aborted mid-stream."""
    resp = _response(headers={"Content-Length": "10"}, chunks=(b"x" * 4096,) * 8)
    dest = tmp_path / "liar.bin"

    with patch("socket.getaddrinfo", return_value=_addrinfo("93.184.216.34")), \
         patch("aiohttp.ClientSession", return_value=_session_for(resp)), \
         patch("aiohttp.TCPConnector", return_value=MagicMock()):
        with pytest.raises(GuardedFetchError, match="exceeded"):
            await guarded_download(
                "https://cdn.test/liar.bin", dest, limits=FetchLimits(max_bytes=1024),
            )

    # No partial file survives — otherwise a later cache hit would serve a
    # truncated file as if it were complete.
    assert not dest.exists()
    assert list(tmp_path.glob(".*.part")) == []


@pytest.mark.asyncio
async def test_within_limit_download_succeeds_and_sniffs_extension(tmp_path):
    """The happy path still works, and a URL with no extension picks one up
    from the served Content-Type."""
    resp = _response(headers={"Content-Type": "image/png"}, chunks=(b"\x89PNG", b"rest"))
    cache = MediaCache(tmp_path)

    with patch("socket.getaddrinfo", return_value=_addrinfo("93.184.216.34")), \
         patch("aiohttp.ClientSession", return_value=_session_for(resp)), \
         patch("aiohttp.TCPConnector", return_value=MagicMock()):
        result = await cache.download("https://cdn.test/photo", "api")

    assert result is not None
    assert result.suffix == ".png"
    assert result.read_bytes() == b"\x89PNGrest"
