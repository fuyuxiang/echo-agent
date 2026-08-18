"""Interactive setup wizard for Echo Agent.

The wizard is structured into twelve sections, each runnable independently
(order matches ``SETUP_SECTIONS`` below):

    1. Language          — auto-detected, overridable
    2. Model & Provider  — LLM provider + default model
    3. Permissions       — approval mode (smart/manual/off)
    4. Terminal / Sandbox — execution backend (local/sandbox/container/remote)
    5. Agent Behavior    — max_iterations / compression / session reset
    6. Tools             — profile + optional integrations (web/tts/mcp/...)
    7. Channels          — messaging integrations + allowlist
    8. Gateway           — Web/WS API exposure
    9. Security          — security.profile hardening
   10. Observability     — log level + OpenTelemetry export
   11. Evolution         — self-evolving skill harness (off by default)
   12. Cost              — daily budget cap and cost tracking

Followed by a Capability Check ("doctor") + summary.

Locale is auto-detected from the OS, but ``--lang`` overrides it and the
user-selected locale is persisted to ``ui.locale`` so subsequent runs are
consistent.
"""

from __future__ import annotations

import json
import math
from pathlib import Path
from typing import Any, Callable

from echo_agent.cli.colors import (
    Colors,
    color,
    set_color_override,
)
from echo_agent.cli import ui
from echo_agent.cli.health import FAIL, OK, WARN, run_health_checks
from echo_agent.cli.i18n import detect_locale, get_locale, set_locale, t
from echo_agent.cli.palette import ansi
from echo_agent.cli.prompt import (
    PromptAborted,
    is_interactive,
    prompt_yes_no,
)
from echo_agent.cli.runtime_probe import GatewayState, is_wsl, probe_gateway
from echo_agent.cli.service import run_service_action
from echo_agent.cli.setup import providers as provider_catalog
from echo_agent.cli.setup.model_verify import (
    VerifyResult,
    list_model_windows,
    list_models,
    verify_model,
)
from echo_agent.cli.setup.providers import find as find_provider, grouped_catalog
from echo_agent.cli.tui.brand import ECHO_LOGO_ART, ECHO_LOGO_GRADIENT, load_brand
from echo_agent.config.loader import find_local_config_file, resolve_config_file, save_config
from echo_agent.runtime_paths import default_config_path


# Keep the section implementations readable while routing all setup notices
# through the same glyphs and palette as the interactive prompt layer.
def print_info(message: str) -> None:
    ui.note(message, "info")


def print_success(message: str) -> None:
    ui.note(message, "success")


def print_warning(message: str) -> None:
    ui.note(message, "warning")


def print_error(message: str) -> None:
    ui.note(message, "error")


# ── Channel presets ────────────────────────────────────────────────────────────

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
    ("whatsapp", "WhatsApp", [("verify_token", "Webhook verify token"), ("access_token", "Cloud API access token"), ("phone_number_id", "Phone number ID")]),
    # webhook / cron 通道运行时也支持,但依赖路径/端口、调度表等结构化配置,
    # 不适配此处 key-value 字段循环,请用 `echo-agent config` 或直接编辑 YAML 配置。
]


# ── Banner & helpers ──────────────────────────────────────────────────────────

def _print_banner() -> None:
    brand = load_brand()
    subtitle = t("banner.subtitle")
    exit_hint = t("banner.exit_hint")
    print()
    if brand.name.lower() == "echo":
        for line, role in zip(ECHO_LOGO_ART, ECHO_LOGO_GRADIENT):
            print(color(f"  {line}", Colors.BOLD, ansi(role)))
    else:
        print(color(f"  {brand.name}", Colors.BOLD, ansi("primary")))
    print(color(f"  · {brand.tagline} · {t('banner.mode')}", ansi("text-muted")))
    print()
    print(f"  {subtitle}")
    print(color(f"  {exit_hint}", ansi("text-muted")))


def _print_section_header(key: str) -> None:
    label = t(f"section.{key}")
    ui.intro(label)


def _ensure_dict(parent: dict, key: str) -> dict:
    value = parent.get(key)
    if not isinstance(value, dict):
        value = {}
        parent[key] = value
    return value


def _choice(question: str, labels: list[str], default: int = 0) -> int:
    """Ask a single-choice question via ``ui.select`` and return the index.

    Wraps ``ui.select`` so the many ``prompt_choice(q, labels, default=i)``
    call-sites can migrate to the arrow-key UI without changing the int-index
    logic each section already relies on.
    """
    picked = ui.select(
        question,
        [(str(i), label, "") for i, label in enumerate(labels)],
        default=str(default),
    )
    return int(picked)


# ── Section 1: Language ───────────────────────────────────────────────────────

def setup_language(config: dict) -> None:
    _print_section_header("language")
    auto_label = t("language.english") if get_locale() == "en" else t("language.chinese")
    print_info(t("language.auto_detected", label=auto_label))
    choices = [t("language.english"), t("language.chinese")]
    default = 0 if get_locale() == "en" else 1
    idx = _choice(t("language.prompt"), choices, default=default)
    chosen = "en" if idx == 0 else "zh"
    set_locale(chosen)
    _ensure_dict(config, "ui")["locale"] = chosen
    print_success(t("language.saved"))


# ── Section 2: Model & Provider ──────────────────────────────────────────────

def setup_model(config: dict) -> None:
    _print_section_header("model")
    ui.note(t("model.intro"), "info")

    groups = [
        (t(f"provider.group.{gid}"),
         # No per-entry hint: provider selection is only about picking a vendor;
         # the very next step lists real models (fetched live), and hints blow
         # out the line width (truncating long entries like Bedrock).
         [(e.id, t_provider_label(e), "") for e in entries])
        for gid, entries in grouped_catalog()
    ]
    existing = ((config.get("models", {}) or {}).get("providers") or [{}])[0]
    default_id = _detect_catalog_id(existing)
    entry_id = ui.select_grouped(t("model.select_provider"), groups, default=default_id)
    entry = find_provider(entry_id)

    api_base = entry.api_base
    if entry.needs_api_base:
        api_base = ui.text(t("model.api_base"), default=existing.get("apiBase", "") or api_base)

    api_key = ""
    if entry.dialect != "bedrock":
        api_key = ui.password(f"{t_provider_label(entry)} {t('model.api_key')}")
        if not api_key and existing.get("apiKey") and _detect_catalog_id(existing) == entry_id:
            api_key = existing.get("apiKey", "")

    models = []
    model_windows: dict[str, int] = {}
    if entry.models_endpoint:
        with ui.spinner(t("model.fetching")):
            models = list_models(entry, api_key, api_base)
            # Capture per-model context windows from the same listing so the
            # runtime gauge/compression track the real window. Best-effort:
            # providers that omit it (e.g. OpenAI native) yield {} and the
            # built-in registry takes over.
            model_windows = list_model_windows(entry, api_key, api_base)
    if not models:
        models = list(entry.fallback_models)

    if models:
        choices = [(m, m, "") for m in models] + [("__custom__", t("model.model_custom"), "")]
        picked = ui.select(t("model.model_select"), choices, default=models[0])
        default_model = ui.text(t("model.model_name")) if picked == "__custom__" else picked
    else:
        default_model = ""
        while not default_model:
            default_model = ui.text(t("model.model_name"))

    if default_model and entry.dialect != "bedrock":
        with ui.spinner(t("model.verifying")):
            result = verify_model(entry.dialect, api_key, api_base, default_model)
        default_model, api_key = _handle_verify(result, entry, api_key, api_base, default_model)

    provider_entry: dict[str, Any] = {"name": entry.dialect}
    if api_key:
        provider_entry["apiKey"] = api_key
    if api_base:
        provider_entry["apiBase"] = api_base
    if entry.fallback_models and entry.dialect not in ("openai",):
        models_list = list(entry.fallback_models)
        if default_model and default_model not in models_list:
            models_list.append(default_model)
        provider_entry["models"] = models_list

    # Only update defaultModel/providers so any existing models.routes (and
    # other keys) survive a re-run of just the model section.
    models_block = _ensure_dict(config, "models")
    models_block["defaultModel"] = default_model
    models_block["providers"] = [provider_entry]
    # Merge captured windows into models.modelWindows (keep any prior entries
    # from other providers/re-runs). Only keep the models we actually offer plus
    # the picked one, to avoid bloating the config with the full catalog.
    if model_windows:
        keep = set(models) | ({default_model} if default_model else set())
        captured = {mid: win for mid, win in model_windows.items() if mid in keep}
        if captured:
            existing_windows = dict(models_block.get("modelWindows") or {})
            existing_windows.update(captured)
            models_block["modelWindows"] = existing_windows
    ui.note(t("model.saved", provider=t_provider_label(entry), model=default_model), "success")


