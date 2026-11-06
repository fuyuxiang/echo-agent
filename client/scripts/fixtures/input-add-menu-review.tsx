// Real menu and styles with isolated catalogs and observable callbacks.
import React, { useState } from "react";
import { createRoot } from "react-dom/client";
import { InputAddMenu } from "../../src/components/InputAddMenu";
import { GlobalTooltip } from "../../src/components/GlobalTooltip";
import "../../src/styles/index.css";

const query = new URLSearchParams(location.search);
document.documentElement.dataset.theme = query.get("theme") || "light";
document.documentElement.classList.toggle("dark", query.get("theme") === "dark");
let failCatalog = query.get("state") === "error";
const count = query.get("state") === "empty" ? 0 : 30;
const calls: string[] = [];
Object.assign(window, {
  inputMenuReview: { calls },
  __TAURI_INTERNALS__: {
    invoke: async (command: string) => {
      if (command === "agents_list" && failCatalog) {
        failCatalog = false;
        throw new Error("offline");
      }
      if (command === "agents_list") return Array.from({ length: count }, (_, i) => ({
        name: `评审专家 ${i + 1}`, path: `/review/agents/${i}.md`, description: "检查需求与实现，整理可操作的改进建议。", scope: "user", raw: "评审需求",
      }));
      if (command === "skills_list") return Array.from({ length: count }, (_, i) => ({
        name: `skill-${i + 1}`, displayName: `文档技能 ${i + 1}`, description: "整理任务资料与交付内容。", enabled: true, scope: "user",
      }));
      if (command === "mcp_list") return Array.from({ length: count }, (_, i) => ({
        name: `connector-${i + 1}`, target: `review-tool-${i + 1}`, enabled: true,
      }));
      return null;
    },
  },
});

function Review() {
  const [feedback, setFeedback] = useState("");
  const [mode, setMode] = useState<"default" | "browser_use" | "computer_use">("default");
  const record = (message: string) => { calls.push(message); setFeedback(message); };
  return <>
    <div style={{ position: "absolute", bottom: 32, left: 30 }}>
      <InputAddMenu onPickFiles={() => record("files")}
        onSelectExpert={expert => record(`expert:${expert.name}`)}
        onSelectSkill={name => record(`skill:${name}`)}
        onNavigateConnectors={() => record("connectors")}
        meetingMinutesAvailable onOpenMeetingMinutes={() => record("meeting")}
        automationMode={mode} onAutomationModeChange={next => { setMode(next); record(`mode:${next}`); }}
        automationCapabilities={{ browser: { available: true }, computer: { available: false, platform: "macos", screenCapture: false, inputControl: false, reason: "需要系统权限" } }} />
    </div>
    <output data-testid="feedback">{feedback}</output>
    <GlobalTooltip />
  </>;
}
createRoot(document.getElementById("root")!).render(<Review />);
