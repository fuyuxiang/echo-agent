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


def test_mask_sensitive_strings_scrubs_complete_quoted_basic_header():
    secret = "dXNlcjpwYXNz"
    out = mask_sensitive_strings(
        f"curl -H 'Authorization: Basic {secret}' https://example.test"
    )
    assert secret not in out
    assert "Authorization: Basic ••••" in out
    assert "https://example.test" in out


def test_mask_sensitive_strings_scrubs_complete_quoted_digest_header():
    out = mask_sensitive_strings(
        'curl -H "Authorization: Digest username=alice, realm=prod, nonce=xyz" '
        "https://example.test"
    )
    assert "alice" not in out
    assert "prod" not in out
    assert "xyz" not in out
    assert "Authorization: Digest ••••" in out
    assert "https://example.test" in out


def test_mask_sensitive_strings_scrubs_standalone_digest_header_line():
    out = mask_sensitive_strings(
        'Authorization: Digest username="alice", realm="prod", nonce="xyz"'
    )
    assert out == "Authorization: Digest ••••"


def test_mask_sensitive_strings_scrubs_unquoted_digest_command_tail():
    out = mask_sensitive_strings(
        "curl -H Authorization: Digest username=alice, realm=prod, nonce=xyz"
    )
    assert "alice" not in out
    assert "prod" not in out
    assert "xyz" not in out
    assert out.endswith("Authorization: Digest ••••")


def test_digest_field_names_without_authorization_are_not_over_redacted():
    assert mask_sensitive_strings("created username=alice") == "created username=alice"


def test_mask_sensitive_strings_scrubs_url_param():
    out = mask_sensitive_strings("https://x.test/a?token=abc123&b=1")
    assert "abc123" not in out
    assert "b=1" in out


def test_mask_sensitive_strings_scrubs_cli_flag():
    out = mask_sensitive_strings("curl --api-key hunter2 https://x.test")
    assert "hunter2" not in out


def test_mask_sensitive_strings_scrubs_quoted_cli_flag_value():
    out = mask_sensitive_strings(
        "curl --token='secret with spaces' https://x.test"
    )
    assert "secret with spaces" not in out
    assert "https://x.test" in out


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


def test_inline_secret_flag_does_not_mask_following_argv_item():
    out = redact_for_export(
        ["curl", "--token=sk-abcdefgh1234", "https://example.test"]
    )
    assert out == ["curl", "--token=••••", "https://example.test"]


def test_redact_for_export_preserves_scalars():
    out = redact_for_export({"n": 3, "ok": True, "nil": None})
    assert out == {"n": 3, "ok": True, "nil": None}


def test_format_params_one_line_per_entry():
    lines = format_params({"path": "/a/b.py", "password": "hunter2xyz"})
    assert lines[0] == "path=/a/b.py"
    assert lines[1] == "password=••••2xyz"


def test_format_params_tracks_secret_flag_value_in_argv_list():
    secret = "sk-abcdefgh1234"
    lines = format_params(
        {"command": ["curl", "--token", secret, "https://example.test"]},
        value_width=200,
    )
    assert secret not in lines[0]
    assert "••••1234" in lines[0]
    assert "https://example.test" in lines[0]


def test_format_params_inline_secret_flag_keeps_following_argv_visible():
    lines = format_params(
        {
            "command": [
                "curl",
                "--token=sk-abcdefgh1234",
                "https://example.test",
            ]
        },
        value_width=200,
    )
    assert "sk-abcdefgh1234" not in lines[0]
    assert "--token=••••" in lines[0]
    assert "https://example.test" in lines[0]