def t_provider_label(entry) -> str:
    # Brand names are locale-neutral; only the generic "custom" entry needs
    # translating, so it goes through the i18n bundle.
    if entry.id == "custom":
        return t("provider.custom_label")
    return entry.label


def _detect_catalog_id(existing: dict) -> str:
    name = (existing.get("name") or "").lower()
    base = existing.get("apiBase") or ""
    for e in provider_catalog.CATALOG:
        if e.dialect == name and (not e.api_base or e.api_base == base):
            return e.id
    return "openai"


def _handle_verify(result: "VerifyResult", entry, api_key, api_base, model):
    if result.status == "ok":
        ui.note(t("model.verify_ok", model=model), "success")
        return model, api_key
    if result.status == "unreachable":
        ui.note(t("model.verify_unreachable"), "warning")
        return model, api_key
    # error: offer retry-key / change-model / skip
    ui.note(t("model.verify_error", detail=result.detail), "error")
    action = ui.select(t("model.verify_action"), [
        ("retry", t("model.verify_retry_key"), ""),
        ("change", t("model.verify_change_model"), ""),
        ("skip", t("model.verify_skip"), ""),
    ], default="retry")
    if action == "skip":
        return model, api_key
    if action == "retry":
        new_key = ui.password(f"{t_provider_label(entry)} {t('model.api_key')}")
        with ui.spinner(t("model.verifying")):
            res2 = verify_model(entry.dialect, new_key, api_base, model)
        return _handle_verify(res2, entry, new_key, api_base, model)
    # change model
    new_model = ui.text(t("model.model_name"))
    with ui.spinner(t("model.verifying")):
        res2 = verify_model(entry.dialect, api_key, api_base, new_model)
    return _handle_verify(res2, entry, api_key, api_base, new_model)


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
    mode_idx = _choice(t("permissions.mode_prompt"), mode_labels, default=default_mode_idx)
    chosen_mode = mode_keys[mode_idx]
    approval["mode"] = chosen_mode

    if chosen_mode == "smart":
        existing_smart_model = approval.get("smart_model", "") or approval.get("smartModel", "")
        smart_model = ui.text(t("permissions.smart_model"), default=existing_smart_model)
        if smart_model:
            approval["smart_model"] = smart_model
        else:
            approval.pop("smart_model", None)
            approval.pop("smartModel", None)

    unattended_keys = ["deny", "allow_safe"]
    unattended_labels = [t(f"permissions.unattended_{k}") for k in unattended_keys]
    current_unatt = approval.get("unattended_policy") or approval.get("unattendedPolicy") or "deny"
    default_unatt_idx = unattended_keys.index(current_unatt) if current_unatt in unattended_keys else 0
    unatt_idx = _choice(t("permissions.unattended"), unattended_labels, default=default_unatt_idx)
    approval["unattended_policy"] = unattended_keys[unatt_idx]

    cli_auto_default = approval.get("cli_auto_approve")
    if cli_auto_default is None:
        cli_auto_default = approval.get("cliAutoApprove", True)
    approval["cli_auto_approve"] = ui.confirm(t("permissions.cli_auto"), default=bool(cli_auto_default))

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
    idx = _choice(t("terminal.select"), backend_labels, default=default_idx)
    chosen = backend_keys[idx]
    execution["default_executor"] = chosen

    if chosen == "container":
        existing_image = execution.get("container_image") or execution.get("containerImage") or ""
        image = ui.text(t("terminal.container_image"), default=existing_image or "python:3.11-slim")
        execution["container_image"] = image
    elif chosen == "remote":
        execution["remote_host"] = ui.text(t("terminal.remote_host"), default=execution.get("remote_host", ""))
        execution["remote_user"] = ui.text(t("terminal.remote_user"), default=execution.get("remote_user", "root"))
        existing_key = execution.get("remote_key_path") or execution.get("remoteKeyPath") or ""
        execution["remote_key_path"] = ui.text(t("terminal.remote_key"), default=existing_key)

    network_keys = ["allow", "deny", "restricted"]
    network_labels = [t(f"terminal.network_{k}") for k in network_keys]
    current_net = execution.get("network_policy") or execution.get("networkPolicy") or "deny"
    default_net = network_keys.index(current_net) if current_net in network_keys else 1
    net_idx = _choice(t("terminal.network"), network_labels, default=default_net)
    execution["network_policy"] = network_keys[net_idx]

    tools = _ensure_dict(config, "tools")
    exec_cfg = _ensure_dict(tools, "exec")
    sec_keys = ["deny", "allowlist", "full"]
    sec_labels = [t(f"terminal.exec_security_{k}") for k in sec_keys]
    current_sec = exec_cfg.get("security", "allowlist")
    default_sec = sec_keys.index(current_sec) if current_sec in sec_keys else 1
    sec_idx = _choice(t("terminal.exec_security"), sec_labels, default=default_sec)
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
    raw = ui.text(t("agent.max_iter"), default=str(current_iter))
    try:
        agent["max_iterations"] = max(1, int(raw))
    except ValueError:
        print_warning(t("common.invalid"))
        agent["max_iterations"] = current_iter

    compression = _ensure_dict(config, "compression")
    compression["enabled"] = ui.confirm(t("agent.compression_enabled"), default=bool(compression.get("enabled", True)))
    if compression["enabled"]:
        current_thr = float(compression.get("trigger_ratio") or compression.get("triggerRatio") or 0.7)
        print_info(t("agent.compression_threshold_hint"))
        raw = ui.text(t("agent.compression_threshold"), default=f"{current_thr:.2f}")
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
    current_reset = sp.get("mode", "both")
    default_reset = reset_keys.index(current_reset) if current_reset in reset_keys else reset_keys.index("both")
    reset_idx = _choice(t("agent.session_reset"), reset_labels, default=default_reset)
    sp["mode"] = reset_keys[reset_idx]
    if reset_keys[reset_idx] in ("idle", "both"):
        cur_idle = int(sp.get("idleTimeoutMinutes") or sp.get("idle_timeout_minutes") or 1440)
        raw = ui.text(t("agent.idle_minutes"), default=str(cur_idle))
        try:
            sp["idleTimeoutMinutes"] = max(1, int(raw))
        except ValueError:
            sp["idleTimeoutMinutes"] = cur_idle
    if reset_keys[reset_idx] in ("daily", "both"):
        cur_hr = int(sp.get("dailyResetHour") or sp.get("daily_reset_hour") or 4)
        raw = ui.text(t("agent.daily_hour"), default=str(cur_hr))
        try:
            v = int(raw)
            sp["dailyResetHour"] = v if 0 <= v <= 23 else cur_hr
        except ValueError:
            sp["dailyResetHour"] = cur_hr

    planning = _ensure_dict(config, "planning")
    planning["enabled"] = ui.confirm(t("agent.planning_enabled"), default=bool(planning.get("enabled", True)))
    memory = _ensure_dict(config, "memory")
    memory["enabled"] = ui.confirm(t("agent.memory_enabled"), default=bool(memory.get("enabled", True)))

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
    p_idx = _choice(t("tools.profile"), profile_labels, default=default_profile)
    tools["profile"] = profile_keys[p_idx]

    pre_selected: list[int] = []
    if (tools.get("web", {}) or {}).get("enabled"):
        pre_selected.append(TOOL_OPTIONS.index("web"))
    image_block = tools.get("image_gen") or tools.get("imageGen") or {}
    # Respect an explicit enabled flag if present; otherwise infer from
    # whether credentials were ever entered (back-compat with older configs).
    image_on = image_block.get("enabled") if "enabled" in image_block else bool(
        image_block.get("api_key") or image_block.get("apiKey")
        or image_block.get("fal_key") or image_block.get("falKey")
    )
    if image_on:
        pre_selected.append(TOOL_OPTIONS.index("image_gen"))
    tts_block = tools.get("tts", {}) or {}
    tts_on = tts_block.get("enabled") if "enabled" in tts_block else bool(
        tts_block.get("openai_api_key") or tts_block.get("openaiApiKey")
        or tts_block.get("default_backend")
    )
    if tts_on:
        pre_selected.append(TOOL_OPTIONS.index("tts"))
    code_exec_block = tools.get("code_exec") or tools.get("codeExec") or {}
    if code_exec_block.get("enabled", True):
        pre_selected.append(TOOL_OPTIONS.index("code_exec"))
    if (config.get("knowledge", {}) or {}).get("enabled", True):
        pre_selected.append(TOOL_OPTIONS.index("knowledge"))
    if (config.get("scheduler", {}) or {}).get("enabled", True):
        pre_selected.append(TOOL_OPTIONS.index("cron"))
    mcp_block = tools.get("mcp") or {}
    mcp_on = mcp_block.get("enabled") if "enabled" in mcp_block else bool(
        tools.get("mcp_servers") or tools.get("mcpServers")
    )
    if mcp_on:
        pre_selected.append(TOOL_OPTIONS.index("mcp"))
    skills_block = config.get("skills", {}) or {}
    skills_on = skills_block.get("enabled") if "enabled" in skills_block else bool(
        skills_block.get("skills_dir") or skills_block.get("skillsDir")
    )
    if skills_on:
        pre_selected.append(TOOL_OPTIONS.index("skills"))
    if (config.get("plugins", {}) or {}).get("enabled", True):
        pre_selected.append(TOOL_OPTIONS.index("plugins"))
    pre_selected = sorted(set(pre_selected))

    labels = [t(f"tools.{k}") for k in TOOL_OPTIONS]
    selected_vals = ui.multiselect(
        t("tools.checklist"),
        [(str(i), label, "") for i, label in enumerate(labels)],
        preselected=[str(i) for i in pre_selected],
    )
    selected = [int(v) for v in selected_vals]
    chosen = {TOOL_OPTIONS[i] for i in selected}

    if "web" in chosen:
        web = _ensure_dict(tools, "web")
        web["enabled"] = True
        provider_choices = ["brave", "tavily", "serpapi", "searxng", "serply"]
        cur_prov = web.get("search_provider") or web.get("searchProvider") or "brave"
        prov_idx = _choice(t("tools.web_provider"), provider_choices,
                           default=provider_choices.index(cur_prov) if cur_prov in provider_choices else 0)
        web["search_provider"] = provider_choices[prov_idx]
        existing_key = web.get("search_api_key") or web.get("searchApiKey") or ""
        if existing_key:
            new_key = ui.password(f"{t('tools.web_api_key')} [****{t('common.saved')}]")
            if new_key:
                web["search_api_key"] = new_key
        else:
            new_key = ui.password(t("tools.web_api_key"))
            if new_key:
                web["search_api_key"] = new_key
    else:
        if "web" in tools and isinstance(tools["web"], dict):
            tools["web"]["enabled"] = False

    if "image_gen" in chosen:
        ig = _ensure_dict(tools, "image_gen")
        ig["enabled"] = True
        backend_options = [t("tools.image_backend_openai"), t("tools.image_backend_fal")]
        backend_values = ["openai", "fal"]
        cur_backend = ig.get("backend", "openai")
        b_idx = _choice(t("tools.image_backend"), backend_options,
                        default=backend_values.index(cur_backend) if cur_backend in backend_values else 0)
        ig["backend"] = backend_values[b_idx]

        if backend_values[b_idx] == "fal":
            existing = ig.get("fal_key") or ig.get("falKey") or ""
            if existing:
                new_key = ui.password(f"{t('tools.image_fal_key')} [****{t('common.saved')}]")
                if new_key:
                    ig["fal_key"] = new_key
            else:
                new_key = ui.password(t("tools.image_fal_key"))
                if new_key:
                    ig["fal_key"] = new_key
            ig["fal_model"] = ui.text(t("tools.image_fal_model"), default=ig.get("fal_model") or ig.get("falModel") or "fal-ai/flux/schnell")
        else:
            existing = ig.get("api_key") or ig.get("apiKey") or ""
            if existing:
                new_key = ui.password(f"{t('tools.image_api_key')} [****{t('common.saved')}]")
                if new_key:
                    ig["api_key"] = new_key
            else:
                new_key = ui.password(t("tools.image_api_key"))
                if new_key:
                    ig["api_key"] = new_key
            ig["api_base"] = ui.text(t("tools.image_api_base"), default=ig.get("api_base") or ig.get("apiBase") or "https://api.openai.com/v1")
            ig["model"] = ui.text(t("tools.image_model"), default=ig.get("model", "dall-e-3"))

    if "tts" in chosen:
        tts = _ensure_dict(tools, "tts")
        tts["enabled"] = True
        # 只列运行时(agent/tools/tts.py)真正实现的后端。曾提供 elevenlabs,
        # 但运行时无该实现,选中后会被静默降级为 Edge TTS,故移除。
        backends = ["edge", "openai"]
        cur_backend = tts.get("default_backend") or tts.get("defaultBackend") or "edge"
        b_idx = _choice(t("tools.tts_backend"), backends,
                        default=backends.index(cur_backend) if cur_backend in backends else 0)
        tts["default_backend"] = backends[b_idx]
        if backends[b_idx] == "openai":
            existing = tts.get("openai_api_key") or tts.get("openaiApiKey") or ""
            if existing:
                new_key = ui.password(f"{t('tools.tts_openai_key')} [****{t('common.saved')}]")
                if new_key:
                    tts["openai_api_key"] = new_key
            else:
                tts["openai_api_key"] = ui.password(t("tools.tts_openai_key"))
            tts["openai_api_base"] = ui.text(t("tools.tts_openai_base"), default=tts.get("openai_api_base", "https://api.openai.com/v1"))
            tts["model"] = ui.text(t("tools.tts_model"), default=tts.get("model", "tts-1"))

    code_exec = _ensure_dict(tools, "code_exec")
    code_exec["enabled"] = "code_exec" in chosen
    _ensure_dict(config, "knowledge")["enabled"] = "knowledge" in chosen
    _ensure_dict(config, "scheduler")["enabled"] = "cron" in chosen
    _ensure_dict(config, "plugins")["enabled"] = "plugins" in chosen

    # Symmetric real switches for the remaining tools. Previously image_gen
    # and tts were only touched when selected (un-checking left a stale
    # enabled/credentials behind), mcp was never persisted (print-only), and
    # skills had no write-back at all — all "fake" toggles. Now un-checking any
    # of them flips a real enabled=False in the config.
    #
    # image_gen / tts: only the enabled flag is flipped; api_key / fal_key /
    # backend are preserved so re-enabling later doesn't lose credentials.
    if "image_gen" not in chosen:
        _ensure_dict(tools, "image_gen")["enabled"] = False
    if "tts" not in chosen:
        _ensure_dict(tools, "tts")["enabled"] = False

    # mcp has no dedicated enabled field in the schema (only tools.mcp_servers),
    # so introduce tools.mcp.enabled as the explicit switch while keeping the
    # "no servers configured" hint.
    mcp_block = _ensure_dict(tools, "mcp")
    mcp_block["enabled"] = "mcp" in chosen
    if "mcp" in chosen and not (tools.get("mcp_servers") or tools.get("mcpServers")):
        print_info(t("tools.mcp_skip_hint"))

    # skills config lives at the top-level `skills` section (SkillsConfig);
    # add an explicit enabled switch there so un-checking really disables it.
    _ensure_dict(config, "skills")["enabled"] = "skills" in chosen

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
    selected_vals = ui.multiselect(
        t("channels.checklist"),
        [(str(i), name, "") for i, name in enumerate(channel_names)],
        preselected=[str(i) for i in pre_selected],
    )
    selected = [int(v) for v in selected_vals]
    selected_keys = {CHANNEL_DEFS[idx][0] for idx in selected}

    # Symmetric persistence: every candidate channel gets an explicit
    # enabled flag written. Un-checking a channel must flip its persisted
    # enabled:true → false, otherwise the UI shows it off while the service
    # keeps listening. Empty selection is handled here too (all → False)
    # rather than early-returning without writing anything. CLI is the
    # implicit default channel (not in CHANNEL_DEFS) and is left untouched.
    for ch_key, _label, _fields in CHANNEL_DEFS:
        if ch_key not in selected_keys:
            _ensure_dict(existing, ch_key)["enabled"] = False

    if not selected:
        print_info(t("channels.no_extra"))
        return

    for idx in selected:
        ch_key, ch_label, fields = CHANNEL_DEFS[idx]
        print()
        print(color(f"  ● {t('channels.config_for', label=ch_label)}", ansi("secondary")))
        ch = _ensure_dict(existing, ch_key)
        ch["enabled"] = True

        if ch_key == "weixin":
            _setup_weixin_qr(ch)
            continue

        for field_key, field_label in fields:
            secret = any(s in field_key.lower() for s in ("key", "secret", "token", "password"))
            if secret:
                value = ui.password(field_label)
            else:
                value = ui.text(field_label, default=ch.get(field_key, ""))
            if value:
                ch[field_key] = value

        if ch_key in ("telegram", "discord", "slack", "qqbot", "email", "weixin", "dingtalk"):
            existing_allow = ch.get("allow_from") or ch.get("allowFrom") or []
            allow_default = ",".join(existing_allow) if isinstance(existing_allow, list) else str(existing_allow or "")
            allow_raw = ui.text(t("channels.allow_from"), default=allow_default)
            if allow_raw:
                ch["allow_from"] = [s.strip() for s in allow_raw.split(",") if s.strip()]
            else:
                ch.pop("allow_from", None)
                ch.pop("allowFrom", None)
                print_warning(t("channels.allow_warn"))

            home_existing = ch.get("home_channel") or ch.get("homeChannel") or ""
            home = ui.text(t("channels.home_channel"), default=home_existing)
            if home:
                ch["home_channel"] = home

    print_success(t("channels.saved", n=len(selected)))


