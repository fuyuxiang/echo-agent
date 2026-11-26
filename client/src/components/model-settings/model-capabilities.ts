import type { ApiBackend, ModelReasoningEffort } from "@/lib/agent-client";
import bundledProfiles from "../../../agent/crates/echo-agent-runtime/src/agent/model_capability_profiles.json";

interface ModelCapabilityProfile {
  model: string;
  endpoints: Partial<Record<ApiBackend, string[]>>;
  tools?: boolean;
  images?: boolean;
  alwaysThinks?: boolean;
  reasoningEfforts: ModelReasoningEffort[];
  providerDefaultEffort?: ModelReasoningEffort;
}

const profiles = bundledProfiles as ModelCapabilityProfile[];

export const REASONING_EFFORT_LABELS: Record<ModelReasoningEffort, string> = {
  none: "关闭",
  minimal: "最低",
  low: "低",
  medium: "中",
  high: "高",
  xhigh: "超高",
  max: "极致",
};

export function modelCapabilityProfile(model: string, baseUrl: string | undefined, backend: ApiBackend) {
  const name = model.trim().toLowerCase();
  const endpoint = baseUrl?.trim().replace(/\/+$/, "");
  return profiles.find(profile => profile.model.toLowerCase() === name &&
    endpoint && profile.endpoints[backend]?.includes(endpoint));
}

export function effortOptionsForProtocol(backend: ApiBackend): ModelReasoningEffort[] {
  // Messages only serializes the positive effort levels. Non-thinking is
  // available on other protocols only when the model explicitly supports it.
  const levels: ModelReasoningEffort[] = ["low", "medium", "high", "xhigh", "max"];
  return backend === "messages" ? levels : ["none", "minimal", ...levels];
}
