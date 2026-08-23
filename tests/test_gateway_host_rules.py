"""Bind-address / Host-header classification — the rules four callers share.

Before ``gateway/host_rules.py`` existed, each caller answered "is this bind
local?" on its own and they disagreed on exactly the values that matter. Two of
those disagreements were bugs with teeth:

* ``host: ""`` counted as loopback in ``_check_bind_safety`` and in the wizard,
  so an unauthenticated gateway was allowed to start on what is actually a
  wildcard bind (aiohttp binds "" to 0.0.0.0 *and* ::) — an unauthenticated
  network exposure.
* ``allowed_hosts`` was compared verbatim, so a pasted ``host:port``, a
  capitalized domain or a bare ``::1`` produced an allowlist that silently
  matched nothing.

These tests pin the rules themselves. The parity between the rules and each
caller is pinned separately (see test_setup_gateway_start.py and
test_gateway_default_startable.py).
"""

import pytest

from echo_agent.gateway.host_rules import (
    LOOPBACK_HOST_NAMES,
    is_loopback_bind,
    is_wildcard_bind,
    normalize_host,
    normalize_host_entries,
)


@pytest.mark.parametrize("host", ["", "   ", "0.0.0.0", "::", "[::]"])
def test_wildcard_binds_are_wildcards(host):
    assert is_wildcard_bind(host)
    # A wildcard is reachable *on* loopback but is not a loopback bind: the
    # question is always "can a stranger reach this", and the answer is yes.
    assert not is_loopback_bind(host)


@pytest.mark.parametrize(
    "host",
    [
        "127.0.0.1",
        "localhost",
        "LocalHost",
        "127.0.0.2",  # all of 127/8 is loopback; the old string tuple missed this
        "127.1.2.3",
        "::1",
        "[::1]",
        "[::1]:58123",
    ],
)
def test_loopback_binds_are_local(host):
    assert is_loopback_bind(host)
    assert not is_wildcard_bind(host)


@pytest.mark.parametrize(
    "host", ["192.168.1.5", "10.0.0.7", "203.0.113.9", "echo.example.com", "[fe80::1]"],
)
def test_routable_binds_are_neither(host):
    assert not is_loopback_bind(host)
    assert not is_wildcard_bind(host)


def test_hostnames_are_not_resolved():
    """Only the literal name ``localhost`` is local; nothing else is resolved.

    Resolution is attacker-influenced — that is the whole premise of DNS
    rebinding — and this predicate decides a security posture, so a name that
    happens to resolve to 127.0.0.1 today must not be treated as local.
    """
    assert not is_loopback_bind("localhost.evil.example")
    assert not is_loopback_bind("my-machine.local")


@pytest.mark.parametrize(
    "raw,expected",
    [
        ("Echo.Example.com", "echo.example.com"),          # case
        ("echo.example.com:58123", "echo.example.com"),    # pasted with port
        ("[::1]:58123", "[::1]"),                          # IPv6 with port
        ("::1", "[::1]"),                                  # bare IPv6 gets bracketed
        ("fe80::1", "[fe80::1]"),
        ("  spaced.example.com  ", "spaced.example.com"),
        ("", ""),
        ("[", ""),                                         # malformed, not a crash
    ],
)
def test_normalize_host_folds_the_forms_that_appear_in_practice(raw, expected):
    assert normalize_host(raw) == expected


def test_bare_ipv6_is_not_truncated_by_the_port_strip():
    """``::1`` must not lose a hextet to the trailing-":port" rule.

    A naive rsplit(":", 1) turns "::1" into ":" — which is why the bare-IPv6
    branch exists and why it runs before the port strip.
    """
    assert normalize_host("::1") == "[::1]"
    assert normalize_host("2001:db8::1") == "[2001:db8::1]"


def test_loopback_names_are_already_normalized():
    """The default-allow set must be in comparison form or it can never match."""
    for name in LOOPBACK_HOST_NAMES:
        assert normalize_host(name) == name


class TestNormalizeHostEntries:
    """Config-side normalization for ``auth.allowed_hosts``."""

    def test_wildcards_are_dropped(self):
        """``allowed_hosts: [0.0.0.0]`` is not a configured allowlist.

        A browser sends the name from its address bar, never the wildcard the
        server bound, so such an entry matches nothing while *looking*
        configured — which is what the wizard used to write by prefilling the
        bind address, and what then suppressed the "unreachable" warning.
        """
        assert normalize_host_entries(["0.0.0.0", "::", ""]) == []
        assert normalize_host_entries(["0.0.0.0", "echo.example.com"]) == ["echo.example.com"]

    def test_entries_are_normalized_and_deduplicated_in_order(self):
        assert normalize_host_entries(
            ["Echo.Example.com", "echo.example.com:58123", "::1", "[::1]"]
        ) == ["echo.example.com", "[::1]"]

    def test_non_list_and_non_string_inputs_do_not_raise(self):
        """Input can be a hand-edited YAML value of any shape."""
        assert normalize_host_entries(None) == []
        assert normalize_host_entries("a.com") == ["a.com"]
        assert normalize_host_entries(["a.com", 42, None]) == ["a.com"]
        assert normalize_host_entries(()) == []

    def test_result_is_yaml_writable(self):
        """Order-preserving list, not a set — the wizard writes it back."""
        result = normalize_host_entries(["b.com", "a.com", "b.com"])
        assert result == ["b.com", "a.com"]
        assert isinstance(result, list)
