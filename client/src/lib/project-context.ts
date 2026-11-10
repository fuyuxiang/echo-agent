import type { ProjectMeta, ProjectConversation, ProjectTab, RefItem } from "@/stores/projects-store";
import type { PermissionMode } from "@/lib/agent-client";

export interface ProjectConversationOptions {
  /** The directory shown at submission, or null to allocate an independent directory. */
  cwd: string | null;
  /** Explicit choice for this task, if the draft overrides the default. */
  requestedPermissionMode?: PermissionMode;
  expectedPermissionMode: PermissionMode;
  prepareAttachments?: () => Promise<string[]>;
  rememberModel?: boolean;
  sourceTab?: ProjectTab;
}

export function projectSessionTitle(sessionTitle?: string, projectTitle?: string): string {
  const title = sessionTitle?.trim();
  return title && title !== "未命名会话" ? title : projectTitle?.trim() || "未命名会话";
}

export type StartProjectConversation = (
  projectId: string,
  message: string,
  modelId: string,
  options: ProjectConversationOptions,
) => Promise<string | undefined>;

function names(items: RefItem[]): string {
  return items.map((item) => `${item.name} (id: ${item.id})`).join("、");
}

/**
 * Build the hidden project contract sent with every newly-created project
 * session. The visible user bubble remains unchanged.
 */
export function buildProjectPrompt(project: ProjectMeta, userMessage: string): string {
  const context: string[] = [
    `<system-reminder>`,
    `你正在 EchoAgent 项目「${project.name}」中工作。以下是本会话的项目级契约：`,
  ];
  if (project.instructions?.trim()) {
    context.push(`\n[项目指令]\n${project.instructions.trim()}`);
  }
  if (project.experts.length > 0) {
    context.push(`\n[项目偏好 Agent]\n${names(project.experts)}\n在适合的任务上优先委派给这些 Agent；执行前仍必须以当前运行时实际可用性为准。`);
  }
  if (project.skills.length > 0) {
    context.push(`\n[项目偏好 Skill]\n${names(project.skills)}\n任务匹配时应优先遵循这些 Skill；只有当前运行时已加载的 Skill 才可执行。`);
  }
  if (project.connectors.length > 0) {
    context.push(`\n[项目偏好 MCP]\n${names(project.connectors)}\n需要外部数据时优先使用这些 MCP；必须先确认服务器已连接，不可伪造外部读写结果。`);
  }
  context.push(`\n如果所需能力当前不可用，必须明确说明阻塞，不得宣称已执行。`, `</system-reminder>`);
  return `${context.join("\n")}\n\n${userMessage}`;
}

/** Empty project chats and newly imported task chats each need one contract turn. */
export function projectPromptForTurn(
  project: ProjectMeta | undefined,
  conversation: ProjectConversation | undefined,
  isFirstUserTurn: boolean,
  promptBody: string,
): { promptText: string; consumedPendingContext: boolean } {
  if (!project || !conversation) return { promptText: promptBody, consumedPendingContext: false };
  const consumedPendingContext = !!conversation.pendingProjectContext;
  return {
    promptText: isFirstUserTurn || consumedPendingContext
      ? buildProjectPrompt(project, promptBody)
      : promptBody,
    consumedPendingContext,
  };
}
