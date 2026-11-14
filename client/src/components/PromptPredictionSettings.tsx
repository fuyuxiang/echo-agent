import { useEffect, useState } from "react";
import { flattenModels, providersList } from "@/lib/agent-client";
import { usePromptPredictionStore } from "@/stores/prompt-prediction-store";
import type { ModelOption } from "./ModelSelector";

export function PromptPredictionSettings() {
  const { enabled, model, save } = usePromptPredictionStore();
  const [models, setModels] = useState<ModelOption[]>([]);
  const [error, setError] = useState("");
  const [modelError, setModelError] = useState("");
  const [loaded, setLoaded] = useState(false);
  const [reload, setReload] = useState(0);
  const [requested, setRequested] = useState(false);
  useEffect(() => {
    if (!requested) return;
    let active = true;
    void providersList().then(list => {
      if (active) { setModels(flattenModels(list)); setLoaded(true); setModelError(""); }
    }).catch(() => { if (active) setModelError("预测模型列表读取失败"); });
    return () => { active = false; };
  }, [reload, requested]);
  const update = (next: { enabled: boolean; model: string }) => {
    if (!save(next)) setError("预测设置保存失败，请检查本地存储后重试");
    else setError("");
  };
  return <>
    <div className="settings-row settings-row--comfortable">
      <div className="settings-row__label settings-row__label--stacked">
        <span className="settings-row__name">显示输入建议</span>
        <span className="settings-row__description">回复结束后预测下一条消息。按 Tab 填入草稿，编辑后再发送；生成建议可能产生额外模型用量。</span>
      </div>
      <label className="sk-toggle">
        <input type="checkbox" role="switch" aria-label="显示输入建议" checked={enabled}
          onChange={event => update({ enabled: event.target.checked, model })} />
        <span className="sk-toggle-track"><span className="sk-toggle-thumb" /></span>
      </label>
    </div>
    <div className="settings-row settings-row--comfortable">
      <label className="settings-row__label settings-row__label--stacked" htmlFor="prompt-prediction-model">
        <span className="settings-row__name">预测模型</span>
        <span className="settings-row__description">自动选择可用轻量模型，也可自行指定。已有固定预测配置时，以该配置为准。</span>
      </label>
      <select id="prompt-prediction-model" aria-label="预测模型" className="settings-input" disabled={!enabled} value={model}
        onFocus={() => setRequested(true)}
        onChange={event => update({ enabled, model: event.target.value })}>
        <option value="">自动选择轻量模型</option>
        {model && !models.some(item => item.id === model) && <option value={model}>{model}{loaded ? "（当前不可用）" : ""}</option>}
        {models.map(item => <option key={item.id} value={item.id}>{item.label ?? item.id}</option>)}
      </select>
    </div>
    {error && <p className="settings-msg settings-msg--warn" role="alert">{error}</p>}
    {modelError && <p className="settings-msg settings-msg--warn" role="alert">{modelError} <button type="button" className="settings-btn" onClick={() => setReload(value => value + 1)}>重试读取</button></p>}
  </>;
}
