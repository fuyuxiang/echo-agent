import { invoke } from "@tauri-apps/api/core";

/** Native YAML merge preserves advanced configuration and rejects unsafe resets. */
export function mergeExpertDefinition(originalRaw: string, generatedRaw: string): Promise<string> {
  return invoke<string>("agents_merge_definition", { originalRaw, generatedRaw });
}

export interface ExpertSettings {
  model: string;
  skills: string[];
  tools: string[];
  disallowedTools: string[];
  discoverSkills: boolean;
  inheritSkills: boolean;
  mcpServers: string[] | null;
  mcpInheritance: "all" | "none" | { named: string[] } | { except: string[] };
}
export const configureExpertDefinition = (raw: string, settings: ExpertSettings) =>
  invoke<string>("agents_configure_definition", { raw, settings });