# ── Section 8: Gateway ────────────────────────────────────────────────────────

def setup_gateway(config: dict) -> None:
    _print_section_header("gateway")
    print_info(t("gateway.intro"))
    print()

    gw = _ensure_dict(config, "gateway")
    gw["enabled"] = ui.confirm(t("gateway.enable"), default=bool(gw.get("enabled", False)))
    if not gw["enabled"]:
        return

    # Offer the schema's loopback default rather than 0.0.0.0: accepting every
    # prompt must not produce a network-exposed gateway (and, with no token set,
    # one that _check_bind_safety refuses to start).
    gw["host"] = ui.text(t("gateway.host"), default=str(gw.get("host", "127.0.0.1")))
    port_str = ui.text(t("gateway.port"), default=str(gw.get("port", 58123)))
    try:
        gw["port"] = int(port_str)
    except ValueError:
        gw["port"] = 58123
        print_warning(t("common.invalid"))

    auth = _ensure_dict(gw, "auth")
    auth_keys = ["open", "allowlist", "pairing"]
    auth_labels = [t(f"gateway.auth_{k}") for k in auth_keys]
    cur_auth = auth.get("mode", "allowlist")
    default_auth = auth_keys.index(cur_auth) if cur_auth in auth_keys else 1
    a_idx = _choice(t("gateway.auth_mode"), auth_labels, default=default_auth)
    auth["mode"] = auth_keys[a_idx]

    # An "open" (no-token) gateway bound to a non-loopback host is refused at
    # startup by gateway/server.py:_check_bind_safety, which would leave the
    # whole service unable to boot. Catch the combo here rather than letting it
    # fail after save.
    host_norm = str(gw["host"]).strip()
    if auth_keys[a_idx] == "open" and host_norm not in ("127.0.0.1", "localhost", "::1", ""):
        print_warning(t("gateway.open_exposed_warn", host=host_norm))
        if ui.confirm(t("gateway.open_exposed_fix"), default=True):
            gw["host"] = "127.0.0.1"
            print_info(t("gateway.host_pinned"))
        else:
            # Keep the exposed host but force a token-bearing mode.
            a_idx = auth_keys.index("allowlist")
            auth["mode"] = "allowlist"

    if auth_keys[a_idx] in ("allowlist", "pairing"):
        existing_tokens = auth.get("api_tokens") or auth.get("apiTokens") or []
        token_default = existing_tokens[0] if existing_tokens else ""
        token = ui.password(t("gateway.api_token")) or token_default
        if not token:
            import secrets
            token = secrets.token_urlsafe(32)
            print_info(f"Generated token: {token}")
        auth["api_tokens"] = [token]
    else:
        # open 模式:清理遗留的 api_tokens(含驼峰键),否则 server 只要发现
        # token 列表非空就仍要求携带 token,与"Open — no auth"提示矛盾。
        removed = bool(auth.get("api_tokens") or auth.get("apiTokens"))
        auth.pop("api_tokens", None)
        auth.pop("apiTokens", None)
        if removed:
            print_info(t("gateway.open_tokens_cleared"))

    print_success(t("gateway.saved", host=gw["host"], port=gw["port"], mode=t(f"gateway.auth_{auth_keys[a_idx]}")))


