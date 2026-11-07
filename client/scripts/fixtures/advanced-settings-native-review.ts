/** Isolated advanced-settings responses: no real runtime calls or config writes. */
export function advancedSettingsNativeReview(query: URLSearchParams) {
  if (!query.has("advancedSettingsReview")) return null;
  let depth = 4, reads = 0;
  const calls = { reads: 0, saves: [] as number[], reloads: [] as string[] };
  const reloadAttempts = new Map<string, number>();
  Object.assign(window, { advancedSettingsReviewCalls: calls });
  return {
    commands: new Set(["subagents_config_get", "subagents_config_save", "internal_reload"]),
    invoke: async (command: string, args: any) => {
      if (command === "subagents_config_get") {
        calls.reads++;
        if (query.has("advancedReadError") && reads++ === 0) throw new Error("隔离验证：配置暂不可读");
        return { maxDepth: depth };
      }
      if (command === "subagents_config_save") { calls.saves.push(args.maxDepth); depth = args.maxDepth; return depth; }
      if (command === "internal_reload") {
        calls.reloads.push(args.kind);
        const attempt = (reloadAttempts.get(args.kind) ?? 0) + 1; reloadAttempts.set(args.kind, attempt);
        await new Promise(resolve => setTimeout(resolve, 180));
        if (query.has("advancedReloadError") && args.kind === "skills" && attempt === 1) throw new Error("隔离验证：技能目录暂不可读");
      }
    },
  };
}
