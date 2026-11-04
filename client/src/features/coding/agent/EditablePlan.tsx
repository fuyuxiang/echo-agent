import { useEffect, useRef, useState } from "react";

type PlanDraft = { text: string; edited: boolean };

function readDraft(key: string, initial: string, interrupted: boolean): PlanDraft {
  try {
    const stored = localStorage.getItem(key);
    if (stored === null) return { text: initial, edited: false };
    try {
      const value = JSON.parse(stored);
      if (value?.kind === "echo-coding-plan-draft" && typeof value.text === "string" && typeof value.edited === "boolean") {
        return { text: !value.edited ? interrupted ? "" : initial || value.text : value.text, edited: value.edited };
      }
    } catch { /* Earlier drafts store the user's text directly. */ }
    return { text: stored, edited: true };
  } catch {
    return { text: initial, edited: false };
  }
}

/** Plans live in application storage; execution passes the final text to the same session. */
export function EditablePlan({
  scope,
  initial,
  busy,
  interrupted = false,
  onExecute,
  onSave,
}: {
  scope: string;
  initial: string;
  busy: boolean;
  interrupted?: boolean;
  onExecute: (text: string) => Promise<boolean>;
  onSave: (text: string) => void;
}) {
  const storageKey = `echo-coding-plan-v1:${scope}`;
  const [draft, setDraft] = useState(() => readDraft(storageKey, initial, interrupted));
  // Hide obsolete automatic content before effects run, so the interruption
  // notice and the disabled execute action become visible in the same commit.
  const text = interrupted && !draft.edited ? "" : draft.text;
  const previousInitial = useRef(initial);
  const [error, setError] = useState("");
  useEffect(() => {
    if (interrupted) {
      previousInitial.current = initial;
      setDraft(current => current.edited || !current.text ? current : { text: "", edited: false });
      return;
    }
    if (previousInitial.current === initial) return;
    previousInitial.current = initial;
    if (initial) setDraft(current => current.edited ? current : { text: initial, edited: false });
  }, [initial, interrupted]);
  useEffect(() => {
    try {
      localStorage.setItem(storageKey, JSON.stringify({ kind: "echo-coding-plan-draft", ...draft }));
      setError("");
    } catch {
      setError("计划尚未保存到应用数据，请复制后重试");
    }
  }, [storageKey, draft]);
  return (
    <details className="coding-editable-plan" open>
      <summary>本会话计划 · 可编辑</summary>
      {interrupted && <p className="coding-editable-plan__notice" role="status">本轮计划未完整生成，未自动采用中断内容。
        {draft.edited && text.trim() ? "你的草稿已保留，请核对后执行。" : "请重新生成方案，或直接编写计划。"}
      </p>}
      <textarea
        aria-label="编辑开发计划"
        value={text}
        onChange={(event) => setDraft({ text: event.target.value, edited: true })}
        rows={9}
      />
      {error && <p role="alert">{error}</p>}
      <div>
        {!interrupted && initial && initial !== text && (
          <button
            type="button"
            disabled={busy}
            onClick={() => setDraft({ text: initial, edited: false })}
          >
            采用最新方案
          </button>
        )}
        <button
          type="button"
          disabled={busy || !text.trim()}
          onClick={() => void onExecute(text)}
        >
          执行最终计划
        </button>
        <button
          type="button"
          disabled={busy || !text.trim()}
          onClick={() => onSave(text)}
        >
          保存到工程…
        </button>
      </div>
    </details>
  );
}
