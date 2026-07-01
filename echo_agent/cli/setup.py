"""Interactive setup wizard for Echo Agent.

The wizard is structured into ten sections, each runnable independently:

    1. Language          — auto-detected, overridable
    2. Model & Provider  — LLM provider + default model
    3. Permissions       — approval mode (smart/manual/off)
    4. Sandbox           — execution backend (local/sandbox/container/remote)
    5. Agent Behavior    — max_iterations / compression / session reset
    6. Tools             — profile + optional integrations (web/tts/mcp/...)
    7. Channels          — messaging integrations + allowlist
    8. Gateway           — Web/WS API exposure
    9. Observability     — log level + OpenTelemetry export
   10. Evolution         — self-evolving skill harness (off by default)

Followed by a Capability Check ("doctor") + summary.

Locale is auto-detected from the OS, but ``--lang`` overrides it and the
user-selected locale is persisted to ``ui.locale`` so subsequent runs are
consistent.
"""

from __future__ import annotations

import math
from pathlib import Path
from typing import Any, Callable

from echo_agent.cli.colors import (
    Colors,
    color,
    print_error,
    print_info,
    print_success,
    print_warning,
)
from echo_agent.cli.i18n import detect_locale, get_locale, set_locale, t
from echo_agent.cli.prompt import (
    is_interactive,
    prompt,
    prompt_checklist,
    prompt_choice,
    prompt_yes_no,
)
from echo_agent.config.loader import find_local_config_file, resolve_config_file, save_config
from echo_agent.runtime_paths import default_config_path


# ── Provider / channel presets ────────────────────────────────────────────────

PROVIDERS: list[tuple[str, str, list[str]]] = [
    ("openai", "OpenAI", [
        "gpt-4o", "gpt-4o-mini", "gpt-4-turbo", "o1", "o3-mini",
    ]),
    ("anthropic", "Anthropic", [
        "claude-sonnet-4-20250514", "claude-opus-4-20250514", "claude-haiku-4-5-20251001",
    ]),
    ("gemini", "Google Gemini", [
        "gemini-2.5-pro", "gemini-2.5-flash",
    ]),
    ("openrouter", "OpenRouter", [
        "openai/gpt-4o", "anthropic/claude-sonnet-4-20250514", "google/gemini-2.5-pro",
    ]),
    ("bedrock", "AWS Bedrock", [
        "anthropic.claude-sonnet-4-20250514-v1:0", "anthropic.claude-haiku-4-5-20251001-v1:0",
    ]),
    ("custom", "Custom (OpenAI-compatible)", []),
]

CHANNEL_DEFS: list[tuple[str, str, list[tuple[str, str]]]] = [
    ("telegram", "Telegram", [("token", "Bot token")]),
    ("discord", "Discord", [("token", "Bot token")]),
    ("slack", "Slack", [("bot_token", "Bot token"), ("app_token", "App token")]),
    ("dingtalk", "DingTalk", [("app_key", "App key"), ("app_secret", "App secret"), ("robot_code", "Robot code")]),
    ("feishu", "Feishu / Lark", [("app_id", "App ID"), ("app_secret", "App secret")]),
    ("wecom", "WeCom", [("corp_id", "Corp ID"), ("agent_id", "Agent ID"), ("secret", "Secret")]),
    ("weixin", "WeChat", []),
    ("qqbot", "QQ Bot", [("app_id", "App ID"), ("app_secret", "App secret")]),
    ("email", "Email", [("imap_host", "IMAP host"), ("smtp_host", "SMTP host"), ("username", "Username"), ("password", "Password")]),
    ("matrix", "Matrix", [("homeserver", "Homeserver URL"), ("user_id", "User ID"), ("access_token", "Access token")]),
]


# ── Banner & helpers ──────────────────────────────────────────────────────────

def _print_banner() -> None:
    title = t("banner.title")
    subtitle = t("banner.subtitle")
    exit_hint = t("banner.exit_hint")
    width = max(len(title), len(subtitle), len(exit_hint), 50)
    inner = width + 4
    print()
    print(color("  ┌" + "─" * inner + "┐", Colors.CYAN))
    print(color(f"  │  {title.center(width)}  │", Colors.CYAN))
    print(color("  ├" + "─" * inner + "┤", Colors.CYAN))
    print(color(f"  │  {subtitle.ljust(width)}  │", Colors.CYAN))
    print(color(f"  │  {exit_hint.ljust(width)}  │", Colors.CYAN))
    print(color("  └" + "─" * inner + "┘", Colors.CYAN))


def _print_section_header(key: str) -> None:
    label = t(f"section.{key}")
    print()
    print(color(f"  ◆ {label}", Colors.CYAN, Colors.BOLD))
    print(color("  " + "─" * (len(label) + 2), Colors.DIM))


def _ensure_dict(parent: dict, key: str) -> dict:
    value = parent.get(key)
    if not isinstance(value, dict):
        value = {}
        parent[key] = value
    return value


# ── Section 1: Language ───────────────────────────────────────────────────────

def setup_language(config: dict) -> None:
    _print_section_header("language")
    auto_label = t("language.english") if get_locale() == "en" else t("language.chinese")
    print_info(t("language.auto_detected", label=auto_label))
    choices = [t("language.english"), t("language.chinese")]
    default = 0 if get_locale() == "en" else 1
    idx = prompt_choice(t("language.prompt"), choices, default=default)
    chosen = "en" if idx == 0 else "zh"
    set_locale(chosen)
    _ensure_dict(config, "ui")["locale"] = chosen
    print_success(t("language.saved"))


# ── Section 2: Model & Provider ──────────────────────────────────────────────

