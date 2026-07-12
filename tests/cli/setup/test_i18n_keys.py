"""Every new setup key must exist in both zh and en bundles."""
from __future__ import annotations

from echo_agent.cli.i18n import set_locale, t

_KEYS = [
    "provider.group.mainstream", "provider.group.domestic", "provider.group.aggregator",
    "provider.group.local", "provider.group.cloud", "provider.custom_label",
    "model.fetching", "model.verifying", "model.verify_ok", "model.verify_unreachable",
    "model.verify_error", "model.verify_action", "model.verify_retry_key",
    "model.verify_change_model", "model.verify_skip",
    "summary.gateway_off",
]


def test_all_new_keys_present_in_both_locales():
    for loc in ("zh", "en"):
        set_locale(loc)
        for key in _KEYS:
            assert t(key) != key, f"missing {key} in {loc}"
    set_locale("en")