# ── Section 9: Security profile ──────────────────────────────────────────────

def setup_security(config: dict) -> None:
    """Pin ``security.profile`` explicitly.

    Without an explicit profile the gateway entrypoint (``echo-agent gateway``,
    which is also how the installed service runs) silently tightens to
    ``public_gateway`` and disables high-risk tools (exec/write_file/execute_code)
    even when ``tools.profile: full`` is set — the failure mode that made a whole
    document-generation task come back empty. Writing the key here makes the
    choice visible and stops the implicit downgrade
    (see ``config.loader.profile_explicitly_set``)."""
    _print_section_header("security")
    print_info(t("security.intro"))
    print()

    sec = _ensure_dict(config, "security")
    gateway_enabled = bool((config.get("gateway", {}) or {}).get("enabled"))

    if not gateway_enabled:
        # No public entrypoint — full local tools are the sane default. Still
        # write the key so the value is explicit and stable across reinstalls.
        sec["profile"] = "personal_cli"
        print_info(t("security.no_gateway"))
        print_success(t("security.saved", profile=sec["profile"]))
        return

    print_info(t("security.gateway_intro"))
    deploy_keys = ["personal_cli", "public_gateway"]
    deploy_labels = [t("security.deploy_personal"), t("security.deploy_public")]
    cur = sec.get("profile", "personal_cli")
    # Default highlight is personal_cli (index 0) — the common self-hosted case.
    default_idx = deploy_keys.index(cur) if cur in deploy_keys else 0
    d_idx = _choice(t("security.deployment"), deploy_labels, default=default_idx)
    sec["profile"] = deploy_keys[d_idx]

    if sec["profile"] == "public_gateway":
        print_warning(t("security.public_hint"))
    else:
        print_info(t("security.personal_hint"))

    print_success(t("security.saved", profile=sec["profile"]))