def setup_model(config: dict) -> None:
    _print_section_header("model")
    print_info(t("model.intro"))
    print()

    models_block = _ensure_dict(config, "models")
    existing_providers = models_block.get("providers", []) or []
    existing_provider = existing_providers[0] if existing_providers else {}
    existing_name = existing_provider.get("name", "")
    existing_key = existing_provider.get("apiKey", "") or existing_provider.get("api_key", "")
    existing_base = existing_provider.get("apiBase", "") or existing_provider.get("api_base", "")
    existing_model = models_block.get("defaultModel", "") or models_block.get("default_model", "")

    provider_default = 0
    for i, (key, _label, _models) in enumerate(PROVIDERS):
        if key == existing_name or (existing_name == "openai" and key == "custom" and existing_base):
            provider_default = i
            break

    provider_names = [p[1] for p in PROVIDERS]
    idx = prompt_choice(t("model.select_provider"), provider_names, default=provider_default)
    provider_key, provider_label, preset_models = PROVIDERS[idx]

    api_key = ""
    api_base = ""
    if provider_key == "bedrock":
        print_info(t("model.bedrock_hint"))
    elif provider_key == "custom":
        api_base = prompt(f"  {t('model.api_base')}", default=existing_base)
        if existing_key:
            api_key = prompt(f"  {t('model.api_key_kept')}", password=True)
            if not api_key:
                api_key = existing_key
        else:
            api_key = prompt(f"  {t('model.api_key')}", password=True)
    else:
        if existing_key and existing_name == provider_key:
            api_key = prompt(f"  {provider_label} {t('model.api_key_kept')}", password=True)
            if not api_key:
                api_key = existing_key
        else:
            api_key = prompt(f"  {provider_label} {t('model.api_key')}", password=True)
            if not api_key:
                print_warning(t("model.api_key_missing"))

    if preset_models:
        custom_idx = len(preset_models)
        model_default = custom_idx
        if existing_model in preset_models:
            model_default = preset_models.index(existing_model)
        model_choices = preset_models + [t("model.model_custom")]
        model_idx = prompt_choice(t("model.model_select"), model_choices, default=model_default)
        if model_idx == custom_idx:
            default_model = prompt(f"  {t('model.model_name')}", default=existing_model)
        else:
            default_model = preset_models[model_idx]
    else:
        default_model = ""
        while not default_model:
            default_model = prompt(f"  {t('model.model_name')}", default=existing_model)
            if not default_model:
                print_warning(t("model.model_required_custom"))

    while not default_model:
        print_warning(t("model.model_required"))
        default_model = prompt(f"  {t('model.model_name')}", default=existing_model)

    actual_name = provider_key if provider_key != "custom" else "openai"
    provider_entry: dict[str, Any] = {"name": actual_name}
    if api_key:
        provider_entry["apiKey"] = api_key
    if api_base:
        provider_entry["apiBase"] = api_base
    if preset_models:
        provider_entry["models"] = preset_models

    config["models"] = {
        "defaultModel": default_model,
        "providers": [provider_entry],
    }
    print_success(t("model.saved", provider=provider_label, model=default_model))


# ── Section 3: Permissions & Approval ────────────────────────────────────────

def setup_permissions(config: dict) -> None:
    _print_section_header("permissions")
    print_info(t("permissions.intro"))
    print()

    perms = _ensure_dict(config, "permissions")
    approval = _ensure_dict(perms, "approval")
    current_mode = approval.get("mode", "smart")

    mode_keys = ["smart", "manual", "off"]
    mode_labels = [t(f"permissions.mode_{k}") for k in mode_keys]
    default_mode_idx = mode_keys.index(current_mode) if current_mode in mode_keys else 0
    mode_idx = prompt_choice(t("permissions.mode_prompt"), mode_labels, default=default_mode_idx)
    chosen_mode = mode_keys[mode_idx]
    approval["mode"] = chosen_mode

    if chosen_mode == "smart":
        existing_smart_model = approval.get("smart_model", "") or approval.get("smartModel", "")
        smart_model = prompt(f"  {t('permissions.smart_model')}", default=existing_smart_model)
        if smart_model:
            approval["smart_model"] = smart_model
        else:
            approval.pop("smart_model", None)
            approval.pop("smartModel", None)

    unattended_keys = ["deny", "allow_safe"]
    unattended_labels = [t(f"permissions.unattended_{k}") for k in unattended_keys]
    current_unatt = approval.get("unattended_policy") or approval.get("unattendedPolicy") or "deny"
    default_unatt_idx = unattended_keys.index(current_unatt) if current_unatt in unattended_keys else 0
    unatt_idx = prompt_choice(t("permissions.unattended"), unattended_labels, default=default_unatt_idx)
    approval["unattended_policy"] = unattended_keys[unatt_idx]

    cli_auto_default = approval.get("cli_auto_approve")
    if cli_auto_default is None:
        cli_auto_default = approval.get("cliAutoApprove", True)
    approval["cli_auto_approve"] = prompt_yes_no(t("permissions.cli_auto"), default=bool(cli_auto_default))

    print_success(t("permissions.saved", mode=t(f"permissions.mode_{chosen_mode}")))


# ── Section 4: Sandbox / Execution Backend ───────────────────────────────────

def setup_terminal(config: dict) -> None:
    _print_section_header("terminal")
    print_info(t("terminal.intro"))
    print()

    execution = _ensure_dict(config, "execution")
    backend_keys = ["local", "sandbox", "container", "remote"]
    backend_labels = [t(f"terminal.{k}") for k in backend_keys]
    current_backend = execution.get("default_executor") or execution.get("defaultExecutor") or "sandbox"
    default_idx = backend_keys.index(current_backend) if current_backend in backend_keys else 1
    idx = prompt_choice(t("terminal.select"), backend_labels, default=default_idx)
    chosen = backend_keys[idx]
    execution["default_executor"] = chosen

    if chosen == "container":
        existing_image = execution.get("container_image") or execution.get("containerImage") or ""
        image = prompt(f"  {t('terminal.container_image')}", default=existing_image or "python:3.11-slim")
        execution["container_image"] = image
    elif chosen == "remote":
        execution["remote_host"] = prompt(f"  {t('terminal.remote_host')}", default=execution.get("remote_host", ""))
        execution["remote_user"] = prompt(f"  {t('terminal.remote_user')}", default=execution.get("remote_user", "root"))
        existing_key = execution.get("remote_key_path") or execution.get("remoteKeyPath") or ""
        execution["remote_key_path"] = prompt(f"  {t('terminal.remote_key')}", default=existing_key)

    network_keys = ["allow", "deny", "restricted"]
    network_labels = [t(f"terminal.network_{k}") for k in network_keys]
    current_net = execution.get("network_policy") or execution.get("networkPolicy") or "deny"
    default_net = network_keys.index(current_net) if current_net in network_keys else 1
    net_idx = prompt_choice(t("terminal.network"), network_labels, default=default_net)
    execution["network_policy"] = network_keys[net_idx]

    tools = _ensure_dict(config, "tools")
    exec_cfg = _ensure_dict(tools, "exec")
    sec_keys = ["deny", "allowlist", "full"]
    sec_labels = [t(f"terminal.exec_security_{k}") for k in sec_keys]
    current_sec = exec_cfg.get("security", "allowlist")
    default_sec = sec_keys.index(current_sec) if current_sec in sec_keys else 1
    sec_idx = prompt_choice(t("terminal.exec_security"), sec_labels, default=default_sec)
    exec_cfg["security"] = sec_keys[sec_idx]

    print_success(t("terminal.saved", backend=t(f"terminal.{chosen}"), network=t(f"terminal.network_{network_keys[net_idx]}")))


