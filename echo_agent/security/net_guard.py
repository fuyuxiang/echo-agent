"""Shared SSRF / resource-limit guard for every outbound HTTP fetch.

These primitives started life inside ``agent/tools/web.py``, guarding the one
surface that was obviously model-controlled (``web_fetch``). They live here
because the *media* path needs exactly the same policy and had none of it: a
chat attachment URL is just as attacker-controlled as a model-supplied one, and
``MediaCache.download`` reached it with no scheme check, no address validation,
no redirect re-validation and no size ceiling. Two callers, one policy, one set
of tests.

What a guarded fetch enforces, and why each part is load-bearing:

* **Scheme allowlist** — ``file://`` / ``data:`` would turn a URL fetch into a
  local-file read.
* **Address validation** — loopback, private, link-local (169.254.169.254 and
  friends: cloud instance metadata, i.e. credentials), reserved, multicast.
* **DNS pinning** — validating a hostname then letting the client re-resolve it
  reopens the rebinding window between check and connect. The connection is
  pinned to the exact address that passed validation.
* **Per-hop re-validation** — an unchecked 30x is a redirect *into* the network
  we just refused to talk to. Redirects are followed manually so every hop goes
  through the same gate, and credentials are dropped when the origin changes.
* **Size ceiling** — enforced on ``Content-Length`` *and* on the actual byte
  stream, because the header is a claim, not a fact. Streamed in chunks so a
  hostile endpoint cannot balloon the process before the limit is noticed.
"""

from __future__ import annotations

import ipaddress
import socket
from dataclasses import dataclass
from pathlib import Path
from typing import Any
from urllib.parse import urljoin, urlparse

import aiohttp

#: Schemes a guarded fetch will speak. Anything else (file, data, gopher, ftp)
#: is refused before a socket is opened.
ALLOWED_SCHEMES = ("http", "https")

#: Marks a ``resolve_and_validate`` error as "DNS/resolver broke" rather than
#: "the target is a policy-blocked address". Only the former is an infra fault:
#: an SSRF rejection is a stable verdict that retrying cannot fix, so it must
#: not open a circuit breaker for every other caller of the tool.
RESOLVE_FAILED_PREFIX = "Blocked: cannot resolve host"


def _ip_is_blocked(ip: ipaddress._BaseAddress) -> bool:
    return bool(
        ip.is_private or ip.is_loopback or ip.is_link_local
        or ip.is_reserved or ip.is_multicast or ip.is_unspecified
    )


async def resolve_and_validate(url: str) -> tuple[list[str], str | None]:
    """Resolve *url*'s host and validate every resolved IP.

    Returns ``(ips, error)``. ``ips`` is the list of validated address
    strings (safe to pin a connection to); ``error`` is non-None when the
    URL must be blocked. Pinning the returned IPs for the actual connection
    closes the DNS-rebinding window between validation and connect.

    Every address must pass: a hostname with one public and one private A
    record must not be usable to reach the private one.
    """
    import asyncio

    parsed = urlparse(url)
    if parsed.scheme not in ALLOWED_SCHEMES:
        return [], f"Blocked: unsupported URL scheme '{parsed.scheme}'"
    host = parsed.hostname
    if not host:
        return [], "Blocked: URL has no host"
    try:
        infos = await asyncio.to_thread(socket.getaddrinfo, host, None)
    except OSError as e:
        return [], f"Blocked: cannot resolve host '{host}': {e}"
    ips: list[str] = []
    for info in infos:
        addr = info[4][0]
        try:
            ip = ipaddress.ip_address(addr)
        except ValueError:
            continue
        if _ip_is_blocked(ip):
            return [], (
                f"Blocked: '{host}' resolves to non-public address {ip}. "
                "Set tools.web.allowPrivateAddresses to true to permit internal targets."
            )
        ips.append(addr)
    if not ips:
        return [], f"Blocked: cannot resolve host '{host}' to a usable address"
    return ips, None


async def check_url_ssrf(url: str) -> str | None:
    """Return an error message when *url* points at a non-public address.

    Blocks loopback, private, link-local (cloud metadata), reserved and
    multicast targets — callers take attacker- or model-controlled URLs, so
    without this they are a free proxy into the host's internal network.
    """
    _, error = await resolve_and_validate(url)
    return error


class PinnedResolver(aiohttp.abc.AbstractResolver):
    """aiohttp resolver that hands back a pre-validated IP for a host.

    Pins DNS so the address aiohttp connects to is exactly the one SSRF
    validation approved, defeating rebinding (validate IP_a, connect IP_b)."""

    def __init__(self, host_to_ips: dict[str, list[str]]):
        self._map = host_to_ips

    async def resolve(self, host: str, port: int = 0, family: int = socket.AF_INET) -> list[dict[str, Any]]:
        ips = self._map.get(host)
        if not ips:
            raise OSError(f"host '{host}' not in pinned set")
        results: list[dict[str, Any]] = []
        for addr in ips:
            try:
                fam = socket.AF_INET6 if ipaddress.ip_address(addr).version == 6 else socket.AF_INET
            except ValueError:
                continue
            if family not in (socket.AF_UNSPEC, fam):
                continue
            results.append({
                "hostname": host, "host": addr, "port": port,
                "family": fam, "proto": 0, "flags": socket.AI_NUMERICHOST,
            })
        if not results:
            raise OSError(f"no pinned address for '{host}' in family {family}")
        return results

    async def close(self) -> None:
        return None


#: Status codes a guarded fetch will follow, re-validating the target each hop.
_REDIRECT_STATUSES = (301, 302, 303, 307, 308)

