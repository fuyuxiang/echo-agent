import type { ReactNode } from "react";
import { Button } from "../Button";
import { modelDisplayName } from "@/lib/model-presentation";
import {
  type ApiBackend,
  type AuthScheme,
  type ModelEntry,
  type ModelProviderEntry,
  type ProviderKind,
} from "@/lib/agent-client";

export interface ProviderPreset {
  label: string;
  shortLabel: string;
  baseUrl: string;
  apiBackend: ApiBackend;
  authScheme: AuthScheme;
  placeholderKey: string;
  keyUrl?: string;
}

export type ServiceKind = ProviderKind | "kimi" | "zhipu";
export const SERVICE_GROUPS: { label: string; kinds: ServiceKind[] }[] = [
  { label: "国内服务商", kinds: ["deepseek", "qwen", "kimi", "zhipu", "minimax"] },
  { label: "其他服务商", kinds: ["openai", "anthropic"] },
  { label: "自定义接口", kinds: ["custom", "custom_anthropic"] },
];

export const PROVIDER_PRESETS: Record<ServiceKind, ProviderPreset> = {
  openai: {
    label: "OpenAI",
    shortLabel: "OpenAI",
    baseUrl: "https://api.openai.com/v1",
    apiBackend: "responses",
    authScheme: "bearer",
    placeholderKey: "sk-...",
    keyUrl: "https://platform.openai.com/api-keys",
  },
  anthropic: {
    label: "Claude（Anthropic）",
    shortLabel: "Claude",
    baseUrl: "https://api.anthropic.com/v1",
    apiBackend: "messages",
    authScheme: "x_api_key",
    placeholderKey: "sk-ant-...",
    keyUrl: "https://console.anthropic.com/settings/keys",
  },
  minimax: {
    label: "MiniMax",
    shortLabel: "MiniMax",
    baseUrl: "https://api.minimax.cn/v1",
    apiBackend: "chat_completions",
    authScheme: "bearer",
    placeholderKey: "MiniMax API Key",
    keyUrl: "https://platform.minimaxi.com",
  },
  deepseek: {
    label: "DeepSeek",
    shortLabel: "DeepSeek",
    baseUrl: "https://api.deepseek.com",
    apiBackend: "chat_completions",
    authScheme: "bearer",
    placeholderKey: "sk-...",
    keyUrl: "https://platform.deepseek.com",
  },
  qwen: {
    label: "阿里云百炼（通义千问）",
    shortLabel: "阿里云百炼",
    baseUrl: "https://dashscope.aliyuncs.com/compatible-mode/v1",
    apiBackend: "chat_completions",
    authScheme: "bearer",
    placeholderKey: "sk-...",
    keyUrl: "https://bailian.console.aliyun.com",
  },
  // UI shortcuts for the existing custom Chat Completions transport. No new
  // provider kind, credential format, or runtime routing is introduced.
  kimi: {
    label: "Kimi（月之暗面）", shortLabel: "Kimi",
    baseUrl: "https://api.moonshot.cn/v1",
    apiBackend: "chat_completions", authScheme: "bearer",
    placeholderKey: "填写 Kimi API 密钥", keyUrl: "https://platform.moonshot.cn",
  },
  zhipu: {
    label: "智谱 GLM", shortLabel: "智谱 GLM",
    baseUrl: "https://open.bigmodel.cn/api/paas/v4/",
    apiBackend: "chat_completions", authScheme: "bearer",
    placeholderKey: "填写智谱 API 密钥", keyUrl: "https://bigmodel.cn/usercenter/proj-mgmt/apikeys",
  },
  custom: {
    label: "自定义（OpenAI 兼容）",
    shortLabel: "OpenAI 兼容",
    baseUrl: "",
    apiBackend: "chat_completions",
    authScheme: "bearer",
    placeholderKey: "your-api-key",
  },
  custom_anthropic: {
    label: "自定义（Anthropic 兼容）",
    shortLabel: "Anthropic 兼容",
    baseUrl: "",
    apiBackend: "messages",
    authScheme: "x_api_key",
    placeholderKey: "sk-ant-...",
  },
};