# ── Section 5: Agent Behavior ─────────────────────────────────────────────────

def setup_agent(config: dict) -> None:
    _print_section_header("agent")
    print_info(t("agent.intro"))
    print()

    agent = _ensure_dict(config, "agent")
    current_iter = int(agent.get("max_iterations") or agent.get("maxIterations") or 40)
    print_info(t("agent.max_iter_hint"))
    raw = prompt(f"  {t('agent.max_iter')}", default=str(current_iter))
    try:
        agent["max_iterations"] = max(1, int(raw))
    except ValueError:
        print_warning(t("common.invalid"))
        agent["max_iterations"] = current_iter

    compression = _ensure_dict(config, "compression")
    compression["enabled"] = prompt_yes_no(t("agent.compression_enabled"), default=bool(compression.get("enabled", True)))
    if compression["enabled"]:
        current_thr = float(compression.get("trigger_ratio") or compression.get("triggerRatio") or 0.7)
        print_info(t("agent.compression_threshold_hint"))
        raw = prompt(f"  {t('agent.compression_threshold')}", default=f"{current_thr:.2f}")
        try:
            value = float(raw)
            if 0.5 <= value <= 0.95:
                compression["trigger_ratio"] = value
            else:
                print_warning(t("common.invalid"))
        except ValueError:
            print_warning(t("common.invalid"))

    gateway = _ensure_dict(config, "gateway")
    sp = _ensure_dict(gateway, "sessionPolicy")
    reset_keys = ["both", "idle", "daily", "none"]
    reset_labels = [t(f"agent.session_reset_{k}") for k in reset_keys]
    current_reset = sp.get("mode", "idle")
    default_reset = reset_keys.index(current_reset) if current_reset in reset_keys else 1
    reset_idx = prompt_choice(t("agent.session_reset"), reset_labels, default=default_reset)
    sp["mode"] = reset_keys[reset_idx]
    if reset_keys[reset_idx] in ("idle", "both"):
        cur_idle = int(sp.get("idleTimeoutMinutes") or sp.get("idle_timeout_minutes") or 1440)
        raw = prompt(f"  {t('agent.idle_minutes')}", default=str(cur_idle))
        try:
            sp["idleTimeoutMinutes"] = max(1, int(raw))
        except ValueError:
            sp["idleTimeoutMinutes"] = cur_idle
    if reset_keys[reset_idx] in ("daily", "both"):
        cur_hr = int(sp.get("dailyResetHour") or sp.get("daily_reset_hour") or 4)
        raw = prompt(f"  {t('agent.daily_hour')}", default=str(cur_hr))
        try:
            v = int(raw)
            sp["dailyResetHour"] = v if 0 <= v <= 23 else cur_hr
        except ValueError:
            sp["dailyResetHour"] = cur_hr

    planning = _ensure_dict(config, "planning")
    planning["enabled"] = prompt_yes_no(t("agent.planning_enabled"), default=bool(planning.get("enabled", True)))
    memory = _ensure_dict(config, "memory")
    memory["enabled"] = prompt_yes_no(t("agent.memory_enabled"), default=bool(memory.get("enabled", True)))

    print_success(t("agent.saved"))


# ── Section 6: Tools ──────────────────────────────────────────────────────────

TOOL_OPTIONS = ["web", "image_gen", "tts", "code_exec", "knowledge", "cron", "mcp", "skills", "plugins"]


