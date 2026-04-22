"""Status command — display current configuration summary."""

from __future__ import annotations

from echo_agent.cli.colors import Colors, color, print_header, print_info
from echo_agent.config.loader import load_config, _find_config_file


def show_status() -> None:
    config_file = _find_config_file()
    print_header("Echo Agent Status")

    if config_file:
        print(f"  Config file:  {color(str(config_file), Colors.CYAN)}")
    else:
        print(f"  Config file:  {color('not found', Colors.YELLOW)}")

    config = load_config()

    print(f"  Workspace:    {color(config.workspace, Colors.CYAN)}")
    print()

    # Providers
    print_header("LLM Providers")
    if config.models.providers:
        for p in config.models.providers:
            models = ", ".join(p.models[:3]) if p.models else "—"
            print(f"  {color(p.name, Colors.GREEN):30s}  models: {models}")
    else:
        print_info("No providers configured")
    print(f"  Default model: {color(config.models.default_model, Colors.CYAN)}")
    print()

    # Channels
    print_header("Channels")
    channel_names = [
        "cli", "webhook", "cron", "telegram", "discord", "slack",
        "whatsapp", "wechat", "weixin", "qqbot", "feishu", "dingtalk",
        "email", "wecom", "matrix",
    ]
    any_enabled = False
    for name in channel_names:
        ch_cfg = getattr(config.channels, name, None)
        if ch_cfg is None:
            continue
        enabled = getattr(ch_cfg, "enabled", False)
        if enabled:
            print(f"  {color('●', Colors.GREEN)} {name}")
            any_enabled = True
        else:
            print(f"  {color('○', Colors.DIM)} {name}")
    if not any_enabled:
        print_info("Only CLI channel is active by default")
    print()

    # Gateway
    print_header("Gateway")
    if config.gateway.enabled:
        print(f"  {color('●', Colors.GREEN)} Enabled on {config.gateway.host}:{config.gateway.port}")
    else:
        print(f"  {color('○', Colors.DIM)} Disabled")
    print()
