import { useEffect, useRef, useState } from "react";

type PlanDraft = { text: string; edited: boolean };

function readDraft(key: string, initial: string): PlanDraft {
  try {
    const stored = localStorage.getItem(key);
    if (stored === null) return { text: initial, edited: false };
    try {
      const value = JSON.parse(stored);
      if (value?.kind === "echo-coding-plan-draft" && typeof value.text === "string" && typeof value.edited === "boolean") {
        return { text: !value.edited && initial ? initial : value.text, edited: value.edited };
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
  onExecute,
  onSave,
}: {
  scope: string;
  initial: string;
  busy: boolean;
  onExecute: (text: string) => Promise<boolean>;
  onSave: (text: string) => void;
}) {
  const storageKey = `echo-coding-plan-v1:${scope}`;
  const [draft, setDraft] = useState(() => readDraft(storageKey, initial));
  const { text } = draft;
  const previousInitial = useRef(initial);
  const [error, setError] = useState("");
  useEffect(() => {
    if (previousInitial.current === initial) return;
    previousInitial.current = initial;
    if (initial) setDraft(current => current.edited ? current : { text: initial, edited: false });
  }, [initial]);
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
      <textarea
        aria-label="编辑开发计划"
        value={text}
        onChange={(event) => setDraft({ text: event.target.value, edited: true })}
        rows={9}
      />
      {error && <p role="alert">{error}</p>}
      <div>
        {initial && initial !== text && (
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