def setup_tools(config: dict) -> None:
    _print_section_header("tools")
    print_info(t("tools.intro"))
    print()

    tools = _ensure_dict(config, "tools")
    profile_keys = ["minimal", "messaging", "coding", "full"]
    profile_labels = [t(f"tools.profile_{k}") for k in profile_keys]
    # Keep the fallback in sync with schema.py (ToolsConfig.profile) and
    # default.yaml (tools.profile) — all three default to "full". A stale
    # "coding" fallback here silently drops execute_code/exec from the
    # exposed tool set even though they're registered and approved.
    current_profile = tools.get("profile", "full")
    default_profile = profile_keys.index(current_profile) if current_profile in profile_keys else profile_keys.index("full")
    p_idx = prompt_choice(t("tools.profile"), profile_labels, default=default_profile)
    tools["profile"] = profile_keys[p_idx]

    pre_selected: list[int] = []
    if (tools.get("web", {}) or {}).get("enabled"):
        pre_selected.append(TOOL_OPTIONS.index("web"))
    image_block = tools.get("image_gen") or tools.get("imageGen") or {}
    if image_block.get("api_key") or image_block.get("apiKey") or image_block.get("fal_key") or image_block.get("falKey"):
        pre_selected.append(TOOL_OPTIONS.index("image_gen"))
    tts_block = tools.get("tts", {}) or {}
    if tts_block.get("openai_api_key") or tts_block.get("openaiApiKey") or tts_block.get("default_backend"):
        pre_selected.append(TOOL_OPTIONS.index("tts"))
    code_exec_block = tools.get("code_exec") or tools.get("codeExec") or {}
    if code_exec_block.get("enabled", True):
        pre_selected.append(TOOL_OPTIONS.index("code_exec"))
    if (config.get("knowledge", {}) or {}).get("enabled", True):
        pre_selected.append(TOOL_OPTIONS.index("knowledge"))
    if (config.get("scheduler", {}) or {}).get("enabled", True):
        pre_selected.append(TOOL_OPTIONS.index("cron"))
    if tools.get("mcp_servers") or tools.get("mcpServers"):
        pre_selected.append(TOOL_OPTIONS.index("mcp"))
    if (config.get("skills", {}) or {}).get("skills_dir") or (config.get("skills", {}) or {}).get("skillsDir"):
        pre_selected.append(TOOL_OPTIONS.index("skills"))
    if (config.get("plugins", {}) or {}).get("enabled", True):
        pre_selected.append(TOOL_OPTIONS.index("plugins"))
    pre_selected = sorted(set(pre_selected))

    labels = [t(f"tools.{k}") for k in TOOL_OPTIONS]
    selected = prompt_checklist(t("tools.checklist"), labels, pre_selected=pre_selected)
    chosen = {TOOL_OPTIONS[i] for i in selected}

    if "web" in chosen:
        web = _ensure_dict(tools, "web")
        web["enabled"] = True
        provider_choices = ["brave", "tavily", "serpapi", "searxng"]
        cur_prov = web.get("search_provider") or web.get("searchProvider") or "brave"
        prov_idx = prompt_choice(t("tools.web_provider"), provider_choices,
                                 default=provider_choices.index(cur_prov) if cur_prov in provider_choices else 0)
        web["search_provider"] = provider_choices[prov_idx]
        existing_key = web.get("search_api_key") or web.get("searchApiKey") or ""
        if existing_key:
            new_key = prompt(f"  {t('tools.web_api_key')} [****{t('common.saved')}]", password=True)
            if new_key:
                web["search_api_key"] = new_key
        else:
            new_key = prompt(f"  {t('tools.web_api_key')}", password=True)
            if new_key:
                web["search_api_key"] = new_key
    else:
        if "web" in tools and isinstance(tools["web"], dict):
            tools["web"]["enabled"] = False

    if "image_gen" in chosen:
        ig = _ensure_dict(tools, "image_gen")
        backend_options = [t("tools.image_backend_openai"), t("tools.image_backend_fal")]
        backend_values = ["openai", "fal"]
        cur_backend = ig.get("backend", "openai")
        b_idx = prompt_choice(t("tools.image_backend"), backend_options,
                              default=backend_values.index(cur_backend) if cur_backend in backend_values else 0)
        ig["backend"] = backend_values[b_idx]

        if backend_values[b_idx] == "fal":
            existing = ig.get("fal_key") or ig.get("falKey") or ""
            if existing:
                new_key = prompt(f"  {t('tools.image_fal_key')} [****{t('common.saved')}]", password=True)
                if new_key:
                    ig["fal_key"] = new_key
            else:
                new_key = prompt(f"  {t('tools.image_fal_key')}", password=True)
                if new_key:
                    ig["fal_key"] = new_key
            ig["fal_model"] = prompt(f"  {t('tools.image_fal_model')}", default=ig.get("fal_model") or ig.get("falModel") or "fal-ai/flux/schnell")
        else:
            existing = ig.get("api_key") or ig.get("apiKey") or ""
            if existing:
                new_key = prompt(f"  {t('tools.image_api_key')} [****{t('common.saved')}]", password=True)
                if new_key:
                    ig["api_key"] = new_key
            else:
                new_key = prompt(f"  {t('tools.image_api_key')}", password=True)
                if new_key:
                    ig["api_key"] = new_key
            ig["api_base"] = prompt(f"  {t('tools.image_api_base')}", default=ig.get("api_base") or ig.get("apiBase") or "https://api.openai.com/v1")
            ig["model"] = prompt(f"  {t('tools.image_model')}", default=ig.get("model", "dall-e-3"))

    if "tts" in chosen:
        tts = _ensure_dict(tools, "tts")
        backends = ["edge", "openai", "elevenlabs"]
        cur_backend = tts.get("default_backend") or tts.get("defaultBackend") or "edge"
        b_idx = prompt_choice(t("tools.tts_backend"), backends,
                              default=backends.index(cur_backend) if cur_backend in backends else 0)
        tts["default_backend"] = backends[b_idx]
        if backends[b_idx] == "openai":
            existing = tts.get("openai_api_key") or tts.get("openaiApiKey") or ""
            if existing:
                new_key = prompt(f"  {t('tools.tts_openai_key')} [****{t('common.saved')}]", password=True)
                if new_key:
                    tts["openai_api_key"] = new_key
            else:
                tts["openai_api_key"] = prompt(f"  {t('tools.tts_openai_key')}", password=True)
            tts["openai_api_base"] = prompt(f"  {t('tools.tts_openai_base')}", default=tts.get("openai_api_base", "https://api.openai.com/v1"))
            tts["model"] = prompt(f"  {t('tools.tts_model')}", default=tts.get("model", "tts-1"))

    code_exec = _ensure_dict(tools, "code_exec")
    code_exec["enabled"] = "code_exec" in chosen
    _ensure_dict(config, "knowledge")["enabled"] = "knowledge" in chosen
    _ensure_dict(config, "scheduler")["enabled"] = "cron" in chosen
    _ensure_dict(config, "plugins")["enabled"] = "plugins" in chosen

    if "mcp" in chosen and not (tools.get("mcp_servers") or tools.get("mcpServers")):
        print_info(t("tools.mcp_skip_hint"))

    extras_str = ", ".join(t(f"tools.{k}") for k in TOOL_OPTIONS if k in chosen) or t("common.no")
    print_success(t("tools.saved", profile=t(f"tools.profile_{profile_keys[p_idx]}"), extras=extras_str))


# ── Section 7: Messaging Channels ─────────────────────────────────────────────

def _setup_weixin_qr(ch: dict) -> None:
    """Run the iLink QR-code login flow for WeChat Personal."""
    import asyncio

    print_info(t("channels.weixin_qr"))
    print_info(t("channels.weixin_qr_hint"))
    print()
    from echo_agent.channels.weixin import WeixinChannel
    result = asyncio.run(WeixinChannel.qr_login())
    if result:
        ch["account_id"] = result["account_id"]
        ch["token"] = result["token"]
        if result.get("base_url"):
            ch["base_url"] = result["base_url"]
        print_success(t("channels.weixin_ok"))
    else:
        print_error(t("channels.weixin_fail"))
        print_info(t("channels.weixin_retry"))


def setup_channels(config: dict) -> None:
    _print_section_header("channel")
    print_info(t("channels.intro"))
    print()

    existing = _ensure_dict(config, "channels")
    pre_selected: list[int] = []
    for i, (ch_key, _label, _fields) in enumerate(CHANNEL_DEFS):
        ch_cfg = existing.get(ch_key, {})
        if isinstance(ch_cfg, dict) and ch_cfg.get("enabled"):
            pre_selected.append(i)

    channel_names = [c[1] for c in CHANNEL_DEFS]
    selected = prompt_checklist(t("channels.checklist"), channel_names, pre_selected=pre_selected or None)
    if not selected:
        print_info(t("channels.no_extra"))
        return

    for idx in selected:
        ch_key, ch_label, fields = CHANNEL_DEFS[idx]
        print()
        print(color(f"  ── {t('channels.config_for', label=ch_label)} ──", Colors.CYAN))
        ch = _ensure_dict(existing, ch_key)
        ch["enabled"] = True

        if ch_key == "weixin":
            _setup_weixin_qr(ch)
            continue

        for field_key, field_label in fields:
            secret = any(s in field_key.lower() for s in ("key", "secret", "token", "password"))
            value = prompt(f"  {field_label}", default=ch.get(field_key, ""), password=secret)
            if value:
                ch[field_key] = value

        if ch_key in ("telegram", "discord", "slack", "qqbot", "email", "weixin", "dingtalk"):
            existing_allow = ch.get("allow_from") or ch.get("allowFrom") or []
            allow_default = ",".join(existing_allow) if isinstance(existing_allow, list) else str(existing_allow or "")
            allow_raw = prompt(f"  {t('channels.allow_from')}", default=allow_default)
            if allow_raw:
                ch["allow_from"] = [s.strip() for s in allow_raw.split(",") if s.strip()]
            else:
                ch.pop("allow_from", None)
                ch.pop("allowFrom", None)
                print_warning(t("channels.allow_warn"))

            home_existing = ch.get("home_channel") or ch.get("homeChannel") or ""
            home = prompt(f"  {t('channels.home_channel')}", default=home_existing)
            if home:
                ch["home_channel"] = home

    print_success(t("channels.saved", n=len(selected)))


