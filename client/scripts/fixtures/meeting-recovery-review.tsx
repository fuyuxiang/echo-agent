import "../../src/styles/index.css";
// Development-only browser fixture: production meeting UI, in-memory native IPC.
// It never records physical audio, accesses credentials or calls a model service.
import React, { useState } from "react";
import { createRoot } from "react-dom/client";
import { MeetingMinutesPanel } from "../../src/components/MeetingMinutesPanel";
import { ThemeProvider } from "../../src/components/ThemeProvider";
import type { MeetingRecord } from "../../src/lib/meeting-minutes";

const query = new URLSearchParams(location.search);
const scenario = query.get("scenario") ?? "history";
const draftScenario = scenario.startsWith("draft-");
localStorage.setItem("echoagent.theme", query.get("theme") ?? "light");
let record: MeetingRecord = {
  id: "meeting-recovery-review",
  title: "需要恢复的中文会议",
  createdAt: 1_795_123_200_000,
  updatedAt: 1_795_123_200_000,
  status: scenario === "pending-save" ? "paused" : scenario === "minutes" ? "transcribed" : "failed",
  modelId: "deleted/MiniMax",
  providerId: "deleted",
  durationSeconds: 60,
  recordedSamples: 960_000,
  captureSource: "system",
  audioPath: "/isolated-review/中文会议.wav",
  transcript: scenario === "minutes" ? [{ id: 0, start: 0, end: 60, speaker: "说话人1", text: "已保存的中文转写，服务恢复后继续整理会议结论。" }] : [],
  transcriptText: scenario === "minutes" ? "已保存的中文转写" : undefined,
  chunks: [],
  error: scenario === "history" ? "上次转写未完成，可以从已保存的录音继续" : undefined,
};
if (draftScenario) record = {
  ...record,
  error: undefined,
  status: scenario === "draft-recovery" ? "partial" : "completed",
  transcript: [{ id: 0, start: 30, end: 60, speaker: "说话人1", text: "已经完成的转写段落" }],
  minutes: scenario === "draft-recovery" ? null : "# 原会议纪要\n\n尚未校对的内容。",
};
let nativeCapture = scenario === "pending-save";
let stops = 0;
let processing = 0;
const commands: { command: string; args: Record<string, unknown> }[] = [];
Object.assign(window, {
  __meetingReview: { commands },
  __TAURI_INTERNALS__: {
    transformCallback: () => 1,
    invoke: async (command: string, args: Record<string, unknown> = {}) => {
      commands.push({ command, args });
      switch (command) {
        case "meeting_list": return [record];
        case "meeting_get": return record;
        case "meeting_capture_support": return { systemAudio: true };
        case "meeting_minutes_target": return { modelId: "review/chat", recipient: "https://review.example", revision: "fixture" };
        case "meeting_capture_status": return nativeCapture ? { meetingId: record.id, active: false, paused: true, pendingSave: true, level: 0, recordedSamples: record.recordedSamples, error: "磁盘空间不足，录音等待保存" } : null;
        case "meeting_job_active": return false;
        case "meeting_process": {
          if (scenario === "draft-recovery") {
            record = { ...record, status: "completed", transcript: [
              { id: 0, start: 0, end: 30, speaker: "说话人1", text: "重试恢复的新段落" },
              { ...record.transcript[0], id: 1 },
            ], minutes: "# 重试后会议纪要\n\n录音已全部恢复。" };
            return record;
          }
          processing += 1;
          if (processing === 1) {
            record = { ...record, status: "partial", error: "转写服务暂时不可用，已完成的分片保存在本机" };
            throw new Error(record.error);
          }
          record = { ...record, status: "completed", minutes: "# 已恢复会议纪要\n\n- 已补齐录音转写。", error: undefined };
          return record;
        }
        case "meeting_regenerate_minutes": {
          record = { ...record, status: "completed", minutes: "# 现有转写纪要\n\n已用保存的转写生成纪要。", error: undefined };
          return record;
        }
        case "meeting_update": {
          const base = args.expectedContent as { transcript: MeetingRecord["transcript"]; minutes: string | null };
          if (!base || JSON.stringify(base.transcript) !== JSON.stringify(record.transcript) || base.minutes !== (record.minutes ?? null)) {
            throw new Error("会议内容已更新，请刷新后合并校对再保存；当前修改尚未覆盖已保存内容");
          }
          record = { ...record,
            transcript: (args.transcript as MeetingRecord["transcript"] | null) ?? record.transcript,
            minutes: args.transcript ? null : (args.minutes as string | null) ?? record.minutes,
          };
          return record;
        }
        case "meeting_capture_stop": {
          stops += 1;
          if (stops === 1) throw new Error("磁盘空间不足，保存失败");
          nativeCapture = false;
          record = { ...record, status: "recorded", error: undefined };
          return record;
        }
        default: throw new Error(`Unhandled meeting review IPC: ${command}`);
      }
    },
  },
});

function Fixture() {
  const [feedback, setFeedback] = useState("");
  return <>
    <MeetingMinutesPanel onToast={setFeedback} />
    {feedback && <output aria-live="polite" style={{ position: "fixed", right: 12, bottom: 12, maxWidth: "calc(100vw - 24px)", padding: "10px 14px", borderRadius: 8, color: "var(--echo-text-primary)", background: "var(--echo-bg-secondary)", border: "1px solid var(--echo-border)", zIndex: 20 }}>{feedback}</output>}
  </>;
}
createRoot(document.getElementById("root")!).render(<ThemeProvider><Fixture /></ThemeProvider>);
