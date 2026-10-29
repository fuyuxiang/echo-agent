import React, { useState } from "react";
import { createRoot } from "react-dom/client";
import { ModelSelector, type ModelOption } from "../../src/components/ModelSelector";
import { modelChoiceLabel } from "../../src/lib/model-presentation";
import "../../src/styles/index.css";

const query = new URLSearchParams(location.search);
document.documentElement.dataset.theme = query.get("theme") || "light";
document.documentElement.classList.toggle("dark", query.get("theme") === "dark");

const models: ModelOption[] = query.get("catalog") === "aliases" ? [
  {id:"gateway/fast",label:"通用模型",remoteModelId:"fast",providerId:"gateway",providerLabel:"我的网关",source:"personal"},
  {id:"gateway/strong",label:"通用模型",remoteModelId:"strong",providerId:"gateway",providerLabel:"我的网关",source:"personal"},
] : query.get("catalog") === "distinct" ? [
  ...["Echo Reed", "Echo Brook", "Echo Cedar", "Echo Grove", "Echo Willow"].map(name => ({id:`echoagent-ojlab/${name}`,source:"builtin" as const})),
  {id:"organization/MiniMax-M3.1-Flash",remoteModelId:"MiniMax-M3.1-Flash",source:"organization",providerLabel:"组织提供"},
] : [
  { id: "echoagent-ojlab/Echo Reed", source: "builtin" },
  { id: "organization/Echo Reed", source: "organization", providerLabel: "研发团队" },
  { id: "custom-2/Echo Reed", remoteModelId: "Echo Reed", source: "personal", providerLabel: "我的网关" },
  { id: "custom-3/Echo Reed", remoteModelId: "Echo Reed", source: "personal", providerLabel: "备用网关" },
  { id: "organization/Qwen/Qwen3-32B", remoteModelId: "Qwen/Qwen3-32B", source: "organization", providerLabel: "组织的一个较长连接名称，用于验证窄窗口中的换行显示" },
  { id: "custom-2/long-model", label: "一个较长的模型显示名称，用于验证菜单里的完整阅读体验".repeat(2), remoteModelId: "long-model", source: "personal", providerLabel: "个人模型网关" },
];

function Review() {
  const [id, setId] = useState(models[0].id);
  const selected = models.find((model) => model.id === id)!;
  return <main style={{ padding: 24, height: "100%", background: "var(--echo-bg-primary)", color: "var(--echo-text-strong)", overflow: "auto" }}>
    <div style={{ width: "min(100%, 420px)", marginLeft: "auto", paddingTop: 40 }}>
      <h2>任务模型</h2>
      <ModelSelector modelId={id} models={models} onModelChange={setId} ariaLabel="任务模型" />
      <p data-testid="selected" data-model-id={id} style={{ overflowWrap: "anywhere" }}>{modelChoiceLabel(selected, models)}</p>
      <label style={{ display: "grid", gap: 8 }}>项目默认模型
        <select value={id} onChange={(event) => setId(event.target.value)} style={{ maxWidth: "100%" }}>
          {models.map((model) => <option key={model.id} value={model.id}>{modelChoiceLabel(model, models)}</option>)}
        </select>
      </label>
    </div>
  </main>;
}

createRoot(document.getElementById("root")!).render(<Review />);