# ── Section 8: Gateway ────────────────────────────────────────────────────────

def setup_gateway(config: dict) -> None:
    _print_section_header("gateway")
    print_info(t("gateway.intro"))
    print()

    gw = _ensure_dict(config, "gateway")
    gw["enabled"] = prompt_yes_no(t("gateway.enable"), default=bool(gw.get("enabled", False)))
    if not gw["enabled"]:
        return

    gw["host"] = prompt(f"  {t('gateway.host')}", default=str(gw.get("host", "0.0.0.0")))
    port_str = prompt(f"  {t('gateway.port')}", default=str(gw.get("port", 9000)))
    try:
        gw["port"] = int(port_str)
    except ValueError:
        gw["port"] = 9000
        print_warning(t("common.invalid"))

    auth = _ensure_dict(gw, "auth")
    auth_keys = ["open", "allowlist", "pairing"]
    auth_labels = [t(f"gateway.auth_{k}") for k in auth_keys]
    cur_auth = auth.get("mode", "allowlist")
    default_auth = auth_keys.index(cur_auth) if cur_auth in auth_keys else 1
    a_idx = prompt_choice(t("gateway.auth_mode"), auth_labels, default=default_auth)
    auth["mode"] = auth_keys[a_idx]

    # An "open" (no-token) gateway bound to a non-loopback host is refused at
    # startup by gateway/server.py:_check_bind_safety, which would leave the
    # whole service unable to boot. Catch the combo here rather than letting it
    # fail after save.
    host_norm = str(gw["host"]).strip()
    if auth_keys[a_idx] == "open" and host_norm not in ("127.0.0.1", "localhost", "::1", ""):
        print_warning(t("gateway.open_exposed_warn", host=host_norm))
        if prompt_yes_no(t("gateway.open_exposed_fix"), default=True):
            gw["host"] = "127.0.0.1"
            print_info(t("gateway.host_pinned"))
        else:
            # Keep the exposed host but force a token-bearing mode.
            a_idx = auth_keys.index("allowlist")
            auth["mode"] = "allowlist"

    if auth_keys[a_idx] in ("allowlist", "pairing"):
        existing_tokens = auth.get("api_tokens") or auth.get("apiTokens") or []
        token_default = existing_tokens[0] if existing_tokens else ""
        token = prompt(f"  {t('gateway.api_token')}", default=token_default, password=True)
        if not token:
            import secrets
            token = secrets.token_urlsafe(32)
            print_info(f"  Generated token: {token}")
        auth["api_tokens"] = [token]

    print_success(t("gateway.saved", host=gw["host"], port=gw["port"], mode=t(f"gateway.auth_{auth_keys[a_idx]}")))


# ── Section 9: Observability ─────────────────────────────────────────────────

def setup_observability(config: dict) -> None:
    _print_section_header("observability")
    print_info(t("observability.intro"))
    print()

    obs = _ensure_dict(config, "observability")
    log_choices = ["INFO", "DEBUG", "WARNING", "ERROR"]
    cur_level = (obs.get("log_level") or obs.get("logLevel") or "INFO").upper()
    default_log = log_choices.index(cur_level) if cur_level in log_choices else 0
    l_idx = prompt_choice(t("observability.log_level"), log_choices, default=default_log)
    obs["log_level"] = log_choices[l_idx]

    obs["trace_enabled"] = prompt_yes_no(t("observability.trace"), default=bool(obs.get("trace_enabled", True)))

    otel_on = prompt_yes_no(t("observability.otel"), default=bool(obs.get("otel_enabled", False)))
    obs["otel_enabled"] = otel_on
    if otel_on:
        obs["otel_endpoint"] = prompt(f"  {t('observability.otel_endpoint')}",
                                       default=obs.get("otel_endpoint") or obs.get("otelEndpoint") or "http://localhost:4317")
        obs["otel_service_name"] = prompt(f"  {t('observability.otel_service')}",
                                           default=obs.get("otel_service_name") or obs.get("otelServiceName") or "echo-agent")

    print_success(t("observability.saved",
                    level=log_choices[l_idx],
                    otel=t("common.yes") if otel_on else t("common.no")))


# ── Section 10: Self-evolving skill harness ───────────────────────────────────

_EVOLUTION_TRIGGER_KEYS: list[str] = ["manual", "threshold", "scheduled"]


