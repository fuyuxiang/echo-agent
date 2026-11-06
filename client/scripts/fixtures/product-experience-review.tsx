// Explicit synthetic data; never imported by the application entry point.
import React, { useState } from "react";
import { createRoot } from "react-dom/client";
import { SearchOverlay } from "../../src/components/SearchOverlay";
import { KnowledgeBasePanel } from "../../src/components/KnowledgeBasePanel";
import { ThemeProvider } from "../../src/components/ThemeProvider";
import { useSessionsStore } from "../../src/stores/sessions-store";
import { registerKbProvider } from "../../src/lib/knowledge-base";
import "../../src/styles/index.css";

const params = new URLSearchParams(location.search);
localStorage.setItem("echoagent.theme", params.get("theme") || "light");
document.documentElement.style.fontSize = `${16 * Number(params.get("font") || 13) / 13}px`;
const calls: { command: string; args: unknown }[] = [];
let rejectPick = params.get("failure") === "picker";
let rejectSave = params.get("failure") === "remove";
let rejectOpen = params.get("failure") === "open";
let rejectLoad = params.get("failure") === "load";
let pickResolver: ((path: string | null) => void) | null = null;
let searchResolver: (() => void) | null = null;
let openResolver: (() => void) | null = null;
let rebuildResolver: (() => void) | null = null;
Object.assign(window, { productReview: { calls, finishPick: (path: string | null) => pickResolver?.(path),
  searchPending: () => searchResolver !== null, finishSearch: () => searchResolver?.(),
  openPending: () => openResolver !== null, failOpen: () => openResolver?.(),
  rebuildPending: () => rebuildResolver !== null, finishRebuild: () => rebuildResolver?.() } });
Object.assign(window, { __TAURI_INTERNALS__: {
  invoke: async (command: string, args: any) => {
    calls.push({ command, args });
    if (command === "session_search") {
      if (params.get("delayedMatch") === "1" && args.query === "交付复盘") {
        await new Promise<void>(resolve=>{searchResolver=resolve;});
        searchResolver=null;
        return [{sessionId:"review-1",title:"交付复盘",cwd:"/review/客户乙/资料",snippet:"延迟返回的正文命中"}];
      }
      await new Promise(resolve => setTimeout(resolve, 100));
      if (args.query === "失败") throw new Error("隔离测试：索引暂不可用");
      return args.query === "正文" ? [{sessionId:"review-1", title:"交付复盘", cwd:"/review/客户乙/资料", snippet:"正文命中：已确认交付范围与后续行动。"}] : [];
    }
    if (command === "org_local_kb_sources_get") return { exists: true, sources: [{ id:"review-source", kind:"local-folder", root:"/review/产品资料", label:"产品资料", enabled:true }] };
    if (command === "org_local_kb_sources_set") {
      if (rejectSave) { rejectSave = false; throw new Error("隔离测试：保存失败"); }
      return;
    }
    if (command === "filesystem_pick_directory") {
      if (rejectPick) { rejectPick = false; throw new Error("隔离测试：目录选择器不可用"); }
      return new Promise(resolve => { pickResolver = resolve; });
    }
    if (command === "filesystem_resource_identity") return {id:args.path, canonicalPath:args.path};
    if (command === "list_dir") return [];
    if (command === "personal_knowledge_rebuild" && params.get("delayedRebuild") === "1") {
      await new Promise<void>(resolve => { rebuildResolver = resolve; });
      rebuildResolver = null;
      return null;
    }
    if (command.startsWith("personal_knowledge_")) return null;
    throw new Error(`Unexpected fixture IPC: ${command}`);
  },
} });
useSessionsStore.setState({ independent: Array.from({length:65}, (_, index) => ({
  sessionId:`review-${index}`, title:index < 2 ? "交付复盘" : `历史任务 ${String(index + 1).padStart(2,"0")} · 整理与核对资料`,
  cwd:`/review/客户${index % 2 ? "乙" : "甲"}/资料`,
  updatedAt:new Date(Date.UTC(2026,10,6,0,index)).toISOString(),
})).reverse() });
registerKbProvider({ id:"review-source", label:"产品资料", isEnabled:()=>true,
  list:query=>!query ? [] : query === "后续" ? [{id:"/review/产品资料/后续.md",title:"后续资料",url:"/review/产品资料/后续.md"}]
    : [{id:"/review/产品资料/交付.md", title:"交付说明", url:"/review/产品资料/交付.md", snippet:"来源清楚、错误可重试，文件保留在原目录。"}],
});
function Review() {
  const [open, setOpen] = useState(true);
  const [selected, setSelected] = useState("");
  return params.get("surface") === "knowledge" ? <div className="placeholder-page knowledge-page"><KnowledgeBasePanel
    onOpen={async id => {
      if (params.get("delayedOpen") === "1" && id.endsWith("交付.md")) {
        await new Promise<void>((_, reject) => { openResolver = () => {
          openResolver = null;
          reject(new Error("隔离测试：旧文件不可读"));
        }; });
      }
      if (rejectOpen) { rejectOpen = false; throw new Error("隔离测试：文件暂时不可读"); }
      setSelected(id);
    }}
  /><output>{selected}</output></div> : <>
    <button type="button" onClick={()=>setOpen(true)}>打开搜索</button><output>{selected}</output>
    <SearchOverlay open={open} onClose={()=>setOpen(false)} onSelect={async (id,cwd)=>{
      if (rejectLoad) {
        rejectLoad=false;
        throw new Error("隔离测试：会话加载失败" + (params.get("longError") === "1" ? `：${"/review/长目录".repeat(24)}` : ""));
      }
      setSelected(`${id} | ${cwd}`);
    }} />
  </>;
}
createRoot(document.getElementById("root")!).render(<ThemeProvider><Review /></ThemeProvider>);
