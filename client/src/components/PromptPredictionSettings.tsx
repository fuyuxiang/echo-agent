import { Select } from "@/components/Select";
import { useState } from "react";
import type { ModelOptionRow } from "@/lib/agent-client";
import { usePromptPredictionStore } from "@/stores/prompt-prediction-store";

export function PromptPredictionSettings({ models, onRetry }: {
  models?: Pick<ModelOptionRow, "id" | "label">[];
  onRetry?: () => void;
}) {
  const { enabled, model, save } = usePromptPredictionStore();
  const [error, setError] = useState("");
  const update = (next: { enabled: boolean; model: string }) => {
    if (!save(next)) setError("预测设置保存失败，请检查本地存储后重试");
    else setError("");
  };
  return <>
    <div className="settings-row settings-row--comfortable">
      <div className="settings-row__label settings-row__label--stacked">
        <span className="settings-row__name">显示输入建议</span>
        <span className="settings-row__description">回复后建议下一条消息，可能产生额外模型用量。</span>
      </div>
      <label className="sk-toggle">
        <input type="checkbox" role="switch" aria-label="显示输入建议" checked={enabled}
          onChange={event => update({ enabled: event.target.checked, model })} />
        <span className="sk-toggle-track"><span className="sk-toggle-thumb" /></span>
      </label>
    </div>
    <div className="settings-row settings-row--comfortable">
      <label className="settings-row__label settings-row__label--stacked" htmlFor="prompt-prediction-model">
        <span className="settings-row__name">建议模型</span>
        <span id="prompt-prediction-model-hint" className="settings-row__description" aria-live="polite">
          {enabled && !models && !onRetry ? "正在读取建议模型…" : enabled && models?.length === 0 ? "没有可手动选择的模型，可保留自动选择。" : "已有固定配置时优先使用固定模型。"}
        </span>
      </label>
      <Select id="prompt-prediction-model" aria-label="建议模型" className="settings-input" aria-describedby="prompt-prediction-model-hint" disabled={!enabled || !models} value={model}
        onChange={event => update({ enabled, model: event.target.value })}>
        <option value="">自动选择轻量模型</option>
        {model && !models?.some(item => item.id === model) && <option value={model}>{model}{models ? "（当前不可用）" : ""}</option>}
        {models?.map(item => <option key={item.id} value={item.id}>{item.label ?? item.id}</option>)}
      </Select>
    </div>
    {enabled && onRetry && <p className="settings-msg settings-msg--warn" role="alert">
      建议模型读取失败。<button type="button" className="settings-btn" onClick={onRetry}>重试读取</button>
    </p>}
    {error && <p className="settings-msg settings-msg--warn" role="alert">{error}</p>}
  </>;
}
