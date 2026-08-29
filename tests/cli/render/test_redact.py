from echo_agent.cli.render.redact import (
    format_params, is_secret_key, mask, mask_sensitive_strings,
    redact_for_export,
)


def test_is_secret_key_matches_substring():
    assert is_secret_key("DB_PASSWORD")
    assert is_secret_key("api_key")
    assert is_secret_key("Authorization")
    assert not is_secret_key("path")


def test_mask_keeps_last_four():
    assert mask("sk-abcdefgh1234") == "••••1234"


def test_mask_short_value_fully():
    assert mask("abc") == "••••"


def test_mask_sensitive_strings_scrubs_bearer():
    out = mask_sensitive_strings("Authorization: Bearer sk-secret-token")
    assert "sk-secret-token" not in out
    assert "••••" in out


def test_mask_sensitive_strings_scrubs_url_param():
    out = mask_sensitive_strings("https://x.test/a?token=abc123&b=1")
    assert "abc123" not in out
    assert "b=1" in out


def test_mask_sensitive_strings_scrubs_cli_flag():
    out = mask_sensitive_strings("curl --api-key hunter2 https://x.test")
    assert "hunter2" not in out


def test_redact_for_export_masks_secret_key():
    out = redact_for_export({"path": "/a", "api_key": "sk-abcdefgh1234"})
    assert out["path"] == "/a"
    assert out["api_key"] == "••••1234"


def test_redact_for_export_recurses_into_nested_dict():
    out = redact_for_export({"cfg": {"password": "topsecret"}})
    assert out["cfg"]["password"] == "••••cret"


def test_redact_for_export_masks_value_after_secret_flag_in_list():
    out = redact_for_export(["curl", "--token", "sk-abcdefgh1234", "url"])
    assert "sk-abcdefgh1234" not in out
    assert out[0] == "curl"


def test_redact_for_export_preserves_scalars():
    out = redact_for_export({"n": 3, "ok": True, "nil": None})
    assert out == {"n": 3, "ok": True, "nil": None}


def test_format_params_one_line_per_entry():
    lines = format_params({"path": "/a/b.py", "password": "hunter2xyz"})
    assert lines[0] == "path=/a/b.py"
    assert lines[1] == "password=••••2xyz"
