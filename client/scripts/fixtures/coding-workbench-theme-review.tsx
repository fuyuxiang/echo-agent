// Production CodingWorkbench, including its own TheiaIdeFrame and Agent pane.
// Only the temporary IDE backend is real; native Agent/task IPC is isolated.
import React, { useEffect } from "react";
import { createRoot } from "react-dom/client";
import "../../src/styles/index.css";
import { ThemeProvider, initializeTheme, useTheme } from "../../src/components/ThemeProvider";
import { CodingWorkbench } from "../../src/features/coding/CodingWorkbench";
import { useCodingRuntimeStore } from "../../src/features/coding/lib/coding-task-runtime";

const query = new URLSearchParams(location.search);
const root = query.get("root")!;
localStorage.setItem("echoagent.theme", query.get("theme") || "light");
initializeTheme();
localStorage.setItem("echo-coding-tab-enabled", "false");
useCodingRuntimeStore.getState().setDocumentCancellationReady(true);
const calls: string[] = [];
let callbackId = 0;
const callbacks = new Map<number, (value: unknown) => void>();
Object.assign(window, {
  __TAURI_EVENT_PLUGIN_INTERNALS__: { unregisterListener: () => {} },
  __TAURI_INTERNALS__: {
    metadata: { currentWindow: { label: "theme-workbench-review" }, currentWebview: { label: "theme-workbench-review" } },
    transformCallback: (callback: (value: unknown) => void) => { callbacks.set(++callbackId, callback); return callbackId; },
    unregisterCallback: (id: number) => callbacks.delete(id),
    invoke: async (command: string, args: Record<string, unknown> = {}) => {
      calls.push(command);
      switch (command) {
        case "plugin:event|listen": return args.handler;
        case "plugin:event|unlisten": return;
        case "coding_theia_start": return { url: query.get("ideUrl"), embedToken: query.get("embedToken") };
        case "coding_document_editor_attach": return args.root;
        case "coding_document_editor_detach": case "coding_inline_cancel": return;
        case "coding_task_list": case "coding_verification_detect": case "coding_document_cancellation_state": return [];
        case "coding_isolation_info": return null;
        case "coding_document_cancel_pending": return false;
        default: throw new Error(`验收未模拟的原生操作：${command}`);
      }
    },
  },
});
function Review() {
  const { setTheme } = useTheme();
  useEffect(() => { Object.assign(window, { codingWorkbenchThemeReview: { calls, setTheme } }); }, [setTheme]);
  return <div className="app app--coding"><CodingWorkbench cwd={root} codingWorkspaces={[{ cwd: root }]} activeCodingWorkspaceCwd={root} apiReady models={[{id:"review/available",label:"团队工作模型",providerId:"review"}]} defaultModelId="review/available" /></div>;
}
createRoot(document.getElementById("root")!).render(<ThemeProvider><Review /></ThemeProvider>);
