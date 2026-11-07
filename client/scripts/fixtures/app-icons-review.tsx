import React, { useState } from "react";
import { createRoot } from "react-dom/client";
import { ThemeProvider } from "../../src/components/ThemeProvider";
import { EchoMark } from "../../src/components/EchoMark";
import { MessageItem } from "../../src/components/MessageItem";
import { ModelSelector } from "../../src/components/ModelSelector";
import macTray from "../../src-tauri/icons/tray/macos@2x.png";
import windowsTray from "../../src-tauri/icons/tray/windows-64.png";
import "../../src/styles/index.css";

localStorage.setItem("echoagent.theme", new URLSearchParams(location.search).get("theme") || "light");
const models = [{ id: "personal/primary", label: "工作模型", source: "personal" as const },
  { id: "custom/demo", label: "个人模型", source: "personal" as const, providerKind: "openai" }];

function Review() {
  const [state, setState] = useState<"waiting" | "decision" | "error" | "complete">("waiting");
  const complete = state === "complete";
  const [model, setModel] = useState(models[0].id);
  return <ThemeProvider><main className="icon-review" data-testid="scroll-area">
    <style>{`
      .icon-review {height:100%;overflow:auto;padding:28px;background:var(--echo-bg-primary);color:var(--echo-text-strong)}
      .icon-review header {display:flex;align-items:center;gap:18px;margin-bottom:22px}
      .icon-review h1 {margin:0;font-size:24px}.icon-review h2 {font-size:15px;margin:0 0 16px}
      .icon-review section {padding:20px;border:1px solid var(--echo-border-default);border-radius:12px;margin-bottom:16px}
      .icon-review .msg {margin:0;padding:0}.icon-review .msg__body {min-height:44px}
      .icon-review .review-button {margin-top:18px;padding:7px 12px;border:1px solid var(--echo-border-default);border-radius:7px;background:var(--echo-bg-secondary);color:inherit}
      .icon-review .model-selector {max-width:300px}
      .tray-preview {display:flex;flex-wrap:wrap;align-items:center;gap:24px;padding:14px 18px;border-radius:8px}
      .tray-preview + .tray-preview {margin-top:10px}
      .tray-preview--light {background:#f1f2f5;color:#1c2027}.tray-preview--dark {background:#20252c;color:#fff}
      .tray-preview figure {display:grid;justify-items:center;gap:8px;margin:0;font-size:11px}
      .tray-preview img {display:block;object-fit:contain}
      .mac-template {display:block;background:currentColor;mask:url('${macTray}') center/contain no-repeat}
    `}</style>
    <header><img src="/app-icon.png" width={64} height={64} alt="EchoAgent" /><div><h1>EchoAgent</h1><p>应用图标 · 对话头像 · 模型 · 系统托盘</p></div></header>
    <section data-testid="conversation"><h2>聊天头像与等待状态</h2>
      <MessageItem message={{ id: "icon-review", role: "assistant", complete, startedAt: Date.now(),
        parts: complete ? [{ kind: "text", text: "已完成，头像保持原图并停止呼吸提示。" }] : [] }}
        streaming={!complete} waitingForModel={state === "waiting"}
        runtimeError={state === "error" ? "模型连接失败，请重试。" : undefined} sessionId="icon-review" />
      <button className="review-button" onClick={() => setState(state === "waiting" ? "complete" : "waiting")}>{state === "waiting" ? "完成回复" : "继续等待"}</button>
      <button className="review-button" onClick={() => setState("decision")}>等待用户确认</button>
      <button className="review-button" onClick={() => setState("error")}>模拟错误</button>
    </section>
    <section><h2>模型标识</h2><ModelSelector modelId={model} models={models} onModelChange={setModel} ariaLabel="任务模型" /></section>
    <section><h2>Windows 托盘 · 原图缩小</h2>{["light", "dark"].map(background =>
      <div key={background} className={`tray-preview tray-preview--${background}`}>{[16, 20, 24, 32, 64].map(size =>
        <figure key={size}><img src={windowsTray} width={size} height={size} alt="" /><figcaption>{size}px</figcaption></figure>)}</div>)}</section>
    <section><h2>macOS 菜单栏 · 单色模板</h2>{["light", "dark"].map(background =>
      <div key={background} className={`tray-preview tray-preview--${background}`}>{[16, 18, 22, 32, 44].map(size =>
        <figure key={size}><span className="mac-template" style={{ width: size, height: size }} /><figcaption>{size}px</figcaption></figure>)}</div>)}</section>
    <div style={{ height: 900 }} />
    <section data-testid="offscreen"><h2>离屏状态验证</h2><EchoMark active /></section>
  </main></ThemeProvider>;
}

createRoot(document.getElementById("root")!).render(<Review />);