export interface ConnectionDraft {
  service: ServiceKind;
  label: string;
  providerKind: ProviderKind;
  baseUrl: string;
  allowInsecureHttp: boolean;
  apiKey: string;
  apiBackend: ApiBackend;
  authScheme: AuthScheme;
  contextWindow: string;
}

export type PanelMessage = { kind: "ok" | "err" | "warn"; text: string; retry?: "load" | "organization" };
export type ModelMutation = (action: () => Promise<void>) => Promise<void>;

export function serviceKind(provider: ModelProviderEntry): ServiceKind {
  if (provider.providerKind === "custom" && (!provider.apiBackend || provider.apiBackend === "chat_completions")
    && (!provider.authScheme || provider.authScheme === "bearer")) {
    const address = provider.baseUrl?.replace(/\/+$/, "");
    for (const kind of ["kimi", "zhipu"] as const) {
      if (address === PROVIDER_PRESETS[kind].baseUrl.replace(/\/+$/, "")) return kind;
    }
  }
  return provider.providerKind;
}

export function isCustomConnection(kind: ServiceKind): boolean {
  return kind === "custom" || kind === "custom_anthropic";
}

export function savedConnectionName(provider: ModelProviderEntry): string {
  const label = provider.label?.trim() ?? "";
  if (provider.source !== "organization" && isCustomConnection(provider.providerKind) &&
      (label === PROVIDER_PRESETS.custom.shortLabel || label === PROVIDER_PRESETS.custom_anthropic.shortLabel)) {
    return "";
  }
  return label;
}

export function customConnectionName(address?: string): string {
  const host = /^https?:\/\/(.+)$/.exec(connectionOrigin(address) ?? "")?.[1];
  return host || "自定义连接";
}

export function connectionName(provider: ModelProviderEntry): string {
  const label = savedConnectionName(provider);
  if (provider.source === "organization") return label || "组织提供";
  if (label) return label;
  if (isCustomConnection(provider.providerKind)) return customConnectionName(provider.baseUrl);
  return PROVIDER_PRESETS[serviceKind(provider)]?.shortLabel || provider.id;
}

export function providerDisplayName(provider: ModelProviderEntry): string {
  const label = savedConnectionName(provider);
  if (provider.source === "organization") return label || "组织提供";
  return label || PROVIDER_PRESETS[serviceKind(provider)]?.shortLabel || "自定义连接";
}

