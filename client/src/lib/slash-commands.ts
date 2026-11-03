import type { SlashCommand } from "@/lib/types";

/** Maximum rows rendered in the Composer command palette. */
export const SLASH_COMMAND_PAGE_SIZE = 12;

/** A slash token that is eligible for command completion. */
export interface SlashToken {
  start: number;
  end: number;
  value: string;
  query: string;
}

export interface SlashCommandInvocation {
  name: string;
  args: string;
}

export interface RememberArguments {
  scope: "global" | "workspace";
  content: string;
}

export interface ClientSlashCommand extends SlashCommand {
  source: "client";
  requiresSession?: boolean;
}

/**
 * Commands owned by the desktop shell rather than the embedded Agent Runtime.
 * Runtime-owned names (for example /compact, /memory and /plugins) deliberately
 * stay out of this table so they continue through ACP's normal prompt path.
 */
export const CLIENT_SLASH_COMMANDS: readonly ClientSlashCommand[] = [
  { name: "help", description: "打开帮助与问题排查", source: "client" },
  { name: "new", description: "新建任务，当前任务保留在历史记录中", source: "client" },
  { name: "clear", description: "清空当前视图并开始新任务", source: "client" },
  { name: "search", description: "搜索任务和对话历史", source: "client" },
  { name: "history", description: "搜索任务和对话历史", source: "client" },
  { name: "settings", description: "打开设置", argumentHint: "model|agent|memory|security|help", source: "client" },
  { name: "model", description: "打开模型设置", source: "client" },
  { name: "projects", description: "打开项目", source: "client" },
  { name: "agents", description: "打开专家", source: "client" },
  { name: "skills", description: "打开技能", source: "client" },
  { name: "connectors", description: "打开连接器", source: "client" },
  { name: "automation", description: "打开自动化", source: "client" },
  { name: "usage", description: "打开 Token 用量统计", source: "client" },
  { name: "remember", description: "保存一条长期记忆", argumentHint: "[global|workspace] <内容>", source: "client" },
  { name: "flush", description: "立即提取当前任务的记忆并查看处理结果", source: "client", requiresSession: true },
  { name: "dream", description: "整理长期记忆并查看处理结果", source: "client", requiresSession: true },
  { name: "plan", description: "切换当前会话的计划模式", argumentHint: "on|off", source: "client", requiresSession: true },
  { name: "fork", description: "在新对话中继续当前会话", source: "client", requiresSession: true },
  { name: "rename", description: "重命名当前会话", argumentHint: "新标题", source: "client", requiresSession: true },
] as const;

const CLIENT_COMMAND_NAMES = new Set(CLIENT_SLASH_COMMANDS.map((command) => command.name));

/**
 * Return the leading slash token at the caret. Completion is intentionally
 * limited to the first non-whitespace token because runtime built-ins are only
 * resolved there. This keeps the menu's promise aligned with execution.
 */
export function slashTokenAtCursor(text: string, cursor: number): SlashToken | null {
  const safeCursor = Math.max(0, Math.min(cursor, text.length));
  const before = text.slice(0, safeCursor);
  const match = before.match(/^(\s*)(\/[^\s/]*)$/);
  if (!match) return null;
  const value = match[2];
  const start = match[1].length;
  return {
    start,
    end: safeCursor,
    value,
    query: value.slice(1).toLowerCase(),
  };
}

/** Replace exactly the active slash token, including qualified names with ':'. */
export function replaceSlashToken(
  text: string,
  cursor: number,
  command: string,
): { text: string; cursor: number } | null {
  const token = slashTokenAtCursor(text, cursor);
  if (!token) return null;
  const replacement = `${command} `;
  const after = text.slice(token.end).replace(/^[ \t]/, "");
  const next = text.slice(0, token.start) + replacement + after;
  return { text: next, cursor: token.start + replacement.length };
}

/** Parse a submitted leading `/name args` invocation. */
export function parseSlashInvocation(text: string): SlashCommandInvocation | null {
  const trimmed = text.trim();
  if (!trimmed.startsWith("/")) return null;
  const body = trimmed.slice(1);
  if (!body) return null;
  const split = body.search(/\s/);
  const name = (split === -1 ? body : body.slice(0, split)).toLowerCase();
  if (!name || name.includes("/")) return null;
  return {
    name,
    args: split === -1 ? "" : body.slice(split).trim(),
  };
}

export function isClientSlashCommand(name: string): boolean {
  return CLIENT_COMMAND_NAMES.has(name.toLowerCase());
}

/** Parse `/remember [global|workspace] <content>` without flattening multiline content. */
export function parseRememberArguments(args: string): RememberArguments | null {
  const trimmed = args.trim();
  if (!trimmed) return null;
  const scoped = trimmed.match(/^(global|workspace)(?:\s+([\s\S]*))?$/i);
  if (!scoped) return { scope: "workspace", content: trimmed };
  const content = (scoped[2] ?? "").trim();
  if (!content) return null;
  return {
    scope: scoped[1].toLowerCase() as RememberArguments["scope"],
    content,
  };
}

export function availableClientSlashCommands(hasSession: boolean): SlashCommand[] {
  return CLIENT_SLASH_COMMANDS.filter((command) => hasSession || !command.requiresSession);
}

/** Client commands take precedence; duplicate backend names are removed. */
export function mergeSlashCommands(
  clientCommands: readonly SlashCommand[],
  runtimeCommands: readonly SlashCommand[],
): SlashCommand[] {
  const seen = new Set<string>();
  const merged: SlashCommand[] = [];
  for (const command of [...clientCommands, ...runtimeCommands]) {
    const name = command.name.trim().replace(/^\/+/, "").toLowerCase();
    if (!name || name === "marketplace" || seen.has(name)) continue;
    seen.add(name);
    merged.push({ ...command, name });
  }
  return merged;
}

export function slashCommandSourceLabel(source?: string): string | undefined {
  if (!source) return undefined;
  if (source === "client") return "桌面";
  if (source === "builtin") return "内置";
  if (source.startsWith("plugin:")) return `插件 · ${source.slice(7)}`;
  if (source.startsWith("skill:")) return `技能 · ${source.slice(6)}`;
  if (source.startsWith("workflow:")) return `工作流 · ${source.slice(9)}`;
  return source;
}
