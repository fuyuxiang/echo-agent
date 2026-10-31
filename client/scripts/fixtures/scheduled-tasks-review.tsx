// Isolated IPC for the real production components. No model requests, disk
// writes, system notifications or real scheduled tasks are performed here.
import React, { useState } from "react";
import { createRoot } from "react-dom/client";
import { AutomationPanel } from "../../src/components/AutomationPanel";
import { buildDraft } from "../../src/components/automation/schedule-utils";
import type { Automation, AutomationRunRecord } from "../../src/lib/types";
import "../../src/styles/index.css";

const params = new URLSearchParams(location.search);
document.documentElement.dataset.theme = params.get("theme") || "light";
document.documentElement.classList.toggle(
  "dark",
  params.get("theme") === "dark",
);
const tomorrow = new Date();
tomorrow.setDate(tomorrow.getDate() + 1);
tomorrow.setHours(9, 0, 0, 0);
let tasks: Automation[] = [
  {
    ...buildDraft(undefined, "/review/EchoAgent"),
    id: "news",
    name: "每日 AI 新闻简报",
    prompt: "整理当天 AI 领域的重要进展，附上来源，说明值得关注的原因。",
    createdAt: new Date().toISOString(),
    nextRunAt: tomorrow.toISOString(),
  },
  {
    ...buildDraft(undefined, "/review/工作资料"),
    id: "weekly",
    name: "每周工作回顾",
    prompt: "汇总本周已完成的工作、遇到的问题和下周待办。",
    status: "PAUSED",
    newSessionEachRun: false,
    createdAt: new Date().toISOString(),
  },
  {
    ...buildDraft(),
    id: "expired",
    name: "已结束的一次性报告",
    prompt: "输出阶段报告。",
    scheduleType: "once",
    scheduledDate: "2020-01-01",
    status: "PAUSED",
    createdAt: "2020-01-01T00:00:00Z",
  },
];
if (params.get("many") === "1") {
  tasks.push(...tasks.map((task, index) => ({
    ...task,
    id: `${task.id}-extra`,
    name: index === 0 ? "跟进每周产品需求评审并整理待办事项、负责人和截止时间" : `${task.name} · 第二组`,
    prompt: `${task.prompt} 汇总各项工作的进度、风险与负责人，给出下一步可执行的建议，并在报告中保留相关文档的引用来源。`,
    cwds: index === 0 ? "/review/一段很长的中文工作目录名称-验证卡片在窄窗口下仍然完整可用" : task.cwds,
  })));
}
let records: AutomationRunRecord[] = [
  {
    id: "old",
    automationId: "news",
    automationName: tasks[0].name,
    promptId: "prompt-old",
    status: params.get("state") === "permission" ? "running" : "success",
    needsAttention: params.get("state") === "permission",
    sessionId: "session-old",
    startedAt: new Date(Date.now() - 3600000).toISOString(),
    archived: false,
    notificationDelivery: "通知投递未全部成功，请检查通知渠道",
  },
];
let sequence = 0;
const calls: { command: string; args: unknown }[] = [];
const resultById: Record<string, string> = {
  old: "## 今日简报\n\n已整理 5 条值得关注的进展。\n\n本次结果已生成，通知未送达不影响执行结果。",
};
Object.assign(window, {
  scheduledReview: {
    calls,
    snapshot: () => structuredClone({ tasks, records }),
  },
  __TAURI_INTERNALS__: {
    transformCallback: () => 1,
    unregisterCallback: () => {},
    invoke: async (command: string, args: Record<string, any> = {}) => {
      calls.push({ command, args });
      switch (command) {
        case "automations_snapshot":
          return structuredClone({ automations: tasks, records });
        case "desktop_preferences_get":
          return { closeToTray: params.get("background") !== "off" };
        case "notify_desktop_status":
          return { enabled: true, source: "default", permission: "unknown" };
        case "notify_channels_list":
          return [];
        case "agent_auth_status":
          return {
            ready: true,
            runtimeReady: true,
            synchronized: true,
            runtimeModels: ["builtin/m", "org/m", "personal/m"],
            defaultModelId: "builtin/m",
            providers: [],
          };
        case "providers_list":
          return {
            providers: [
              {
                id: "builtin",
                providerKind: "custom",
                source: "builtin",
                label: "Echo",
                managed: true,
              },
              {
                id: "org",
                providerKind: "custom",
                source: "organization",
                label: "研发团队",
                managed: true,
              },
              {
                id: "personal",
                providerKind: "custom",
                source: "personal",
                label: "我的网关",
              },
            ],
            models: [
              {
                modelId: "builtin/m",
                name: "Echo Brook",
                providerId: "builtin",
                remoteModelId: "brook",
              },
              {
                modelId: "org/m",
                name: "团队模型",
                providerId: "org",
                remoteModelId: "team",
              },
              {
                modelId: "personal/m",
                name: "个人模型",
                providerId: "personal",
                remoteModelId: "custom",
              },
            ],
          };
        case "agent_list_workspaces":
          return [{ cwd: "/review/EchoAgent", sessionCount: 2 }];
        case "agents_list":
        case "skills_list":
        case "mcp_list":
          return [];
        case "automations_save": {
          const current = tasks.find((task) => task.id === args.automation.id);
          const saved: Automation = {
            ...args.automation,
            id: current?.id || "new-task",
            createdAt: current?.createdAt || new Date().toISOString(),
            nextRunAt:
              args.automation.status === "ACTIVE"
                ? tomorrow.toISOString()
                : undefined,
            contextGeneration: current?.contextGeneration || 0,
          };
          tasks = [...tasks.filter((task) => task.id !== saved.id), saved];
          return structuredClone(saved);
        }
        case "automations_set_status":
          tasks = tasks.map((task) =>
            task.id === args.id
              ? {
                  ...task,
                  status: args.status,
                  nextRunAt:
                    args.status === "ACTIVE"
                      ? tomorrow.toISOString()
                      : undefined,
                }
              : task,
          );
          return;
        case "automations_delete":
          tasks = tasks.filter((task) => task.id !== args.id);
          return;
        case "automations_reset_session": {
          const task = tasks.find((task) => task.id === args.id)!;
          task.contextGeneration = (task.contextGeneration || 0) + 1;
          task.dedicatedSessionId = undefined;
          return structuredClone(task);
        }
        case "automation_run_result":
          return { text: resultById[args.id] || "", truncated: false };
        case "automation_run_stop":
          records = records.map((run) =>
            run.id === args.id
              ? { ...run, status: "stopped", stopRequested: true }
              : run,
          );
          return;
        case "automation_records_archive":
          records = records.map((run) =>
            run.id === args.id ? { ...run, archived: args.archived } : run,
          );
          return;
        case "automation_records_delete":
          records = records.filter((run) => run.id !== args.id);
          return;
        case "automations_run": {
          const task = tasks.find((task) => task.id === args.id)!;
          if (
            records.some(
              (run) =>
                run.automationId === task.id &&
                ["queued", "running"].includes(run.status),
            )
          )
            throw new Error("该任务正在运行");
          const id = `run-${++sequence}`;
          const sessionId =
            task.newSessionEachRun === false
              ? (task.dedicatedSessionId ||= `shared-${task.contextGeneration || 0}`)
              : `fresh-${sequence}`;
          records.unshift({
            id,
            automationId: task.id,
            automationName: task.name,
            status: "queued",
            sessionId,
            promptId: `prompt-${sequence}`,
            startedAt: new Date().toISOString(),
            archived: false,
          });
          setTimeout(() => {
            const run = records.find((run) => run.id === id)!;
            if (run.status !== "stopped") run.status = "success";
            run.finishedAt = new Date().toISOString();
            resultById[id] =
              `第 ${sequence} 次运行的独立结果。会话方式：${task.newSessionEachRun === false ? "沿用任务专属会话" : "每次新会话"}。`;
          }, 250);
          return id;
        }
        default:
          throw new Error(`Unexpected isolated command: ${command}`);
      }
    },
  },
});
function Review() {
  const [message, setMessage] = useState("");
  return (
    <main
      style={{
        height: "100%",
        background: "var(--echo-bg-primary)",
        color: "var(--echo-text-strong)",
        display: "flex",
        flexDirection: "column",
      }}
    >
      {message && (
        <div
          data-testid="review-feedback"
          style={{ padding: "8px 16px", flex: "none" }}
        >
          {message}
        </div>
      )}
      <AutomationPanel
        cwd="/review/EchoAgent"
        onToast={setMessage}
        onOpenSession={(id) => setMessage(`已打开会话：${id}`)}
        onOpenSettings={(section) => setMessage(`已打开设置：${section}`)}
      />
    </main>
  );
}
createRoot(document.getElementById("root")!).render(<Review />);
