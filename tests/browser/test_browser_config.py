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


def test_browser_mounted_on_tools():
    c = Config()
    assert isinstance(c.tools, ToolsConfig)
    assert isinstance(c.tools.browser, BrowserToolConfig)
