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
