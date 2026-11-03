import "../../src/styles/index.css";
// Production UI/recorder with explicit in-memory IPC. Audio is a synthetic WAV;
// no device, credentials, account or external service is touched.
import React, { useState } from "react";
import { createRoot } from "react-dom/client";
import { MeetingMinutesPanel } from "../../src/components/MeetingMinutesPanel";
import { PageChromeProvider } from "../../src/components/PageToolbar";
import { ThemeProvider } from "../../src/components/ThemeProvider";
import type { MeetingRecord } from "../../src/lib/meeting-minutes";

const query = new URLSearchParams(location.search);
const scenario = query.get("scenario") ?? "completed";
localStorage.setItem("echoagent.theme", query.get("theme") ?? "light");
localStorage.removeItem("echo.meeting.recording-source");
const rate = 16000, seconds = 60;
const audioBytes = new ArrayBuffer(44 + rate * seconds * 2), wav = new DataView(audioBytes);
const text = (offset: number, value: string) => [...value].forEach((letter, index) => wav.setUint8(offset + index, letter.charCodeAt(0)));
text(0, "RIFF"); wav.setUint32(4, audioBytes.byteLength - 8, true); text(8, "WAVEfmt "); wav.setUint32(16, 16, true); wav.setUint16(20, 1, true); wav.setUint16(22, 1, true); wav.setUint32(24, rate, true); wav.setUint32(28, rate * 2, true); wav.setUint16(32, 2, true); wav.setUint16(34, 16, true); text(36, "data"); wav.setUint32(40, audioBytes.byteLength - 44, true);
for (let index = 0; index < rate * seconds; index += 1) wav.setInt16(44 + index * 2, Math.sin(index * Math.PI * 2 * 220 / rate) * 250, true);
const audioUrl = URL.createObjectURL(new Blob([audioBytes], { type: "audio/wav" }));
const base: MeetingRecord = {
  id: "019a0000-0000-7000-8000-000000000001", title: "产品评审周会", createdAt: Date.now(), updatedAt: Date.now(),
  status: scenario.startsWith("recording") ? "recording" : scenario === "failed" ? "partial" : "completed",
  modelId: "review/MiniMax-M3", providerId: "review", durationSeconds: 60, recordedSamples: rate * 60,
  captureSource: "both", audioPath: "/isolated-review/recording.wav", chunks: [],
  transcript: Array.from({ length: scenario === "long" ? 1800 : 6 }, (_, index) => ({ id: index, start: index * (scenario === "long" ? 4 : 8), end: (index + 1) * (scenario === "long" ? 4 : 8), speaker: index % 2 ? "林悦" : "陈晨", text: `第 ${index + 1} 段：优先完成录音、短句增量转写和应用内回听。结束后自动生成纪要，人工校对的内容需要可靠保留。` })),
  minutes: scenario === "failed" || scenario.startsWith("recording") ? undefined : "## 会议摘要\n\n本次评审确定，实时转写与页面内回听作为首期核心能力。\n\n## 结论与待办\n\n- 结束录音后自动整理纪要。[00:00:08]\n- 林悦于本周五完成交互稿。[00:00:16]\n\n## 待确认\n\n电脑声音采集的各平台支持范围。",
  live: { processedSamples: scenario.startsWith("recording") ? rate * 40 : rate * 60 },
  error: scenario === "failed" ? "服务连接暂时中断" : undefined,
};
let record = { ...base };
let records = scenario === "empty" ? [] : [record];
let nativeCapture = scenario.startsWith("recording");
let paused = false;
let pendingSave = false;
let job = false;
let liveCalls = 0;
let stopFailures = scenario === "save-error" ? 1 : 0;
let credentialsRepaired=scenario!=='recording-credentials';
const calls: { command: string; args: Record<string, unknown> }[] = [];
const update = (next: Partial<MeetingRecord>) => { record = { ...record, ...next, updatedAt: Date.now() }; records = [record]; return record; };
Object.assign(window, {
  __meetingWorkbench: { calls, getRecord: () => record, replaceTranscript: () => update({ transcript: record.transcript.map((segment,index) => index === 0 ? { ...segment, text: "服务端更正后的原文" } : segment) }) },
  __TAURI_INTERNALS__: {
    transformCallback: () => 1,
    convertFileSrc: () => audioUrl,
    invoke: async (command: string, args: Record<string, unknown> = {}) => {
      calls.push({ command, args });
      switch (command) {
        case "meeting_list": return records;
        case "meeting_get": return record;
        case "meeting_capture_support": return { systemAudio: true };
        case "meeting_job_active": return job;
        case "meeting_audio_token": if (scenario === "audio-error") throw new Error("missing audio"); return "fixture-token";
        case "meeting_create": return update({ ...base, title: String(args.title), status: "recording", recordedSamples: 0, durationSeconds: 0, transcript: [], minutes: undefined, live: { processedSamples: 0 }, error: undefined, captureSource: args.captureSource as MeetingRecord["captureSource"] });
        case "meeting_capture_start": nativeCapture = true; paused = false; return { meetingId: record.id, active: true, paused, level: .3, recordedSamples: record.recordedSamples };
        case "meeting_capture_status":
          if (!nativeCapture) return null;
          if (!paused) update({ recordedSamples: record.recordedSamples + rate / 2, durationSeconds: record.durationSeconds + .5 });
          return { meetingId: record.id, active: !pendingSave, paused, pendingSave, error: pendingSave ? "磁盘空间不足，录音等待保存" : undefined, level: paused ? 0 : .35, recordedSamples: record.recordedSamples };
        case "meeting_capture_pause": paused = Boolean(args.paused); return update({ status: paused ? "paused" : "recording" });
        case "meeting_capture_stop":
          if (stopFailures-- > 0) { pendingSave = true; paused = true; throw new Error("磁盘空间不足"); }
          nativeCapture = false; pendingSave = false; return update({ status: "recorded" });
        case "meeting_transcribe_live": {
          liveCalls += 1;
          if(!credentialsRepaired)throw new Error('会议模型 API Key 未配置或已失效');
          if (scenario === "recording-error" && liveCalls < 2) throw new Error("temporary unavailable");
          const start = (record.live?.processedSamples ?? 0) / rate;
          if (record.durationSeconds - start < 1 && !args.finalize) return { record, waiting: true };
          await new Promise(resolve => setTimeout(resolve, 150));
          const end = record.durationSeconds;
          if (end > start) update({ transcript: [...record.transcript, { id: record.transcript.length, start, end, speaker: "说话人1", text: `增量转写 ${liveCalls}：录音保存与文字更新相互独立。` }], live: { processedSamples: record.recordedSamples } });
          return { record, waiting: false };
        }
        case 'meeting_set_transcription_connection':return update({modelId:String(args.modelId),providerId:String(args.providerId)});
        case "meeting_process":
          job = true; update({ status: "summarizing", error: undefined });
          await new Promise(resolve => setTimeout(resolve, 900));
          job = false; return update({ status: "completed", minutes: base.minutes ?? "## 会议摘要\n\n短句转写已完成，结束后自动生成纪要。", error: undefined, minutesStale: false });
        case "meeting_regenerate_minutes": return update({ previousMinutes: record.minutes, minutes: "## 更新后的纪要\n\n已根据校对原文重新整理。", minutesStale: false });
        case "meeting_restore_minutes": return update({ minutes: record.previousMinutes, previousMinutes: record.minutes });
        case "meeting_update": {
          if (args.expectedContent) {
            const expected = args.expectedContent as { transcript: MeetingRecord["transcript"]; minutes: string | null };
            if (JSON.stringify(expected.transcript) !== JSON.stringify(record.transcript) || expected.minutes !== (record.minutes ?? null)) throw new Error("会议内容已更新，请合并草稿");
          }
          return update({ ...(args.title ? { title: String(args.title) } : {}), ...(args.minutes ? { previousMinutes: record.minutes, minutes: String(args.minutes), minutesStale: false } : {}), ...(args.transcript ? { transcript: args.transcript as MeetingRecord["transcript"], minutesStale: Boolean(record.minutes) } : {}) });
        }
        case "meeting_export": return "/isolated-review/会议资料.docx";
        case "meeting_delete": records = []; return;
        case "meeting_open_audio": return;
        default: throw new Error(`Unhandled workbench IPC: ${command}`);
      }
    },
  },
});
function Fixture() {
  const [collapsed, setCollapsed] = useState(query.get("collapsed") === "1");
  const [feedback, setFeedback] = useState("");
  return <div className="meeting-review-shell">
    {!collapsed && <aside className="meeting-review-sidebar"><strong>EchoAgent</strong><span>新建任务</span><span>项目</span><span>录音转写</span></aside>}
    <PageChromeProvider sidebarCollapsed={collapsed} macOS={query.get("platform") === "macos"} onToggleSidebar={() => setCollapsed(value => !value)} onNewTask={() => setFeedback("新建任务")}>
      <MeetingMinutesPanel models={[{ id: "review/MiniMax-M3", providerId: "review", source: "personal" }]} onToast={setFeedback} onOpenModelSettings={()=>{credentialsRepaired=true;setFeedback('连接已在设置中修复，请重试转写');}} />
    </PageChromeProvider>
    {feedback && <output className="meeting-review-feedback">{feedback}</output>}
  </div>;
}
const style = document.createElement("style"); style.textContent = `html,body,#root{height:100%;margin:0} .meeting-review-shell{height:100%;display:flex;min-width:0} .meeting-review-sidebar{width:200px;flex-shrink:0;background:var(--echo-bg-secondary);padding:24px 18px;box-sizing:border-box;display:flex;flex-direction:column;gap:28px;border-right:1px solid var(--echo-border)} .meeting-review-feedback{pointer-events:none;position:fixed;right:12px;bottom:12px;padding:8px 12px;max-width:calc(100% - 24px);background:var(--echo-bg-secondary);color:var(--echo-text-primary);z-index:30;border:1px solid var(--echo-border);border-radius:8px} @media(max-width:700px){.meeting-review-sidebar{display:none}}`;
document.head.append(style);
document.documentElement.style.fontSize = `${query.get("font") ?? "14"}px`;
createRoot(document.getElementById("root")!).render(<ThemeProvider><Fixture /></ThemeProvider>);
