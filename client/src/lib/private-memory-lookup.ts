import { invoke } from "@tauri-apps/api/core";

export const privateMemoryInSharedProject = "共享项目会话无法查询私人记忆。请在个人会话中查询，或从记忆管理查看原文。";

export async function isPrivateMemoryLookup(text: string): Promise<boolean> {
  const candidate = /^\s*(?:请\s*)?(?:查记忆\s|(?:仅检查|只检查|只查|查询|查找)\s*(?:workspace|global|当前工作目录|工作目录|工作区|个人全局|全局))/u.test(text);
  return candidate && await invoke<boolean>("agent_is_private_memory_query", { text }) === true;
}