def setup_evolution(config: dict) -> None:
    """Configure the self-evolving skill harness.

    Disabled by default. When enabled, this section also writes
    ``data/eval/baseline.yaml`` (the gating dataset) if it does not already
    exist, since promotion is impossible without it.
    """
    _print_section_header("evolution")
    print_info(t("evolution.intro"))
    print_warning(t("evolution.warning"))
    print()

    evo = _ensure_dict(config, "evolution")

    enabled = prompt_yes_no(
        t("evolution.enabled"),
        default=bool(evo.get("enabled", False)),
    )
    evo["enabled"] = enabled
    if not enabled:
        evo["record_trajectories"] = bool(evo.get("record_trajectories", True))
        print_success(t("evolution.saved_disabled"))
        return

    # Trigger mode
    trigger_labels = [t(f"evolution.trigger_{k}") for k in _EVOLUTION_TRIGGER_KEYS]
    current_trigger = evo.get("trigger_mode") or evo.get("triggerMode") or "manual"
    default_trigger = (
        _EVOLUTION_TRIGGER_KEYS.index(current_trigger)
        if current_trigger in _EVOLUTION_TRIGGER_KEYS
        else 0
    )
    print_info(t("evolution.trigger_hint"))
    t_idx = prompt_choice(t("evolution.trigger"), trigger_labels, default=default_trigger)
    trigger = _EVOLUTION_TRIGGER_KEYS[t_idx]
    evo["trigger_mode"] = trigger

    if trigger == "threshold":
        cur = int(evo.get("threshold_trajectories") or evo.get("thresholdTrajectories") or 50)
        raw = prompt(f"  {t('evolution.threshold')}", default=str(cur))
        try:
            evo["threshold_trajectories"] = max(1, int(raw))
        except ValueError:
            print_warning(t("common.invalid"))
            evo["threshold_trajectories"] = cur
    elif trigger == "scheduled":
        cur = evo.get("cron_expression") or evo.get("cronExpression") or "0 4 * * *"
        raw = prompt(f"  {t('evolution.cron')}", default=cur)
        evo["cron_expression"] = raw or cur

    # Eval dataset path
    cur_dataset = evo.get("eval_dataset_path") or evo.get("evalDatasetPath") or "data/eval/baseline.yaml"
    raw = prompt(f"  {t('evolution.dataset_path')}", default=cur_dataset)
    evo["eval_dataset_path"] = raw or cur_dataset

    # Strict / regression policy
    evo["require_strict_improvement"] = prompt_yes_no(
        t("evolution.strict"),
        default=bool(evo.get("require_strict_improvement", True)),
    )
    cur_thr = float(evo.get("regression_threshold") or evo.get("regressionThreshold") or 0.05)
    print_info(t("evolution.regression_hint"))
    raw = prompt(f"  {t('evolution.regression')}", default=f"{cur_thr:.2f}")
    try:
        v = float(raw)
        if 0.0 <= v <= 0.5:
            evo["regression_threshold"] = v
        else:
            print_warning(t("common.invalid"))
    except ValueError:
        print_warning(t("common.invalid"))

    # Operational knobs
    evo["candidate_review_required"] = prompt_yes_no(
        t("evolution.review_required"),
        default=bool(evo.get("candidate_review_required", False)),
    )
    cur_cand = int(evo.get("max_candidates_per_run") or evo.get("maxCandidatesPerRun") or 3)
    raw = prompt(f"  {t('evolution.max_candidates')}", default=str(cur_cand))
    try:
        evo["max_candidates_per_run"] = max(1, int(raw))
    except ValueError:
        evo["max_candidates_per_run"] = cur_cand

    cur_retain = int(evo.get("trajectory_retention_days") or evo.get("trajectoryRetentionDays") or 30)
    raw = prompt(f"  {t('evolution.retention_days')}", default=str(cur_retain))
    try:
        evo["trajectory_retention_days"] = max(0, int(raw))
    except ValueError:
        evo["trajectory_retention_days"] = cur_retain

    evo["redact_args"] = prompt_yes_no(
        t("evolution.redact_args"),
        default=bool(evo.get("redact_args", True)),
    )
    evo["record_trajectories"] = prompt_yes_no(
        t("evolution.record"),
        default=bool(evo.get("record_trajectories", True)),
    )

    # Eval execution knobs (used by PromotionGate)
    cur_par = int(evo.get("eval_parallel") or evo.get("evalParallel") or 2)
    raw = prompt(f"  {t('evolution.eval_parallel')}", default=str(cur_par))
    try:
        evo["eval_parallel"] = max(1, int(raw))
    except ValueError:
        evo["eval_parallel"] = cur_par

    cur_to = int(evo.get("eval_timeout_seconds") or evo.get("evalTimeoutSeconds") or 60)
    raw = prompt(f"  {t('evolution.eval_timeout')}", default=str(cur_to))
    try:
        evo["eval_timeout_seconds"] = max(1, int(raw))
    except ValueError:
        evo["eval_timeout_seconds"] = cur_to

    # Ensure the baseline dataset exists — otherwise PromotionGate will
    # reject every candidate. Resolve workspace from config (it may be
    # relative; in that case we anchor at cwd).
    ws = _resolve_workspace(config)
    dataset_path = ws / evo["eval_dataset_path"]
    if not dataset_path.exists():
        try:
            dataset_path.parent.mkdir(parents=True, exist_ok=True)
            from echo_agent.cli.evolution_cmd import _DEFAULT_DATASET
            dataset_path.write_text(_DEFAULT_DATASET, encoding="utf-8")
            print_info(t("evolution.dataset_seeded", path=str(dataset_path)))
        except Exception as e:
            print_warning(t("evolution.dataset_seed_failed", error=str(e)))

    print_success(t(
        "evolution.saved_enabled",
        trigger=trigger,
        dataset=evo["eval_dataset_path"],
    ))


# ── Section 11: Cost budget ───────────────────────────────────────────────────

def setup_cost(config: dict) -> None:
    """Configure the daily cost budget gate.

    Disabled by default. When enabled, asks for a daily cap in USD. A cap of
    0 (or a negative value, which is clamped to 0) means metering-only with no
    hard stop — see echo_agent/cost/budget.py. Leaving the prompt blank keeps
    the current value.
    """
    _print_section_header("cost")
    print_info(t("cost.intro"))
    print()

    cost = _ensure_dict(config, "cost")
    enabled = prompt_yes_no(t("cost.enable"), default=bool(cost.get("enabled", False)))
    cost["enabled"] = enabled
    if not enabled:
        print_success(t("cost.saved_disabled"))
        return

    cur_budget = float(cost.get("daily_budget_usd") or cost.get("dailyBudgetUsd") or 0.0)
    raw = prompt(f"  {t('cost.daily_budget')}", default=f"{cur_budget:.2f}")
    try:
        budget = float(raw)
        if not math.isfinite(budget):
            raise ValueError
    except ValueError:
        print_warning(t("common.invalid"))
        budget = cur_budget
    if budget < 0:
        budget = 0.0
    cost["daily_budget_usd"] = budget
    if budget <= 0:
        print_info(t("cost.budget_zero_hint"))

    print_success(t("cost.saved", budget=budget))


# ── Section registry ──────────────────────────────────────────────────────────

SETUP_SECTIONS: list[tuple[str, Callable[[dict], None]]] = [
    ("language", setup_language),
    ("model", setup_model),
    ("permissions", setup_permissions),
    ("terminal", setup_terminal),
    ("agent", setup_agent),
    ("tools", setup_tools),
    ("channel", setup_channels),
    ("gateway", setup_gateway),
    ("observability", setup_observability),
    ("evolution", setup_evolution),
    ("cost", setup_cost),
]

