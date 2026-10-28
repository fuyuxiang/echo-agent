"""Keep legacy Chinese UI assertions deterministic while locale is global."""

import pytest

from echo_agent.cli.i18n import get_locale, set_locale


@pytest.fixture(autouse=True)
def _chinese_terminal_locale():
    saved = get_locale()
    set_locale("zh")
    try:
        yield
    finally:
        set_locale(saved)
