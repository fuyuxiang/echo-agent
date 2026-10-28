"""Shared fixtures for the setup-section test package.

Several setup helpers (and ``run_setup_wizard``) mutate the process-global
locale via :func:`echo_agent.cli.i18n.set_locale`. Without a restore guard a
test that leaves the locale on a non-English value leaks into locale-sensitive
tests elsewhere, producing order-dependent failures. The autouse fixture below
snapshots and restores the locale around every test in this package.
"""

from __future__ import annotations

import pytest

from echo_agent.cli.i18n import get_locale, set_locale


@pytest.fixture(autouse=True)
def _restore_locale():
    saved = get_locale()
    try:
        yield
    finally:
        set_locale(saved)
