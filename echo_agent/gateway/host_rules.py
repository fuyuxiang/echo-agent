"""Bind-address and Host-header classification — one set of rules, four callers.

Four places used to decide "is this bind address local?" independently, and
they disagreed on exactly the values that matter:

  - ``server.py:_check_bind_safety`` compared the string against a literal
    tuple that included ``""``, so an empty host counted as loopback and an
    unauthenticated gateway was allowed to start — but ``asyncio``/aiohttp
    bind ``""`` to ``0.0.0.0`` *and* ``::``, i.e. every interface. That was an
    unauthenticated network exposure.
  - ``server.py:_warn_host_allowlist_if_unset`` used ``ipaddress`` instead, so
    it disagreed with the check above on ``""`` (wildcard here, loopback
    there) and on ``127.0.0.2`` (loopback here, exposed there).
  - ``auth.py:_bound_is_loopback`` reached into ``cli.runtime_probe`` for the
    wildcard tuple and did not strip IPv6 brackets, so ``[::1]`` — the shape
    the value actually takes in a Host header — classified as non-loopback.
  - the setup wizard kept its own copy of the string tuple, inheriting the
    ``""`` bug and rejecting ``127.0.0.2``.

This module is the single answer. It lives under ``gateway/`` because the rules
are the gateway's, and it depends on nothing but the stdlib — no config schema,
no server, no aiohttp — so the setup wizard can share the rules while holding
only a raw, possibly hand-edited YAML dict, long before any server exists.
"""

from __future__ import annotations

import ipaddress
from urllib.parse import urlsplit

WILDCARD_HOSTS = frozenset({"0.0.0.0", "::", "[::]", ""})
"""Bind-only wildcards, i.e. "every interface".

``""`` belongs here, not with the loopback names: ``asyncio.start_server(host="")``
binds ``0.0.0.0`` and ``::``. A server on a wildcard *is* reachable on loopback,
but the wildcard is not itself a connectable address, so probes translate it to
``127.0.0.1`` first (see ``cli/runtime_probe.py``).
"""

LOOPBACK_HOST_NAMES = frozenset({"localhost", "127.0.0.1", "[::1]"})
"""Host-header values that mean "this machine" when the bind is loopback.

Listed explicitly rather than resolved, because the attack this guards against
— DNS rebinding — is precisely the case where the browser sends a Host that
*resolves* to 127.0.0.1 while naming the attacker's domain. Comparing strings
is the whole point. Values here are already in ``normalize_host`` form.
"""


def normalize_host(host: str) -> str:
    """Fold a Host header or configured allowlist entry to its comparison form.

    Lowercases, strips a trailing ``:port``, and normalizes IPv6 to bracketed
    form so ``::1``, ``[::1]`` and ``[::1]:58123`` all compare equal. Applied to
    *both* sides of the allowlist comparison: an operator who writes
    ``Echo.Example.com`` or pastes ``192.168.1.5:58123`` out of the address bar
    means the host, and a config that silently never matches is the failure this
    whole module exists to prevent.
    """
    host = (host or "").strip()
    if not host:
        return ""
    if host.startswith("["):
        # "[::1]:58123" / "[::1]" → "[::1]". Everything past the bracket is port.
        inner, sep, _rest = host.partition("]")
        if not sep:
            return ""  # malformed: unterminated bracket
        return "[" + inner.lstrip("[").lower() + "]"
    lowered = host.lower()
    if lowered.count(":") > 1:
        # Bare IPv6 literal without brackets ("::1", "fe80::1"). Bracket it so it
        # matches the shape a Host header carries; the trailing-port strip below
        # would otherwise chop a hextet off.
        try:
            return "[" + str(ipaddress.ip_address(lowered)) + "]"
        except ValueError:
            return lowered
    if ":" in lowered:
        return lowered.rsplit(":", 1)[0]
    return lowered


def is_wildcard_bind(host: str) -> bool:
    """Whether ``host`` binds every interface rather than one address."""
    return normalize_host(host) in WILDCARD_HOSTS


def is_loopback_bind(host: str) -> bool:
    """Whether a gateway bound to ``host`` is reachable only from this machine.

    A wildcard is never loopback even though loopback traffic reaches it — the
    point of the question is always "can a stranger reach this?". Covers the
    whole ``127.0.0.0/8`` and ``::1`` (so ``127.0.0.2``, used by some proxy
    setups, is correctly local) plus the name ``localhost``.
    """
    normalized = normalize_host(host)
    if not normalized or normalized in WILDCARD_HOSTS:
        return False
    if normalized == "localhost":
        return True
    literal = normalized[1:-1] if normalized.startswith("[") else normalized
    try:
        return ipaddress.ip_address(literal).is_loopback
    except ValueError:
        # A hostname other than localhost. Not resolved on purpose: resolution
        # is attacker-influenced and this decides a security posture.
        return False


def normalize_host_entries(entries: object) -> list[str]:
    """Normalize configured allowlist entries, dropping wildcards and blanks.

    Wildcard entries are dropped rather than kept because they cannot match any
    real Host header — a browser sends the name in the address bar, never
    ``0.0.0.0`` — so an ``allowed_hosts: [0.0.0.0]`` is an allowlist that
    rejects everything while *looking* configured. Order is preserved and
    duplicates collapse, so the value can be written straight back to YAML.
    """
    if isinstance(entries, str) or not isinstance(entries, (list, tuple, set, frozenset)):
        entries = [entries] if entries else []
    seen: dict[str, None] = {}
    for raw in entries:
        if not isinstance(raw, str):
            continue
        normalized = normalize_host(raw)
        if not normalized or normalized in WILDCARD_HOSTS:
            continue
        seen.setdefault(normalized, None)
    return list(seen)


def normalize_origin(origin: str) -> str:
    """Return a browser Origin in the exact form used for allowlist matching.

    Browsers serialize origins without a trailing slash and normally omit a
    scheme's default port. Operators, however, commonly paste a full address
    bar URL (``https://Example.com:443/``). Keeping those spellings verbatim
    creates an allowlist that looks configured but can never match. Only an
    origin is accepted here: paths, credentials, queries and fragments are not
    valid trust boundaries and are rejected rather than silently discarded.

    ``null`` is preserved as an explicit opt-in for sandboxed documents and
    webviews. It remains unsafe to add casually, but an operator who writes it
    deliberately must not have it transformed into another value.
    """
    origin = (origin or "").strip()
    if not origin:
        return ""
    if origin.lower() == "null":
        return "null"
    try:
        parsed = urlsplit(origin)
        port = parsed.port
    except ValueError:
        return ""
    if (
        not parsed.scheme
        or not parsed.hostname
        or parsed.username is not None
        or parsed.password is not None
        or parsed.path not in ("", "/")
        or parsed.query
        or parsed.fragment
    ):
        return ""

    scheme = parsed.scheme.lower()
    host = normalize_host(parsed.hostname)
    if not host:
        return ""
    default_port = 443 if scheme == "https" else 80 if scheme == "http" else None
    authority = host if port is None or port == default_port else f"{host}:{port}"
    return f"{scheme}://{authority}"


def normalize_origin_entries(entries: object) -> list[str]:
    """Normalize and deduplicate configured browser Origin entries."""
    if isinstance(entries, str) or not isinstance(entries, (list, tuple, set, frozenset)):
        entries = [entries] if entries else []
    seen: dict[str, None] = {}
    for raw in entries:
        if not isinstance(raw, str):
            continue
        normalized = normalize_origin(raw)
        if normalized:
            seen.setdefault(normalized, None)
    return list(seen)