# Aliases so users can pass any reasonable section name from the CLI.
SECTION_ALIASES: dict[str, str] = {
    "lang": "language",
    "language": "language",
    "model": "model",
    "provider": "model",
    "permission": "permissions",
    "permissions": "permissions",
    "approval": "permissions",
    "sandbox": "terminal",
    "terminal": "terminal",
    "execution": "terminal",
    "agent": "agent",
    "behavior": "agent",
    "tool": "tools",
    "tools": "tools",
    "channel": "channel",
    "channels": "channel",
    "gateway": "gateway",
    "network": "gateway",
    "observability": "observability",
    "logging": "observability",
    "evolution": "evolution",
    "evolve": "evolution",
    "self-evolve": "evolution",
    "self_evolve": "evolution",
    "cost": "cost",
    "budget": "cost",
    "doctor": "doctor",
    "check": "doctor",
}

def _capability_check(config: dict) -> list[tuple[str, bool, str]]:
    """Return a list of (label, ok, hint) tuples summarising what's available."""
    checks: list[tuple[str, bool, str]] = []
    providers = (config.get("models", {}) or {}).get("providers", []) or []
    if providers and any(p.get("name") for p in providers):
        checks.append((t("doctor.model_ok"), True, ""))
    else:
        checks.append((t("doctor.model_missing"), False, ""))

    enabled_channels = [
        k for k, v in (config.get("channels", {}) or {}).items()
        if isinstance(v, dict) and v.get("enabled") and k != "cli"
    ]
    if enabled_channels:
        checks.append((t("doctor.channel_ok", n=len(enabled_channels)), True, ", ".join(enabled_channels)))
    else:
        checks.append((t("doctor.channel_missing"), False, ""))

    profile = (config.get("tools", {}) or {}).get("profile", "full")
    checks.append((t("doctor.tools_ok", profile=profile), True, ""))

    perm_mode = ((config.get("permissions", {}) or {}).get("approval", {}) or {}).get("mode", "smart")
    checks.append((t("doctor.perm_ok", mode=perm_mode), True, ""))

    sandbox = (config.get("execution", {}) or {}).get("default_executor") or \
              (config.get("execution", {}) or {}).get("defaultExecutor") or "sandbox"
    checks.append((t("doctor.sandbox_ok", backend=sandbox), True, ""))

    gw = config.get("gateway", {}) or {}
    if gw.get("enabled"):
        checks.append((t("doctor.gateway_on", host=gw.get("host", "0.0.0.0"), port=gw.get("port", 9000)), True, ""))
    else:
        checks.append((t("doctor.gateway_off"), False, ""))

    if (config.get("memory", {}) or {}).get("enabled", True):
        checks.append((t("doctor.memory_ok"), True, ""))
    else:
        checks.append((t("doctor.memory_off"), False, ""))

    if (config.get("knowledge", {}) or {}).get("enabled", True):
        checks.append((t("doctor.knowledge_ok"), True, ""))
    else:
        checks.append((t("doctor.knowledge_off"), False, ""))

    mcps = (config.get("tools", {}) or {}).get("mcp_servers") or (config.get("tools", {}) or {}).get("mcpServers") or {}
    if mcps:
        checks.append((t("doctor.mcp_ok", n=len(mcps)), True, ""))
    else:
        checks.append((t("doctor.mcp_off"), False, ""))

    obs = config.get("observability", {}) or {}
    checks.append((t("doctor.obs_ok", level=(obs.get("log_level") or obs.get("logLevel") or "INFO")), True, ""))
    if obs.get("otel_enabled"):
        endpoint = obs.get("otel_endpoint") or obs.get("otelEndpoint") or "?"
        checks.append((t("doctor.otel_on", endpoint=endpoint), True, ""))
    else:
        checks.append((t("doctor.otel_off"), False, ""))

    return checks


def setup_doctor(config: dict) -> None:
    _print_section_header("doctor")
    print_info(t("doctor.intro"))
    print()
    checks = _capability_check(config)
    ok_count = sum(1 for _, ok, _ in checks if ok)
    print_info(t("doctor.summary_count", ok=ok_count, total=len(checks)))
    print()
    for label, ok, extra in checks:
        mark = color("✓", Colors.GREEN) if ok else color("✗", Colors.RED)
        line = f"  {mark} {label}"
        if extra:
            line += color(f"  ({extra})", Colors.DIM)
        print(line)
    print()


# ── Credential key ──────────────────────────────────────────────────────────

def _resolve_workspace(config: dict) -> Path:
    """Resolve the workspace dir from config, anchoring relative paths at cwd.

    Mirrors the runtime's relative-path handling so setup writes artifacts
    (e.g. the credential key) to the same place the agent later reads them.
    """
    workspace_raw = config.get("workspace") or "~/.echo-agent"
    ws = Path(str(workspace_raw)).expanduser()
    if not ws.is_absolute():
        ws = (Path.cwd() / ws).resolve()
    return ws


def _ensure_credential_key(workspace: Path) -> None:
    """Generate the credential encryption key on first setup if absent.

    No-op when ECHO_AGENT_CREDENTIAL_KEY is set or the key file already exists.
    """
    import os

    from echo_agent.permissions.credential_key import KEY_FILENAME, resolve_or_create_key

    if os.environ.get("ECHO_AGENT_CREDENTIAL_KEY"):
        return
    key_file = Path(workspace) / KEY_FILENAME
    existed = key_file.exists()
    resolve_or_create_key(key_file)
    if not existed:
        print_success(t("credentials.key_generated", path=str(key_file)))
        print_warning(t("credentials.key_warning"))


# ── Summary ───────────────────────────────────────────────────────────────────

def _print_summary(config: dict, config_path: Path) -> None:
    print()
    print(color(f"  ◆ {t('summary.header')}", Colors.CYAN, Colors.BOLD))
    models = config.get("models", {}) or {}
    providers = models.get("providers", []) or []
    if providers:
        print_info(f"  {t('summary.provider')}: {providers[0].get('name', '?')}")
    print_info(f"  {t('summary.model')}: {models.get('defaultModel') or models.get('default_model') or '-'}")
    enabled = [k for k, v in (config.get("channels", {}) or {}).items()
               if isinstance(v, dict) and v.get("enabled") and k != "cli"]
    if enabled:
        print_info(f"  {t('summary.channels')}: {t('summary.channels_cli_plus', names=', '.join(enabled))}")
    else:
        print_info(f"  {t('summary.channels')}: {t('summary.channels_cli_only')}")
    print_info(f"  {t('summary.config_file')}: {config_path}")
    print()
    print(color(f"  {t('summary.next_steps')}", Colors.CYAN))
    print(color(t("summary.next_run"), Colors.GREEN))
    print(color(t("summary.next_setup"), Colors.GREEN))
    print(color(t("summary.next_status"), Colors.GREEN))
    print()


