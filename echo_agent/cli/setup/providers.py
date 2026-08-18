"""Provider brand catalog for the setup wizard.

Each entry maps a user-facing brand onto a runtime provider *dialect*
(echo_agent.models.providers._PROVIDER_MAP) plus the base URL / fallback
models / models-listing endpoint the wizard needs. OpenAI-compatible brands
(DeepSeek, Qwen, Kimi, GLM, SiliconFlow, Ollama, ...) use dialect="openai"
with an explicit api_base.
"""
from __future__ import annotations

from dataclasses import dataclass, field

# group ids -> i18n label keys are resolved by the caller via t("provider.group.<id>")
GROUP_ORDER = ["mainstream", "domestic", "aggregator", "local", "cloud"]


@dataclass(frozen=True)
class ProviderCatalogEntry:
    id: str
    label: str               # display name (plain; may be localized by caller)
    group: str               # one of GROUP_ORDER
    dialect: str             # runtime provider name
    api_base: str = ""       # prefilled base URL for openai-compat brands
    api_key_env_vars: tuple[str, ...] = ()
    fallback_models: list[str] = field(default_factory=list)
    models_endpoint: str = ""  # absolute or relative; "" => no dynamic listing
    needs_api_base: bool = False


CATALOG: list[ProviderCatalogEntry] = [
    # ── mainstream ──
    ProviderCatalogEntry(
        id="openai", label="OpenAI", group="mainstream", dialect="openai",
        api_base="https://api.openai.com/v1", api_key_env_vars=("OPENAI_API_KEY",),
        fallback_models=["gpt-4o", "gpt-4o-mini", "gpt-4-turbo", "o1", "o3-mini"],
        models_endpoint="https://api.openai.com/v1/models",
    ),
    ProviderCatalogEntry(
        id="anthropic", label="Anthropic", group="mainstream", dialect="anthropic",
        api_key_env_vars=("ANTHROPIC_API_KEY",),
        fallback_models=["claude-sonnet-4-20250514", "claude-opus-4-20250514",
                         "claude-haiku-4-5-20251001"],
        models_endpoint="https://api.anthropic.com/v1/models",
    ),
    ProviderCatalogEntry(
        id="gemini", label="Google Gemini", group="mainstream", dialect="gemini",
        api_key_env_vars=("GOOGLE_API_KEY", "GEMINI_API_KEY"),
        fallback_models=["gemini-2.5-pro", "gemini-2.5-flash"],
        models_endpoint="https://generativelanguage.googleapis.com/v1beta/models",
    ),
    # ── domestic (OpenAI-compatible) ──
    ProviderCatalogEntry(
        id="deepseek", label="DeepSeek", group="domestic", dialect="openai",
        api_base="https://api.deepseek.com/v1", api_key_env_vars=("DEEPSEEK_API_KEY",),
        fallback_models=["deepseek-chat", "deepseek-reasoner"],
        models_endpoint="https://api.deepseek.com/v1/models",
    ),
    ProviderCatalogEntry(
        id="qwen", label="通义千问 Qwen", group="domestic", dialect="openai",
        api_base="https://dashscope.aliyuncs.com/compatible-mode/v1",
        api_key_env_vars=("DASHSCOPE_API_KEY", "QWEN_API_KEY"),
        fallback_models=["qwen-max", "qwen-plus", "qwen-turbo"],
        models_endpoint="https://dashscope.aliyuncs.com/compatible-mode/v1/models",
    ),
    ProviderCatalogEntry(
        id="kimi", label="Moonshot Kimi", group="domestic", dialect="openai",
        api_base="https://api.moonshot.cn/v1", api_key_env_vars=("MOONSHOT_API_KEY", "KIMI_API_KEY"),
        fallback_models=["moonshot-v1-8k", "moonshot-v1-32k", "moonshot-v1-128k"],
        models_endpoint="https://api.moonshot.cn/v1/models",
    ),
    ProviderCatalogEntry(
        id="glm", label="智谱 GLM", group="domestic", dialect="openai",
        api_base="https://open.bigmodel.cn/api/paas/v4",
        api_key_env_vars=("ZHIPU_API_KEY", "GLM_API_KEY"),
        fallback_models=["glm-4-plus", "glm-4-air", "glm-4-flash"],
        models_endpoint="https://open.bigmodel.cn/api/paas/v4/models",
    ),
    ProviderCatalogEntry(
        id="minimax", label="MiniMax", group="domestic", dialect="openai",
        # minimaxi.com (extra "i") is the mainland-China endpoint; the
        # international one is api.minimax.io. Keys are not interchangeable.
        api_base="https://api.minimaxi.com/v1", api_key_env_vars=("MINIMAX_API_KEY",),
        fallback_models=["MiniMax-Text-01", "abab6.5s-chat"],
        models_endpoint="https://api.minimaxi.com/v1/models",
    ),
    # ── aggregator ──
    ProviderCatalogEntry(
        id="openrouter", label="OpenRouter", group="aggregator", dialect="openrouter",
        api_key_env_vars=("OPENROUTER_API_KEY",),
        fallback_models=["openai/gpt-4o", "anthropic/claude-sonnet-4-20250514",
                         "google/gemini-2.5-pro"],
        models_endpoint="https://openrouter.ai/api/v1/models",
    ),
    ProviderCatalogEntry(
        id="siliconflow", label="SiliconFlow 硅基流动", group="aggregator", dialect="openai",
        api_base="https://api.siliconflow.cn/v1", api_key_env_vars=("SILICONFLOW_API_KEY",),
        fallback_models=["deepseek-ai/DeepSeek-V3", "Qwen/Qwen2.5-72B-Instruct"],
        models_endpoint="https://api.siliconflow.cn/v1/models",
    ),
    ProviderCatalogEntry(
        id="atlascloud", label="Atlas Cloud", group="aggregator", dialect="openai",
        api_base="https://api.atlascloud.ai/v1",
        api_key_env_vars=("ATLASCLOUD_API_KEY",),
        fallback_models=["qwen/qwen3.8-max"],
        models_endpoint="https://api.atlascloud.ai/v1/models",
    ),
    # ── local ──
    ProviderCatalogEntry(
        id="ollama", label="Ollama", group="local", dialect="openai",
        api_base="http://127.0.0.1:11434/v1",
        fallback_models=["llama3.1", "qwen2.5"],
        models_endpoint="http://127.0.0.1:11434/v1/models",
    ),
    ProviderCatalogEntry(
        id="lmstudio", label="LM Studio / vLLM", group="local", dialect="openai",
        needs_api_base=True, fallback_models=[],
    ),
    # ── cloud ──
    ProviderCatalogEntry(
        id="bedrock", label="AWS Bedrock", group="cloud", dialect="bedrock",
        fallback_models=["anthropic.claude-sonnet-4-20250514-v1:0",
                         "anthropic.claude-haiku-4-5-20251001-v1:0"],
    ),
    # custom lives in the "local" group: like LM Studio / vLLM it's a
    # user-supplied OpenAI-compatible endpoint, so it needs no separator of
    # its own. Its display name is localized via t("provider.custom_label");
    # this label is only the fallback for non-i18n contexts.
    ProviderCatalogEntry(
        id="custom", label="Custom (OpenAI-compatible)", group="local", dialect="openai",
        needs_api_base=True, fallback_models=[],
    ),
]


def find(entry_id: str) -> ProviderCatalogEntry | None:
    for e in CATALOG:
        if e.id == entry_id:
            return e
    return None


def grouped_catalog() -> list[tuple[str, list[ProviderCatalogEntry]]]:
    """Return (group_id, entries) in GROUP_ORDER, preserving CATALOG order."""
    out: list[tuple[str, list[ProviderCatalogEntry]]] = []
    for gid in GROUP_ORDER:
        entries = [e for e in CATALOG if e.group == gid]
        if entries:
            out.append((gid, entries))
    return out
