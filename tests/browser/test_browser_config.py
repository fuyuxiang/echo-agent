from echo_agent.config.schema import BrowserToolConfig, Config, ToolsConfig


def test_browser_config_defaults():
    c = BrowserToolConfig()
    assert c.enabled is True
    assert c.max_sessions == 3
    assert c.session_idle_timeout_sec == 300
    assert c.max_snapshot_chars == 8000
    assert c.headless is True
    assert c.nav_timeout_sec == 30
    assert c.allow_private_addresses is False


def test_new_capability_defaults_are_conservative():
    """Dialogs are dismissed, JS escape hatch and login persistence are opt-in."""
    c = BrowserToolConfig()
    assert c.dialog_policy == "dismiss"
    assert c.allow_unsafe_evaluate is False
    assert c.persist_login_state is False


def test_session_caps_cover_both_scopes():
    """max_sessions is per-owner, so a global ceiling is needed as well."""
    c = BrowserToolConfig()
    assert c.max_total_sessions == 10
    assert BrowserToolConfig(max_total_sessions=0).max_total_sessions == 0


def test_viewport_defaults_are_desktop_sized():
    c = BrowserToolConfig()
    assert (c.viewport_width, c.viewport_height) == (1280, 800)
    assert c.user_agent == ""


def test_overrides_are_applied():
    c = BrowserToolConfig(dialog_policy="accept", allow_unsafe_evaluate=True,
                          persist_login_state=True, viewport_width=1920,
                          viewport_height=1080, user_agent="UA/1")
    assert c.dialog_policy == "accept"
    assert c.allow_unsafe_evaluate is True
    assert c.persist_login_state is True
    assert (c.viewport_width, c.viewport_height) == (1920, 1080)
    assert c.user_agent == "UA/1"


def test_browser_mounted_on_tools():
    c = Config()
    assert isinstance(c.tools, ToolsConfig)
    assert isinstance(c.tools.browser, BrowserToolConfig)
