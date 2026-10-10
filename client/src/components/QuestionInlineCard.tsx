import { useCallback, useEffect, useRef, useState } from "react";
import {
  useQuestionStore,
  selectQuestionForSession,
  type QuestionItem,
} from "@/stores/question-store";
import {
  agentListPendingInteractions,
  agentResolveQuestion,
  onQuestionClosedEvent,
} from "@/lib/agent-client";

/** Inline, session-scoped UI for the blocking AskUserQuestion ExtMethod. */
export function QuestionInlineCard({ sessionId }: { sessionId: string | null }) {
  const head = useQuestionStore(selectQuestionForSession(sessionId));
  const dismiss = useQuestionStore((state) => state.dismiss);
  const [selections, setSelections] = useState<Record<string, string[]>>({});
  const [customInputs, setCustomInputs] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [remainingSeconds, setRemainingSeconds] = useState<number | null>(null);
  const requestScope = head ? `${head.sessionId}:${head.requestId}` : null;
  const currentScopeRef = useRef(requestScope);
  currentScopeRef.current = requestScope;
  const resolvingScopesRef = useRef(new Set<string>());

  useEffect(() => {
    setSelections({});
    setCustomInputs({});
    setBusy(requestScope !== null && resolvingScopesRef.current.has(requestScope));
    setError(null);
  }, [requestScope]);

  useEffect(() => {
    if (!head || head.timeout == null) {
      setRemainingSeconds(null);
      return;
    }

    const deadline = Date.now() + Math.max(0, head.timeout) * 1_000;
    const update = () => {
      setRemainingSeconds(Math.max(0, Math.ceil((deadline - Date.now()) / 1_000)));
    };
    update();
    const timer = window.setInterval(update, 250);
    return () => window.clearInterval(timer);
  }, [head?.requestId, head?.timeout]);

  useEffect(() => {
    if (!head) return;
    const requestId = head.requestId;
    const ownerSessionId = head.sessionId;
    let cancelled = false;
    let unlisten: (() => void) | undefined;

    void (async () => {
      unlisten = await onQuestionClosedEvent((event) => {
        if (event.requestId === requestId && event.sessionId === ownerSessionId) {
          dismiss(requestId, ownerSessionId);
        }
      });
      if (cancelled) {
        unlisten();
        return;
      }

      // A close event can be missed while this session is not mounted. The
      // retained backend registry is authoritative, so remove stale cards.
      const pending = await agentListPendingInteractions(ownerSessionId);
      if (
        !cancelled &&
        !pending.questions.some((question) => question.requestId === requestId)
      ) {
        dismiss(requestId, ownerSessionId);
      }
    })().catch((cause) => {
      console.warn("failed to synchronize pending question", cause);
    });

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [dismiss, head?.requestId, head?.sessionId]);

  const handleSelect = useCallback((question: QuestionItem, label: string) => {
    setSelections((previous) => {
      if (!question.multiSelect) return { ...previous, [question.id]: [label] };
      const selected = previous[question.id] ?? [];
      const next = selected.includes(label)
        ? selected.filter((item) => item !== label)
        : [...selected, label];
      return { ...previous, [question.id]: next };
    });
    if (!question.multiSelect) {
      setCustomInputs((previous) => ({ ...previous, [question.id]: "" }));
    }
  }, []);

  const handleCustomInput = useCallback((question: QuestionItem, value: string) => {
    setCustomInputs((previous) => ({ ...previous, [question.id]: value }));
    if (!question.multiSelect && value.trim()) {
      setSelections((previous) => ({ ...previous, [question.id]: [] }));
    }
  }, []);

  if (!head) return null;

  const buildAccepted = () => {
    const answers: Record<string, string[]> = {};
    const annotations: Record<string, { preview?: string; notes?: string }> = {};
    for (const question of head.questions) {
      const key = question.question || question.id;
      const selected = [...(selections[question.id] ?? [])];
      const custom = (customInputs[question.id] ?? "").trim();
      if (custom && (question.multiSelect || selected.length === 0)) selected.push("Other");
      if (selected.length === 0) continue;
      answers[key] = selected;

      const preview = !question.multiSelect
        ? question.options.find((option) => option.label === selected[0])?.preview
        : undefined;
      if (preview || custom) {
        annotations[key] = {
          ...(preview ? { preview } : {}),
          ...(custom ? { notes: custom } : {}),
        };
      }
    }
    return {
      answers,
      annotations: Object.keys(annotations).length ? annotations : undefined,
    };
  };

  const buildPartialAnswers = () => {
    const partial: Record<string, string> = {};
    for (const question of head.questions) {
      const selected = [...(selections[question.id] ?? [])];
      const custom = (customInputs[question.id] ?? "").trim();
      if (custom && (question.multiSelect || selected.length === 0)) selected.push("Other");
      if (selected.length) partial[question.question || question.id] = selected.join(", ");
    }
    return partial;
  };

  const resolve = async (payload: Parameters<typeof agentResolveQuestion>[1]) => {
    if (!requestScope || resolvingScopesRef.current.has(requestScope) || remainingSeconds === 0) return;
    const requestId = head.requestId;
    const ownerSessionId = head.sessionId;
    resolvingScopesRef.current.add(requestScope);
    setBusy(true);
    setError(null);
    try {
      const acknowledged = await agentResolveQuestion(requestId, payload);
      if (!acknowledged) throw new Error("后端未找到该提问请求，请重试");
      dismiss(requestId, ownerSessionId);
    } catch (cause) {
      console.error("resolve question failed", cause);
      if (currentScopeRef.current === requestScope) {
        setError(String(cause).replace(/^Error:\s*/, ""));
      }
    } finally {
      resolvingScopesRef.current.delete(requestScope);
      if (currentScopeRef.current === requestScope) setBusy(false);
    }
  };

  const hasAnswer = head.questions.some(
    (question) =>
      (selections[question.id]?.length ?? 0) > 0 ||
      Boolean(customInputs[question.id]?.trim()),
  );
  const expired = remainingSeconds === 0;

  return (
    <div className="question-inline">
      <div className="question-inline__head">
        <span className="question-inline__icon">?</span>
        <span className="question-inline__title">{head.title || "Agent 提问"}</span>
        {head.questions.length > 1 && (
          <span className="question-inline__count">{head.questions.length} 个问题</span>
        )}
      </div>
      <div
        className="question-inline__body"
        role="region"
        aria-label="Agent 问题列表"
        tabIndex={0}
      >
        {remainingSeconds != null && (
          <p className="question-inline__timeout" aria-live="polite">
            {expired
              ? "已超时，正在等待 Agent 结束本次提问…"
              : `剩余 ${remainingSeconds} 秒`}
          </p>
        )}
        {head.questions.map((question) => (
          <div key={question.id} className="question-inline__question">
            <p className="question-inline__question-text">
              {question.question}
              {question.multiSelect && <span> · 可多选</span>}
            </p>
            {question.options.length > 0 && (
              <div className="question-inline__options">
                {question.options.map((option) => {
                  const selected = selections[question.id]?.includes(option.label) ?? false;
                  return (
                    <button
                      key={option.id ?? option.label}
                      type="button"
                      className={
                        "question-inline__option" +
                        (selected ? " question-inline__option--selected" : "")
                      }
                      onClick={() => handleSelect(question, option.label)}
                      disabled={busy || expired}
                      aria-pressed={selected}
                    >
                      <span>{option.label}</span>
                      {option.description && (
                        <small className="question-inline__option-description">
                          {option.description}
                        </small>
                      )}
                    </button>
                  );
                })}
              </div>
            )}
            {!question.multiSelect && selections[question.id]?.[0] && (() => {
              const preview = question.options.find(
                (option) => option.label === selections[question.id]?.[0],
              )?.preview;
              return preview ? (
                <pre className="question-inline__preview">{preview}</pre>
              ) : null;
            })()}
            <input
              type="text"
              className="question-inline__custom-input"
              placeholder={question.multiSelect ? "输入补充回答…" : "输入自定义回答…"}
              value={customInputs[question.id] ?? ""}
              onChange={(event) => handleCustomInput(question, event.target.value)}
              onKeyDown={(event) => {
                if (event.nativeEvent.isComposing || event.nativeEvent.keyCode === 229) return;
                if (event.key === "Enter" && hasAnswer && !busy && !expired) {
                  event.preventDefault();
                  void resolve({ outcome: "accepted", ...buildAccepted() });
                }
              }}
              disabled={busy || expired}
            />
          </div>
        ))}
        {error && <p className="question-inline__error" role="alert">{error}</p>}
      </div>
      <div className="question-inline__footer">
        <button
          className="btn btn--ghost"
          onClick={() => void resolve({ outcome: "cancelled", cancelled: true })}
          disabled={busy || expired}
        >
          取消
        </button>
        {head.mode === "plan" && (
          <>
            <button
              className="btn btn--ghost"
              onClick={() => void resolve({
                outcome: "chat_about_this",
                partialAnswers: buildPartialAnswers(),
              })}
              disabled={busy || expired}
            >
              聊聊这些选择
            </button>
            <button
              className="btn btn--ghost"
              onClick={() => void resolve({
                outcome: "skip_interview",
                partialAnswers: buildPartialAnswers(),
              })}
              disabled={busy || expired}
            >
              跳过访谈直接规划
            </button>
          </>
        )}
        <button
          className="btn btn--primary"
          onClick={() => void resolve({ outcome: "accepted", ...buildAccepted() })}
          disabled={busy || expired || !hasAnswer}
        >
          提交
        </button>
      </div>
    </div>
  );
}