export function connectionBaseUrlError(value: string, allowInsecureHttp: boolean): string | null {
  if (!value.trim()) return "请填写 Base URL。";
  let url: URL;
  try {
    url = new URL(value.trim());
  } catch {
    return "Base URL 格式不正确。";
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") return "Base URL 必须使用 HTTP 或 HTTPS。";
  const misplacedPort = /^\/:([0-9]+)(?:\/|$)/.exec(url.pathname);
  if (misplacedPort) return `端口号位置不对：请把 /:${misplacedPort[1]}/ 改成 :${misplacedPort[1]}/。`;
  if (url.protocol === "http:" && !allowInsecureHttp) {
    return "使用 HTTP 前，请确认下方的明文传输选项；也可以改用 HTTPS。";
  }
  return null;
}

export function connectionOrigin(value: string | undefined): string | undefined {
  try {
    return value ? new URL(value.trim()).origin : undefined;
  } catch {
    return undefined;
  }
}

export function modelRemoteId(model: ModelEntry): string {
  return model.remoteModelId || model.modelId;
}

export function modelName(model: ModelEntry): string {
  return modelDisplayName({ id: model.modelId, label: model.name, remoteModelId: model.remoteModelId });
}

export type ModelLimitDraft = { contextWindow: string; maxOutputTokens: string };

export function parseModelLimits(draft: ModelLimitDraft, connectionContextWindow?: number) {
  const contextWindow = draft.contextWindow.trim() ? Number(draft.contextWindow) : undefined;
  const maxOutputTokens = draft.maxOutputTokens.trim() ? Number(draft.maxOutputTokens) : undefined;
  if (contextWindow !== undefined && (!Number.isSafeInteger(contextWindow) || contextWindow <= 0)) return "上下文窗口必须是大于 0 的整数。";
  if (maxOutputTokens !== undefined && (!Number.isInteger(maxOutputTokens) || maxOutputTokens <= 0 || maxOutputTokens > 0xffffffff)) return "最大输出必须是大于 0 的整数，且不超过 4,294,967,295。";
  if (maxOutputTokens !== undefined && maxOutputTokens > (contextWindow ?? connectionContextWindow ?? Infinity)) return "最大输出不能超过上下文窗口。";
  return { contextWindow, maxOutputTokens };
}

function LimitPresets({ label, sizes, value, ceiling, onSelect }: {
  label: string;
  sizes: number[];
  value: string;
  ceiling?: number;
  onSelect: (value: string) => void;
}) {
  return <div className="model-limit-presets" role="group" aria-label={label}>
    {sizes.map(size => {
      const tokens = size * 1024;
      return <Button key={size} size="compact" variant="ghost" aria-pressed={Number(value) === tokens}
        disabled={ceiling !== undefined && tokens > ceiling} onClick={() => onSelect(String(tokens))}>{size}K</Button>;
    })}
  </div>;
}

export function ModelLimitFields({ draft, onChange, connectionContextWindow, fieldA11y, fieldError, layout }: {
  draft: ModelLimitDraft;
  onChange: (draft: ModelLimitDraft) => void;
  connectionContextWindow?: number;
  /** Optional per-field ARIA attributes, used by editors that locate validation errors. */
  fieldA11y?: (id: string) => Record<string, unknown>;
  /** Optional per-field inline error rendered after the input. */
  fieldError?: (id: string) => ReactNode;
  layout?: "directions";
}) {
  const contextLimit = Number(draft.contextWindow) || connectionContextWindow;
  return (
    <div className="models-settings-panel__field-row model-connection-editor__limits">
      <div className="models-settings-panel__field">
        <label className="models-settings-panel__label" htmlFor="manual-model-context-window">{layout === "directions" ? "输入" : "上下文窗口（tokens）"}</label>
        <input id="manual-model-context-window" aria-label={layout === "directions" ? "输入，上下文窗口（tokens）" : undefined}
          {...fieldA11y?.("manual-model-context-window")} className="form-control models-settings-panel__input" type="number" min={1}
          value={draft.contextWindow} onChange={event => onChange({ ...draft, contextWindow: event.target.value })}
          placeholder={connectionContextWindow ? `${connectionContextWindow.toLocaleString()}（连接默认）` : "使用默认值"} />
        {fieldError?.("manual-model-context-window")}
        <LimitPresets label="常用上下文窗口" sizes={[32, 64, 128, 256]} value={draft.contextWindow}
          onSelect={contextWindow => onChange({ ...draft, contextWindow })} />
      </div>
      <div className="models-settings-panel__field">
        <label className="models-settings-panel__label" htmlFor="manual-model-max-output">{layout === "directions" ? "输出" : "最大输出（tokens）"}</label>
        <input id="manual-model-max-output" aria-label={layout === "directions" ? "输出，最大输出（tokens）" : undefined}
          {...fieldA11y?.("manual-model-max-output")} className="form-control models-settings-panel__input" type="number" min={1} step={1} max={4294967295}
          value={draft.maxOutputTokens} onChange={event => onChange({ ...draft, maxOutputTokens: event.target.value })} placeholder="使用默认值" />
        {fieldError?.("manual-model-max-output")}
        <LimitPresets label="常用最大输出" sizes={[8, 16, 32, 64]} value={draft.maxOutputTokens} ceiling={contextLimit}
          onSelect={maxOutputTokens => onChange({ ...draft, maxOutputTokens })} />
      </div>
    </div>
  );
}

export function draftProvider(original: ModelProviderEntry | undefined, draft: ConnectionDraft): ModelProviderEntry {
  return {
    id: original?.id ?? "",
    label: draft.label.trim(),
    providerKind: draft.providerKind,
    baseUrl: draft.baseUrl.trim(),
    allowInsecureHttp: draft.allowInsecureHttp,
    apiKey: draft.apiKey.trim() || undefined,
    apiBackend: draft.apiBackend,
    authScheme: draft.authScheme,
    contextWindow: draft.contextWindow ? Number(draft.contextWindow) : undefined,
    source: original?.source,
  };
}