# ── Section 9: Observability ─────────────────────────────────────────────────

def setup_observability(config: dict) -> None:
    _print_section_header("observability")
    print_info(t("observability.intro"))
    print()

    obs = _ensure_dict(config, "observability")
    log_choices = ["INFO", "DEBUG", "WARNING", "ERROR"]
    cur_level = (obs.get("log_level") or obs.get("logLevel") or "INFO").upper()
    default_log = log_choices.index(cur_level) if cur_level in log_choices else 0
    l_idx = _choice(t("observability.log_level"), log_choices, default=default_log)
    obs["log_level"] = log_choices[l_idx]

    obs["trace_enabled"] = ui.confirm(t("observability.trace"), default=bool(obs.get("trace_enabled", True)))

    otel_on = ui.confirm(t("observability.otel"), default=bool(obs.get("otel_enabled", False)))
    obs["otel_enabled"] = otel_on
    if otel_on:
        obs["otel_endpoint"] = ui.text(t("observability.otel_endpoint"),
                                       default=obs.get("otel_endpoint") or obs.get("otelEndpoint") or "http://localhost:4317")
        obs["otel_service_name"] = ui.text(t("observability.otel_service"),
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

    enabled = ui.confirm(
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
    t_idx = _choice(t("evolution.trigger"), trigger_labels, default=default_trigger)
    trigger = _EVOLUTION_TRIGGER_KEYS[t_idx]
    evo["trigger_mode"] = trigger

    if trigger == "threshold":
        cur = int(evo.get("threshold_trajectories") or evo.get("thresholdTrajectories") or 50)
        raw = ui.text(t("evolution.threshold"), default=str(cur))
        try:
            evo["threshold_trajectories"] = max(1, int(raw))
        except ValueError:
            print_warning(t("common.invalid"))
            evo["threshold_trajectories"] = cur
    elif trigger == "scheduled":
        cur = evo.get("cron_expression") or evo.get("cronExpression") or "0 4 * * *"
        raw = ui.text(t("evolution.cron"), default=cur)
        evo["cron_expression"] = raw or cur

    # Eval dataset path
    cur_dataset = evo.get("eval_dataset_path") or evo.get("evalDatasetPath") or "data/eval/baseline.yaml"
    raw = ui.text(t("evolution.dataset_path"), default=cur_dataset)
    evo["eval_dataset_path"] = raw or cur_dataset

    # Strict / regression policy
    evo["require_strict_improvement"] = ui.confirm(
        t("evolution.strict"),
        default=bool(evo.get("require_strict_improvement", True)),
    )
    cur_thr = float(evo.get("regression_threshold") or evo.get("regressionThreshold") or 0.05)
    print_info(t("evolution.regression_hint"))
    raw = ui.text(t("evolution.regression"), default=f"{cur_thr:.2f}")
    try:
        v = float(raw)
        if 0.0 <= v <= 0.5:
            evo["regression_threshold"] = v
        else:
            print_warning(t("common.invalid"))
    except ValueError:
        print_warning(t("common.invalid"))

    # Operational knobs
    evo["candidate_review_required"] = ui.confirm(
        t("evolution.review_required"),
        default=bool(evo.get("candidate_review_required", False)),
    )
    cur_cand = int(evo.get("max_candidates_per_run") or evo.get("maxCandidatesPerRun") or 3)
    raw = ui.text(t("evolution.max_candidates"), default=str(cur_cand))
    try:
        evo["max_candidates_per_run"] = max(1, int(raw))
    except ValueError:
        evo["max_candidates_per_run"] = cur_cand

    cur_retain = int(evo.get("trajectory_retention_days") or evo.get("trajectoryRetentionDays") or 30)
    raw = ui.text(t("evolution.retention_days"), default=str(cur_retain))
    try:
        evo["trajectory_retention_days"] = max(0, int(raw))
    except ValueError:
        evo["trajectory_retention_days"] = cur_retain

    evo["redact_args"] = ui.confirm(
        t("evolution.redact_args"),
        default=bool(evo.get("redact_args", True)),
    )
    evo["record_trajectories"] = ui.confirm(
        t("evolution.record"),
        default=bool(evo.get("record_trajectories", True)),
    )

    # Eval execution knobs (used by PromotionGate)
    cur_par = int(evo.get("eval_parallel") or evo.get("evalParallel") or 2)
    raw = ui.text(t("evolution.eval_parallel"), default=str(cur_par))
    try:
        evo["eval_parallel"] = max(1, int(raw))
    except ValueError:
        evo["eval_parallel"] = cur_par

    cur_to = int(evo.get("eval_timeout_seconds") or evo.get("evalTimeoutSeconds") or 60)
    raw = ui.text(t("evolution.eval_timeout"), default=str(cur_to))
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
    enabled = ui.confirm(t("cost.enable"), default=bool(cost.get("enabled", False)))
    cost["enabled"] = enabled
    if not enabled:
        print_success(t("cost.saved_disabled"))
        return

    cur_budget = float(cost.get("daily_budget_usd") or cost.get("dailyBudgetUsd") or 0.0)
    raw = ui.text(t("cost.daily_budget"), default=f"{cur_budget:.2f}")
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
    ("security", setup_security),
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
    "security": "security",
    "profile": "security",
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

def section_names() -> list[str]:
    """Every section name ``setup <section>`` accepts, in wizard order.

    ``doctor`` is appended because it is dispatched separately (read-only) and
    is therefore not part of ``SETUP_SECTIONS``. ``__main__`` renders its
    ``--help`` from this so the advertised list can never drift from the
    implemented one again.
    """
    return [key for key, _ in SETUP_SECTIONS] + ["doctor"]


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
        # Read the live state, not just the YAML: this used to print a ✓ for a
        # gateway nobody was serving, on the very screen users rely on to decide
        # whether their setup worked.
        rt = probe_gateway(config=_probe_config(config))
        if rt.state is GatewayState.RUNNING:
            checks.append((t("doctor.gateway_on", host=rt.probe_host, port=rt.effective_port), True, ""))
        else:
            checks.append((t("doctor.gateway_enabled_not_running"), False,
                           t("doctor.gateway_not_running_hint")))
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


def _doctor_mark(status: str) -> str:
    """Map a health status to a coloured glyph (ok/warn/fail → ✓/!/✗)."""
    if status == OK:
        return color("✓", ansi("success"))
    if status == WARN:
        return color("!", ansi("warning"))
    return color("✗", ansi("error"))


def doctor_report(config: dict) -> dict:
    """Structured doctor result: live probes plus the config echo.

    Shared by the interactive rendering and ``setup doctor --json`` so both
    report the exact same findings.
    """
    probes = run_health_checks(config)
    return {
        "ok": all(p["status"] != FAIL for p in probes),
        "probes": [
            {
                "name": p["name"],
                "status": p["status"],
                "detail": p.get("detail") or None,
            }
            for p in probes
        ],
        "ok_count": sum(1 for p in probes if p["status"] == OK),
        "total": len(probes),
        "config_echo": [
            {"label": label, "ok": bool(ok), "detail": extra or None}
            for label, ok, extra in _capability_check(config)
        ],
    }


def setup_doctor(config: dict) -> None:
    _print_section_header("doctor")
    print_info(t("doctor.intro"))
    print()

    # Real environment probes (ports, paths, credentials, PATH binaries).
    # These replace the old hard-coded "OK"s with actual detection.
    probes = run_health_checks(config)
    ok_count = sum(1 for p in probes if p["status"] == OK)
    print_info(t("doctor.summary_count", ok=ok_count, total=len(probes)))
    print()
    for probe in probes:
        mark = _doctor_mark(probe["status"])
        line = f"  {mark} {probe['name']}"
        if probe.get("detail"):
            line += color(f"  ({probe['detail']})", ansi("text-muted"))
        print(line)
    print()

    # Retained config echo: a quick read-out of what the config declares
    # (profile / approval mode / channels / observability), distinct from the
    # live probes above.
    checks = _capability_check(config)
    for label, ok, extra in checks:
        mark = color("✓", ansi("success")) if ok else color("✗", ansi("error"))
        line = f"  {mark} {label}"
        if extra:
            line += color(f"  ({extra})", ansi("text-muted"))
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
    ui.intro(t("summary.header"))
    models = config.get("models", {}) or {}
    providers = models.get("providers", []) or []
    model_name = models.get("defaultModel") or models.get("default_model") or "-"
    prov_name = providers[0].get("name", "?") if providers else "?"
    print_info(f"{t('summary.model')}: {prov_name} · {model_name}")

    enabled = [k for k, v in (config.get("channels", {}) or {}).items()
               if isinstance(v, dict) and v.get("enabled") and k != "cli"]
    if enabled:
        print_info(f"{t('summary.channels')}: {', '.join(enabled)}")
    else:
        print_info(f"{t('summary.channels')}: {t('summary.channels_cli_only')}  → echo-agent setup channel")

    # No gateway next-step line here: it used to appear only when the gateway was
    # *disabled*, which is exactly the case that needs no `gateway install`, and
    # stayed silent for the enabled-but-not-running case that does. The startup
    # handoff (_offer_gateway_start, called right after this) reads the live state
    # and gives the one command that applies.
    print_info(f"{t('summary.config_file')}: {config_path}")
    print()
    print(color(f"  {t('summary.next_steps')}", Colors.BOLD, ansi("primary")))
    print(color(t("summary.next_run"), ansi("secondary")))
    print(color(t("summary.next_setup"), ansi("secondary")))
    print(color(t("summary.next_status"), ansi("secondary")))
    print()


# ── Startup handoff ───────────────────────────────────────────────────────────

_START_TIMEOUT_SECONDS = 15.0
"""How long to wait for the port after a successful `start`.

Bootstrap loads the embedding model, so a cold start is not instant — hence a
generous ceiling rather than a single check.
"""


def _as_str(path: Any) -> str | None:
    return str(path) if path else None


def _probe_config(config: dict) -> Any:
    """Adapt the wizard's plain dict to the attribute access the probe expects.

    The wizard works on raw YAML dicts (it must write keys the schema may not
    know yet), while the probe reads config.gateway.*. Building a tiny view is
    cheaper and safer than round-tripping the dict through the pydantic schema
    mid-wizard, which would reject a half-configured file.

    Every field is coerced defensively: the config may have been hand-edited
    (``port: abc``), and the probe's own tolerance does not help here because
    this view is built *before* the call, outside its never-raises guard.
    """
    from types import SimpleNamespace

    gw = config.get("gateway", {})
    if not isinstance(gw, dict):
        gw = {}
    try:
        port = int(gw.get("port", 58123))
    except (TypeError, ValueError):
        port = 0
    return SimpleNamespace(
        gateway=SimpleNamespace(
            enabled=bool(gw.get("enabled", False)),
            # Mirrors the schema default so the probe targets the same address
            # the gateway will actually bind when the key is absent.
            host=str(gw.get("host", "127.0.0.1")),
            port=port,
        ),
        workspace=str(config.get("workspace") or "."),
    )


def _wait_until_listening(
    config: dict, config_path: Any, workspace: str | None,
    timeout: float = _START_TIMEOUT_SECONDS,
) -> bool:
    """Poll until the gateway port actually accepts a connection.

    A service manager's `start` returning 0 only means the fork succeeded. The
    agent can still die during bootstrap (bad API key, port taken) and the
    wizard would have reported success for a gateway nobody is serving.
    """
    import time

    from echo_agent.cli.runtime_probe import GatewayState

    deadline = time.monotonic() + timeout
    while True:
        rt = probe_gateway(config=_probe_config(config), config_path=_as_str(config_path),
                           workspace=workspace)
        if rt.state is GatewayState.RUNNING:
            return True
        if time.monotonic() >= deadline:
            return False
        time.sleep(0.5)


def _service_action(action: str, workspace: str | None, config_path: Any) -> int:
    """Run one service action, converting a hard exit into a return code.

    The backends call ``sys.exit`` on a failing ``systemctl`` and raise
    ``SystemExit`` when the unit is missing. That is right for
    ``echo-agent gateway start``, whose only job is that action, but here the
    config is already saved: a service that will not start is a result to report,
    not a reason to kill the wizard before it prints anything.
    """
    try:
        return int(run_service_action(action, workspace=workspace, config=_as_str(config_path)) or 0)
    except SystemExit as exc:
        return exc.code if isinstance(exc.code, int) else 1
    except Exception:  # noqa: BLE001 - a broken service manager must not lose the summary
        return 1


def _installer_owns_service_registration() -> bool:
    """True when install.sh will register the service itself after the wizard.

    Only when running as root: install.sh then registers a *system* unit, while
    this wizard can only register a user-scope one. Registering both would leave
    two units competing for port 58123 and the workspace lock. As a normal user
    the installer's scope matches the wizard's, so the wizard goes ahead and
    install.sh detects the unit and skips its own prompt.
    """
    import os
    import sys

    if os.environ.get("ECHO_AGENT_SETUP_HANDLES_SERVICE") != "1":
        return False
    return sys.platform == "linux" and os.geteuid() == 0


def _print_linger_hint_if_needed() -> None:
    """A Linux user-scope service dies with the login session unless lingering
    is enabled — surprising for something meant to run 24/7."""
    import os
    import subprocess
    import sys

    if sys.platform != "linux" or os.geteuid() == 0:
        return
    try:
        user = os.getlogin()
    except OSError:
        user = os.environ.get("USER", "") or ""
    if not user:
        return
    try:
        out = subprocess.run(
            ["loginctl", "show-user", user], capture_output=True, text=True, timeout=5,
        ).stdout
    except (OSError, subprocess.SubprocessError):
        return
    if "Linger=yes" in out:
        return
    print_warning(t("startup.linger_warn"))
    print_info(t("startup.linger_fix", user=user))


def _report_started(config: dict, config_path: Any, workspace: str | None) -> None:
    print_info(t("startup.starting"))
    if _wait_until_listening(config, config_path, workspace):
        rt = probe_gateway(config=_probe_config(config), config_path=_as_str(config_path),
                           workspace=workspace)
        print_success(t("startup.start_ok", host=rt.probe_host, port=rt.effective_port))
    else:
        print_warning(t("startup.start_timeout", seconds=int(_START_TIMEOUT_SECONDS)))
        print_info(t("startup.start_timeout_hint"))


def _report_start_failed() -> None:
    print_warning(t("startup.start_failed"))
    print_info(t("startup.start_timeout_hint"))


def _maybe_offer_dashboard_build() -> None:
    """Offer to build the SPA before a background service takes over.

    The service process itself never builds (it must not block on pnpm), so if we
    do not ask here a fresh install would never build the Dashboard at all: the
    installer no longer builds it either.
    """
    try:
        from echo_agent.gateway.dashboard_build import (
            BuildReason,
            build_dashboard,
            dashboard_build_needed,
            describe_outcome,
            find_web_dir,
        )
    except Exception:  # noqa: BLE001 - the frontend is optional
        return

    web_dir = find_web_dir()
    if web_dir is None or not dashboard_build_needed(web_dir):
        return
    if not ui.confirm(t("dashboard.ask_build"), default=False):
        print_info(t("dashboard.declined"))
        return
    print_info(t("dashboard.building"))
    outcome = build_dashboard(
        web_dir,
        on_output=lambda line: print(f"    {line}", flush=True),
        confirm=lambda msg: ui.confirm(msg, default=True),
    )
    # BuildOutcome 没有 ok 字段(拆成了 build_succeeded / artifact_usable),此处沿用
    # describe_outcome 的判据:只有 reason 为 OK 才是纯成功。artifact_usable=True 但
    # 构建失败时,消息是"继续使用上一次的产物",那属于警告而不是成功。
    succeeded = outcome.reason is BuildReason.OK
    (print_success if succeeded else print_warning)(describe_outcome(outcome))


def _unstartable_reason(config: dict) -> str:
    """Why the saved gateway config cannot start, or "" when it can.

    Mirrors ``gateway/server.py:_check_bind_safety``: a non-loopback bind with no
    token of any kind is refused at startup. Checking it here turns a service
    that fails on every start into an explanation at the one moment the user can
    still act on it — quickstart never visits the gateway section, so nothing
    else in that flow would notice.

    Kept as a *duplicate* of the server's rule rather than a call into it,
    because the wizard holds a raw YAML dict (possibly hand-edited, possibly not
    schema-valid yet) and building a GatewayServer to ask would mean standing up
    a bus, session manager and workspace mid-wizard. The pairing is pinned by a
    test that fails if the two rules drift.
    """
    gw = config.get("gateway", {})
    if not isinstance(gw, dict) or not gw.get("enabled"):
        return ""
    host = str(gw.get("host", "127.0.0.1")).strip()
    if host in ("127.0.0.1", "localhost", "::1", ""):
        return ""
    auth = gw.get("auth", {})
    if not isinstance(auth, dict):
        auth = {}
    has_token = bool(
        auth.get("api_tokens") or auth.get("apiTokens")
        or auth.get("admin_tokens") or auth.get("adminTokens")
    )
    if has_token:
        return ""
    return host


def _offer_gateway_start(
    config: dict, config_path: Any, workspace: Any = None, *, section_only: bool = False
) -> None:
    """Close the gap between "config saved" and "the product works".

    The wizard only ever wrote YAML; nothing started the process that serves
    58123, and nothing said so. This runs after the summary and, per the live
    runtime state, either starts the gateway or hands the user the one command
    that applies to their platform.

    ``section_only``: the user reconfigured a single section rather than doing a
    first-time install. Never offer to install a service in that case — just say
    whether the change is live yet.
    """
    from echo_agent.cli.runtime_probe import GatewayState

    if not is_interactive():
        return

    # Refuse to register a unit that provably cannot boot. Doing this before the
    # probe (rather than letting `start` fail and reporting it) means the user
    # gets the actual cause and the fix, not a generic "could not start".
    bad_host = _unstartable_reason(config)
    if bad_host:
        print()
        print_warning(t("startup.unstartable", host=bad_host))
        print_info(t("startup.unstartable_fix"))
        return

    ws = _as_str(workspace)
    rt = probe_gateway(config=_probe_config(config), config_path=_as_str(config_path),
                       workspace=ws)

    if rt.state is GatewayState.DISABLED:
        print_info(t("startup.disabled"))
        print_info(t("startup.disabled_fix"))
        return

    print()
    ui.intro(t("startup.header"))

    if rt.state is GatewayState.RUNNING:
        print_success(t("startup.running", host=rt.probe_host, port=rt.effective_port))
        if section_only:
            print_info(t("startup.restart_needed"))
        return

    if rt.state is GatewayState.NO_SERVICE_MANAGER:
        # Deliberately no prompt and no action: a nohup'd gateway the wizard
        # spawned would be unsupervised, would not restart on crash or reboot,
        # and would collide with the workspace single-instance lock later.
        print_warning(t("startup.no_manager"))
        if is_wsl():
            print_info(t("startup.no_manager_wsl"))
        print_info(t("startup.no_manager_tmux"))
        return

    if section_only:
        # The next command differs by state: with no unit registered, a bare
        # `gateway start` exits 1 and tells the user to run `install` first, so
        # pointing there would send them down a command that cannot work.
        if rt.state is GatewayState.NOT_INSTALLED:
            print_warning(t("startup.not_installed_hint"))
        else:
            print_warning(t("startup.not_running"))
        return

    _maybe_offer_dashboard_build()

    if rt.state is GatewayState.SERVICE_INSTALLED_STOPPED:
        if not ui.confirm(t("startup.ask_start"), default=True):
            print_info(t("startup.declined_start"))
            return
        # This state also covers "the manager calls the unit active but the port
        # is dead" — a unit that forked and then died in bootstrap. `start` on an
        # already-active unit is a no-op that would burn the whole poll window
        # and still fail, so restart it instead; only a genuinely stopped unit
        # gets `start`.
        action = "restart" if rt.service_running else "start"
        if _service_action(action, ws, config_path) != 0:
            _report_start_failed()
            return
        _report_started(config, config_path, ws)
        return

    # NOT_INSTALLED
    if _installer_owns_service_registration():
        # install.sh running as root registers a *system* unit; this wizard only
        # ever registers a user-scope one. Offering here would leave the machine
        # with two units for one gateway, so defer to the installer.
        print_info(t("startup.declined"))
        return
    if not ui.confirm(t("startup.ask_install"), default=True):
        print_info(t("startup.declined"))
        return
    if _service_action("install", ws, config_path) != 0:
        print_warning(t("startup.install_failed"))
        return
    if _service_action("start", ws, config_path) != 0:
        _report_start_failed()
        return
    _report_started(config, config_path, ws)
    _print_linger_hint_if_needed()


# ── Headless / non-interactive guidance ───────────────────────────────────────

def _print_headless_guidance() -> None:
    ui.intro(t("headless.guide_title"))
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
    as_json: bool = False,
) -> int:
    """Run the setup wizard, treating Ctrl-C / EOF as a clean cancellation.

    The prompt helpers raise PromptAborted rather than calling sys.exit(0)
    themselves, so that a command whose work did NOT happen can report a
    non-zero code. The wizard is the case where aborting genuinely is a normal
    user action, so it keeps the old exit-0 behaviour — just decided here rather
    than deep inside the input helper."""
    try:
        return _run_setup_wizard(
            section=section, config_path=config_path, workspace=workspace,
            lang=lang, flow=flow, as_json=as_json,
        )
    except PromptAborted:
        return 0


def _run_setup_wizard(
    section: str | None = None,
    config_path: str | Path | None = None,
    workspace: str | Path | None = None,
    lang: str | None = None,
    flow: str | None = None,
    as_json: bool = False,
) -> int:
    """Run the interactive setup wizard and return a process exit code.

    ``section``:  name (or alias) of a single section to configure.
    ``flow``:     ``"quickstart"`` or ``"full"``; bypasses the menu.
    ``lang``:     ``"zh"`` or ``"en"``; overrides auto-detection.
    ``as_json``:  only meaningful for the read-only ``doctor`` section — emits
                  the structured report with ANSI off and skips the TTY
                  requirement (nothing is prompted or written). Any other
                  section rejects it rather than pretending to be scriptable.
    """
    if as_json:
        config, _ = _load_existing_config(config_path, workspace)
        _resolve_initial_locale(config, lang)
        canonical = SECTION_ALIASES.get((section or "").lower(), (section or "").lower())
        if canonical != "doctor":
            print(json.dumps({
                "ok": False,
                "error": "--json is only supported for the read-only 'doctor' section",
                "hint": "echo-agent setup doctor --json",
            }, ensure_ascii=False, indent=2))
            return 2
        set_color_override(False)
        try:
            report = doctor_report(config)
            print(json.dumps(report, ensure_ascii=False, indent=2))
        finally:
            set_color_override(None)
        return 0 if report["ok"] else 1

    if not is_interactive():
        # Pick a locale even in headless mode so the guidance prints in the
        # user's language.
        config, _ = _load_existing_config(config_path, workspace)
        _resolve_initial_locale(config, lang)
        _print_headless_guidance()
        return 1

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
            return 0
        func = section_map.get(canonical)
        if func is None:
            print_error(f"Unknown section: {section}")
            print_info("Available: " + ", ".join(k for k, _ in SETUP_SECTIONS) + ", doctor")
            return 1
        _print_banner()
        func(config)
        path = save_config(config, config_target)
        label = t(f"section.{canonical}")
        print_success(t("summary.section_saved", label=label, path=path))
        _offer_gateway_start(config, path, workspace, section_only=True)
        return 0

    _print_banner()

    is_existing = bool(existing_file and (config.get("models", {}) or {}).get("providers"))

    if flow is None and is_existing:
        print()
        print_success(t("menu.existing_detected"))
        menu_choices: list[ui.Choice] = [
            ("quickstart", t("menu.existing_quick"), ""),
            ("full", t("menu.existing_full"), ""),
            *[(f"section:{key}", t("menu.existing_section", label=t(f"section.{key}")), "")
              for key, _ in SETUP_SECTIONS],
            ("doctor", t("section.doctor"), ""),
            ("exit", t("menu.exit"), ""),
        ]
        choice = ui.select(t("menu.existing_what"), menu_choices, default="quickstart")
        if choice == "exit":
            print_info(t("menu.exit_hint"))
            return 0
        if choice == "quickstart":
            flow = "quickstart"
        elif choice == "full":
            flow = "full"
        elif choice == "doctor":
            setup_doctor(config)
            return 0
        else:
            key = choice.split(":", 1)[1]
            func = section_map[key]
            func(config)
            path = save_config(config, config_target)
            print_success(t("summary.section_saved", label=t(f"section.{key}"), path=path))
            _offer_gateway_start(config, path, workspace, section_only=True)
            return 0

    language_done = False
    if flow is None and not is_existing:
        # Language is a meta-setting: choose it before the first_run choice (and
        # everything after) so the whole wizard renders in the user's language
        # instead of an auto-detected guess.
        setup_language(config)
        language_done = True
        flow = ui.select(t("menu.first_run"), [
            ("quickstart", t("menu.first_run_quick"), ""),
            ("full", t("menu.first_run_full"), ""),
        ], default="quickstart")

    if flow == "quickstart":
        if not language_done:
            setup_language(config)
        setup_model(config)
        setup_permissions(config)
        path = save_config(config, config_target)
        _ensure_credential_key(_resolve_workspace(config))
        setup_doctor(config)
        _print_summary(config, path)
        _offer_gateway_start(config, path, workspace)
        ui.outro(t("summary.complete"))
        return 0

    for _key, func in SETUP_SECTIONS:
        if _key == "language" and language_done:
            continue  # already chosen before the first_run menu
        func(config)

    path = save_config(config, config_target)
    _ensure_credential_key(_resolve_workspace(config))
    setup_doctor(config)
    _print_summary(config, path)
    _offer_gateway_start(config, path, workspace)
    print_success(t("summary.complete"))
    return 0


def has_any_provider_configured(config_path: str | Path | None = None, workspace: str | Path | None = None) -> bool:
    config_file = resolve_config_file(config_path=config_path, search_dir=workspace)
    if not config_file or not config_file.exists():
        return False
    import yaml
    with open(config_file, encoding="utf-8") as f:
        data = yaml.safe_load(f) or {}
    providers = (data.get("models", {}) or {}).get("providers", []) or []
    # An empty mapping/list item is not a usable provider. Keeping this predicate
    # aligned with the doctor capability check prevents the installer from
    # treating ``providers: [{}]`` as a completed setup and silently skipping the
    # wizard.
    return any(isinstance(provider, dict) and provider.get("name") for provider in providers)


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
    try:
        accepted = prompt_yes_no(t("summary.first_run_prompt"), default=True)
    except PromptAborted:
        # Ctrl-C / EOF at the very first prompt of a fresh install means "let me
        # out", not "run me without a config" - keep the historical clean exit,
        # decided here at the flow boundary instead of inside the input helper.
        raise SystemExit(0) from None
    if accepted:
        run_setup_wizard(config_path=config_path, workspace=workspace, lang=lang)
        return True
    print_info(t("summary.first_run_skip_msg"))
    print_info(t("summary.first_run_skip_hint"))
    return False