# ── Headless / non-interactive guidance ───────────────────────────────────────

def _print_headless_guidance() -> None:
    print()
    print(color(f"  ◆ {t('headless.guide_title')}", Colors.CYAN, Colors.BOLD))
    print()
    print_warning(t("headless.warning"))
    print_info(t("headless.guide_intro"))
    print_info(t("headless.guide_yaml"))
    print_info(t("headless.guide_env"))
    print_info(t("headless.guide_docs"))
    print()
    print_info(t("headless.guide_retry"))


# ── Main entry points ─────────────────────────────────────────────────────────

def _setup_config_target(config_path: str | Path | None = None, workspace: str | Path | None = None) -> Path | None:
    if config_path:
        return Path(config_path).expanduser()
    if workspace:
        ws = Path(workspace).expanduser()
        return find_local_config_file(ws) or ws / "echo-agent.yaml"
    return resolve_config_file() or default_config_path()


def _load_existing_config(config_path: str | Path | None, workspace: str | Path | None) -> tuple[dict, Path | None]:
    existing_file = resolve_config_file(config_path=config_path, search_dir=workspace)
    config: dict[str, Any] = {}
    if existing_file and existing_file.exists():
        import yaml
        with open(existing_file, encoding="utf-8") as f:
            config = yaml.safe_load(f) or {}
    return config, existing_file


def _resolve_initial_locale(config: dict, lang_override: str | None) -> str:
    """Pick the active locale, preferring CLI flag > saved pref > OS detection."""
    if lang_override:
        chosen = set_locale(lang_override)
        return chosen
    saved = (config.get("ui", {}) or {}).get("locale")
    if saved and saved != "auto":
        return set_locale(saved)
    return set_locale(detect_locale(saved))


def run_setup_wizard(
    section: str | None = None,
    config_path: str | Path | None = None,
    workspace: str | Path | None = None,
    lang: str | None = None,
    flow: str | None = None,
) -> None:
    """Run the interactive setup wizard.

    ``section``:  name (or alias) of a single section to configure.
    ``flow``:     ``"quickstart"`` or ``"full"``; bypasses the menu.
    ``lang``:     ``"zh"`` or ``"en"``; overrides auto-detection.
    """
    if not is_interactive():
        # Pick a locale even in headless mode so the guidance prints in the
        # user's language.
        config, _ = _load_existing_config(config_path, workspace)
        _resolve_initial_locale(config, lang)
        _print_headless_guidance()
        return

    config_target = _setup_config_target(config_path=config_path, workspace=workspace)
    config, existing_file = _load_existing_config(config_path, workspace)

    if workspace and (not config_target or not config_target.exists()):
        config["workspace"] = "."
    elif workspace and "workspace" not in config:
        config["workspace"] = "."

    _resolve_initial_locale(config, lang)
    config.setdefault("ui", {})["locale"] = get_locale()

    section_map = {key: func for key, func in SETUP_SECTIONS}
    if section:
        canonical = SECTION_ALIASES.get(section.lower(), section.lower())
        if canonical == "doctor":
            _print_banner()
            setup_doctor(config)
            return
        func = section_map.get(canonical)
        if func is None:
            print_error(f"Unknown section: {section}")
            print_info("Available: " + ", ".join(k for k, _ in SETUP_SECTIONS) + ", doctor")
            return
        _print_banner()
        func(config)
        path = save_config(config, config_target)
        label = t(f"section.{canonical}")
        print_success(t("summary.section_saved", label=label, path=path))
        return

    _print_banner()

    is_existing = bool(existing_file and (config.get("models", {}) or {}).get("providers"))

    if flow is None and is_existing:
        print()
        print_success(t("menu.existing_detected"))
        menu = [
            t("menu.existing_quick"),
            t("menu.existing_full"),
            *[t("menu.existing_section", label=t(f"section.{key}")) for key, _ in SETUP_SECTIONS],
            t("section.doctor"),
            t("menu.exit"),
        ]
        choice = prompt_choice(t("menu.existing_what"), menu)
        if choice == len(menu) - 1:
            print_info(t("menu.exit_hint"))
            return
        if choice == 0:
            flow = "quickstart"
        elif choice == 1:
            flow = "full"
        elif choice == len(menu) - 2:
            setup_doctor(config)
            return
        else:
            section_idx = choice - 2
            key, func = SETUP_SECTIONS[section_idx]
            func(config)
            path = save_config(config, config_target)
            print_success(t("summary.section_saved", label=t(f"section.{key}"), path=path))
            return

    if flow is None:
        idx = prompt_choice(t("menu.first_run"), [
            t("menu.first_run_quick"),
            t("menu.first_run_full"),
        ])
        flow = "quickstart" if idx == 0 else "full"

    if flow == "quickstart":
        setup_model(config)
        print()
        if prompt_yes_no(t("channels.configure_now"), default=False):
            setup_channels(config)
        path = save_config(config, config_target)
        _ensure_credential_key(_resolve_workspace(config))
        setup_doctor(config)
        _print_summary(config, path)
        print_success(t("summary.complete"))
        return

    for _key, func in SETUP_SECTIONS:
        func(config)

    path = save_config(config, config_target)
    _ensure_credential_key(_resolve_workspace(config))
    setup_doctor(config)
    _print_summary(config, path)
    print_success(t("summary.complete"))


def has_any_provider_configured(config_path: str | Path | None = None, workspace: str | Path | None = None) -> bool:
    config_file = resolve_config_file(config_path=config_path, search_dir=workspace)
    if not config_file or not config_file.exists():
        return False
    import yaml
    with open(config_file, encoding="utf-8") as f:
        data = yaml.safe_load(f) or {}
    providers = (data.get("models", {}) or {}).get("providers", []) or []
    return bool(providers)


def prompt_first_run_setup(
    config_path: str | Path | None = None,
    workspace: str | Path | None = None,
    lang: str | None = None,
) -> bool:
    if not is_interactive():
        return False
    if has_any_provider_configured(config_path=config_path, workspace=workspace):
        return False
    config_file = _setup_config_target(config_path=config_path, workspace=workspace)
    if config_file and config_file.exists():
        return False

    config, _ = _load_existing_config(config_path, workspace)
    _resolve_initial_locale(config, lang)

    print()
    print_warning(t("summary.first_run_no_config"))
    if prompt_yes_no(t("summary.first_run_prompt"), default=True):
        run_setup_wizard(config_path=config_path, workspace=workspace, lang=lang)
        return True
    print_info(t("summary.first_run_skip_msg"))
    print_info(t("summary.first_run_skip_hint"))
    return False