#: Headers that must not survive a redirect to a different origin — otherwise a
#: 30x to an attacker's host hands them the credential meant for the first one.
_SENSITIVE_HEADERS = frozenset({"authorization", "cookie", "proxy-authorization"})


class GuardedFetchError(Exception):
    """A guarded fetch was refused, or exceeded a resource limit.

    Carries ``blocked_by_policy`` so callers can tell a stable verdict (SSRF
    rejection, over-size) from a transient infra fault (DNS down) and classify
    or retry accordingly."""

    def __init__(self, message: str, *, blocked_by_policy: bool = True):
        super().__init__(message)
        self.blocked_by_policy = blocked_by_policy


@dataclass(frozen=True)
class FetchLimits:
    """Resource ceilings for one guarded fetch.

    Defaults are deliberately conservative: a caller that forgets to pass
    limits still gets bounded behaviour rather than an unbounded read.
    """

    max_bytes: int = 25 * 1024 * 1024
    max_redirects: int = 5
    timeout_seconds: float = 60.0


def _origin(url: str) -> tuple[str, str, int | None]:
    parsed = urlparse(url)
    return parsed.scheme, (parsed.hostname or ""), parsed.port


def _strip_sensitive_headers(headers: dict[str, str]) -> dict[str, str]:
    return {k: v for k, v in headers.items() if k.lower() not in _SENSITIVE_HEADERS}


async def guarded_download(
    url: str,
    dest: Path,
    *,
    headers: dict[str, str] | None = None,
    limits: FetchLimits | None = None,
    allow_private: bool = False,
    on_content_type: Any = None,
) -> int:
    """Download *url* to *dest* under the full guard, returning bytes written.

    Redirects are followed manually so each hop is re-validated and pinned;
    sensitive headers are dropped on cross-origin hops. The size ceiling is
    checked against ``Content-Length`` first (cheap rejection) and then against
    the real stream (authoritative — the header is a claim).

    ``on_content_type`` is an optional callback invoked with the response's
    Content-Type once the final hop's headers are in. It lets a caller pick a
    file extension from the served type without re-reading the response.

    Nothing is written to *dest* until the body has been fully received within
    the limit: the bytes land in a sibling temp file that is atomically renamed
    on success and removed on any failure, so a rejected or truncated download
    never leaves a partial file that a later cache hit would serve as complete.

    Raises ``GuardedFetchError`` on policy rejection or limit breach.
    """
    limits = limits or FetchLimits()
    request_headers = dict(headers or {})
    current = url
    origin = _origin(url)

    for _hop in range(limits.max_redirects + 1):
        connector = None
        if not allow_private:
            ips, error = await resolve_and_validate(current)
            if error:
                raise GuardedFetchError(
                    error, blocked_by_policy=not error.startswith(RESOLVE_FAILED_PREFIX),
                )
            host = urlparse(current).hostname or ""
            connector = aiohttp.TCPConnector(resolver=PinnedResolver({host: ips}))
        elif urlparse(current).scheme not in ALLOWED_SCHEMES:
            # allow_private skips address validation but never the scheme gate:
            # reaching internal HTTP is a deliberate opt-in, reading local files
            # through file:// is not.
            raise GuardedFetchError(
                f"Blocked: unsupported URL scheme '{urlparse(current).scheme}'"
            )

        async with aiohttp.ClientSession(connector=connector) as session:
            async with session.get(
                current,
                headers=request_headers,
                allow_redirects=False,
                timeout=aiohttp.ClientTimeout(total=limits.timeout_seconds),
            ) as resp:
                if resp.status in _REDIRECT_STATUSES:
                    location = resp.headers.get("Location")
                    if not location:
                        raise GuardedFetchError(
                            f"Blocked: HTTP {resp.status} redirect without Location"
                        )
                    current = urljoin(current, location)
                    if _origin(current) != origin:
                        request_headers = _strip_sensitive_headers(request_headers)
                        origin = _origin(current)
                    continue

                if resp.status != 200:
                    raise GuardedFetchError(
                        f"download failed (HTTP {resp.status})", blocked_by_policy=False,
                    )

                declared = resp.headers.get("Content-Length")
                if declared:
                    try:
                        if int(declared) > limits.max_bytes:
                            raise GuardedFetchError(
                                f"Blocked: response declares {declared} bytes, "
                                f"over the {limits.max_bytes} byte limit"
                            )
                    except ValueError:
                        pass  # unparseable header: fall through to stream check

                if on_content_type is not None:
                    on_content_type(resp.headers.get("Content-Type", ""))

                return await _stream_to_file(resp, dest, limits.max_bytes)

    raise GuardedFetchError(f"Blocked: exceeded {limits.max_redirects} redirects")


async def _stream_to_file(resp: aiohttp.ClientResponse, dest: Path, max_bytes: int) -> int:
    """Stream *resp* into *dest*, aborting past *max_bytes*.

    Chunked rather than ``resp.read()``: reading first and measuring after lets
    a hostile endpoint exhaust memory before the limit is ever consulted.
    """
    dest.parent.mkdir(parents=True, exist_ok=True)
    tmp = dest.with_name(f".{dest.name}.part")
    written = 0
    try:
        with tmp.open("wb") as fh:
            async for chunk in resp.content.iter_chunked(64 * 1024):
                written += len(chunk)
                if written > max_bytes:
                    raise GuardedFetchError(
                        f"Blocked: response exceeded the {max_bytes} byte limit"
                    )
                fh.write(chunk)
        tmp.replace(dest)
        return written
    except BaseException:
        tmp.unlink(missing_ok=True)
        raise
