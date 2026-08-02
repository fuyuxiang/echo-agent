"""Installer / default-config startability regressions.

The shipped defaults used to combine into a gateway that could not start:
``host: 0.0.0.0`` (schema) + empty ``auth.apiTokens`` (schema) + gateway forced
on by the ``gateway`` entrypoint, against a ``_check_bind_safety`` that refuses
a non-loopback bind with no token. A quickstart install never visits the gateway
section — so it never sets a token — and therefore always produced a service
that failed on every start.

The fix is the default bind address, NOT the safety check: refusing to expose an
unauthenticated agent is correct and stays. These tests pin both halves, so a
future "make it reachable out of the box" change cannot quietly delete the gate.
"""

from __future__ import annotations

from pathlib import Path
from unittest.mock import MagicMock

import pytest

from echo_agent.config.schema import Config
from echo_agent.gateway.server import GatewayServer


def _server(cfg: Config, tmp_path: Path) -> GatewayServer:
    return GatewayServer(
        cfg.gateway, MagicMock(), MagicMock(), MagicMock(), tmp_path,
    )


def test_default_gateway_binds_loopback():
    """Config() built explicitly — never load_config(), which would read the
    machine's own yaml and make this assert the environment."""
    assert Config().gateway.host == "127.0.0.1"


def test_default_config_starts_without_a_token(tmp_path):
    """The exact combination a quickstart install writes must be startable."""
    cfg = Config()
    assert cfg.gateway.auth.api_tokens == []
    assert cfg.gateway.auth.admin_tokens == []

    _server(cfg, tmp_path)._check_bind_safety()  # must not raise


def test_exposed_bind_without_token_is_still_refused(tmp_path):
    """The safety gate is load-bearing: binding to the network with no token of
    any kind must remain a hard startup failure."""
    cfg = Config()
    cfg.gateway.host = "0.0.0.0"

    with pytest.raises(RuntimeError, match="without any"):
        _server(cfg, tmp_path)._check_bind_safety()


def test_exposed_bind_with_token_is_allowed(tmp_path):
    cfg = Config()
    cfg.gateway.host = "0.0.0.0"
    cfg.gateway.auth.api_tokens = ["s3cret"]

    _server(cfg, tmp_path)._check_bind_safety()  # must not raise


def test_wizard_host_default_matches_schema():
    """The wizard's prompt default is a separate literal from the schema's. When
    they drift, accepting every prompt writes a config the schema never blessed —
    which is how the unstartable default survived: the schema said 0.0.0.0 and so
    did the wizard, each looking like it was following the other."""
    import inspect

    from echo_agent.cli import setup as setup_mod

    source = inspect.getsource(setup_mod.setup_gateway)
    assert 'gw.get("host", "127.0.0.1")' in source
    assert '"0.0.0.0"' not in source
