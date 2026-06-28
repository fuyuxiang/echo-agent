from echo_agent.config.profile_defaults import apply_profile_cognitive_defaults


def test_personal_cli_disables_planning_by_default():
    data = {"security": {"profile": "personal_cli"}, "planning": {}}
    out = apply_profile_cognitive_defaults(data)
    assert out["planning"]["enabled"] is False


def test_daemon_keeps_planning_enabled():
    data = {"security": {"profile": "daemon"}, "planning": {}}
    out = apply_profile_cognitive_defaults(data)
    assert out["planning"]["enabled"] is True


def test_user_explicit_value_is_never_overridden():
    # 用户在 cli profile 下显式打开 planning —— 必须保留
    data = {"security": {"profile": "personal_cli"}, "planning": {"enabled": True}}
    out = apply_profile_cognitive_defaults(data)
    assert out["planning"]["enabled"] is True


def test_cli_sets_retrieval_degrade_on_miss():
    data = {"security": {"profile": "personal_cli"}, "memory": {}}
    out = apply_profile_cognitive_defaults(data)
    assert out["memory"]["retrieval_on_miss"] == "degrade"


def test_daemon_sets_retrieval_sync_on_miss():
    data = {"security": {"profile": "daemon"}, "memory": {}}
    out = apply_profile_cognitive_defaults(data)
    assert out["memory"]["retrieval_on_miss"] == "sync"


def test_section_none_is_skipped_not_injected():
    # planning 显式为 None —— 非 dict 分支应被跳过，保持 None
    data = {"security": {"profile": "personal_cli"}, "planning": None}
    out = apply_profile_cognitive_defaults(data)
    assert out["planning"] is None


def test_section_non_dict_is_left_untouched():
    # planning 为非 dict 字符串 —— 不崩，原值保留
    data = {"security": {"profile": "personal_cli"}, "planning": "weird"}
    out = apply_profile_cognitive_defaults(data)
    assert out["planning"] == "weird"


def test_unknown_profile_injects_nothing():
    data = {"security": {"profile": "nonexistent"}}
    out = apply_profile_cognitive_defaults(data)
    assert out == {"security": {"profile": "nonexistent"}}


def test_missing_security_applies_default_profile():
    # No security block at all → mirror pydantic's default (personal_cli),
    # so a zero-config CLI run gets the lean defaults (planning off).
    data = {}
    out = apply_profile_cognitive_defaults(data)
    assert out["planning"]["enabled"] is False
    assert out["memory"]["retrieval_on_miss"] == "degrade"


def test_missing_profile_applies_default_profile():
    # security present but no profile → same default-profile fallback.
    data = {"security": {}}
    out = apply_profile_cognitive_defaults(data)
    assert out["planning"]["enabled"] is False
    assert out["memory"]["retrieval_on_miss"] == "degrade"


def test_public_gateway_enables_planning_and_sync_memory():
    data = {"security": {"profile": "public_gateway"}, "planning": {}, "memory": {}}
    out = apply_profile_cognitive_defaults(data)
    assert out["planning"]["enabled"] is True
    assert out["memory"]["retrieval_on_miss"] == "sync"
