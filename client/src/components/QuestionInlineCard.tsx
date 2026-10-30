import { useCallback, useEffect, useId, useRef, useState } from "react";
import { Check, ChevronLeft, ChevronRight, Clock3, MessageCircleQuestion, PencilLine, X } from "lucide-react";
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
  const [activeIndex, setActiveIndex] = useState(0);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [remainingSeconds, setRemainingSeconds] = useState<number | null>(null);
  const requestScope = head ? `${head.sessionId}:${head.requestId}` : null;
  const currentScopeRef = useRef(requestScope);
  currentScopeRef.current = requestScope;
  const resolvingScopesRef = useRef(new Set<string>());
  const questionId = useId();
  const bodyRef = useRef<HTMLDivElement>(null);
  const headingRef = useRef<HTMLHeadingElement>(null);
  const focusOnStepRef = useRef(false);

  useEffect(() => {
    setSelections({});
    setCustomInputs({});
    setActiveIndex(0);
    focusOnStepRef.current = false;
    setBusy(requestScope !== null && resolvingScopesRef.current.has(requestScope));
    setError(null);
  }, [requestScope]);

  useEffect(() => {
    if (bodyRef.current) bodyRef.current.scrollTop = 0;
    if (focusOnStepRef.current) {
      headingRef.current?.focus({ preventScroll: true });
      focusOnStepRef.current = false;
    }
  }, [activeIndex, requestScope]);

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
      if (!question.multiSelect) {
        return { ...previous, [question.id]: previous[question.id]?.[0] === label ? [] : [label] };
      }
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

  const questionIndex = Math.min(activeIndex, Math.max(0, head.questions.length - 1));
  const question = head.questions[questionIndex];
  const hasCurrentAnswer = question && (
    (selections[question.id]?.length ?? 0) > 0 || Boolean(customInputs[question.id]?.trim())
  );
  const lastQuestion = questionIndex === head.questions.length - 1;
  const expired = remainingSeconds === 0;
  const navigate = (index: number) => {
    if (busy || expired || index < 0 || index >= head.questions.length || index === questionIndex) return;
    focusOnStepRef.current = true;
    setActiveIndex(index);
  };
  const advance = () => {
    if (lastQuestion) void resolve({ outcome: "accepted", ...buildAccepted() });
    else navigate(questionIndex + 1);
  };
  const preview = question && !question.multiSelect
    ? question.options.find((option) => option.label === selections[question.id]?.[0])?.preview
    : undefined;

  return (
    <section
      className="question-inline"
      aria-label={head.title || "Agent 提问"}
      onKeyDown={(event) => {
        if (!question || busy || expired || event.nativeEvent.isComposing || event.nativeEvent.keyCode === 229
          || event.ctrlKey || event.metaKey || event.altKey
          || (event.target as HTMLElement).closest("input, textarea, [contenteditable=true]")) return;
        if (!/^[1-9]$/.test(event.key)) return;
        const option = question.options[Number(event.key) - 1];
        if (option) {
          event.preventDefault();
          handleSelect(question, option.label);
        }
      }}
    >
      <div className="question-inline__head">
        <MessageCircleQuestion size={17} className="question-inline__icon" aria-hidden="true" />
        <span className="question-inline__title" title={head.title}>问题</span>
        <div className="question-inline__progress">
          {remainingSeconds != null && !expired && (
            <span className="question-inline__timeout" title={`剩余 ${remainingSeconds} 秒`}>
              <Clock3 size={12} aria-hidden="true" />
              {remainingSeconds < 60 ? `剩余 ${remainingSeconds} 秒`
                : `${Math.floor(remainingSeconds / 60)}:${String(remainingSeconds % 60).padStart(2, "0")}`}
            </span>
          )}
          <nav className="question-inline__navigation" aria-label="问题导航">
            {head.questions.length > 1 && <button type="button" className="question-inline__icon-button"
              aria-label="上一题" title="上一题" disabled={busy || expired || questionIndex === 0}
              onClick={() => navigate(questionIndex - 1)}><ChevronLeft size={16} aria-hidden="true" /></button>}
            <span className="question-inline__count" role="status" aria-live="polite"
              aria-label={`第 ${question ? questionIndex + 1 : 0} 题，共 ${head.questions.length} 题`}>
              {question ? questionIndex + 1 : 0} / {head.questions.length}
            </span>
            {head.questions.length > 1 && <button type="button" className="question-inline__icon-button"
              aria-label="下一题" title="下一题" disabled={busy || expired || lastQuestion}
              onClick={() => navigate(questionIndex + 1)}><ChevronRight size={16} aria-hidden="true" /></button>}
          </nav>
        </div>
        <button type="button" className="question-inline__icon-button question-inline__close"
          aria-label="取消" title="取消本次提问" disabled={busy || expired}
          onClick={() => void resolve({ outcome: "cancelled", cancelled: true })}>
          <X size={16} aria-hidden="true" />
        </button>
      </div>
      <div
        ref={bodyRef}
        className="question-inline__body"
        role="region"
        aria-label="当前问题"
        tabIndex={0}
      >
        {expired && (
          <p className="question-inline__notice" role="status">已超时，正在等待 Agent 结束本次提问…</p>
        )}
        {question && (
          <div className="question-inline__question">
            <h3 ref={headingRef} id={questionId} tabIndex={-1} className="question-inline__question-text">
              {question.question}
            </h3>
            {question.multiSelect && <p className="question-inline__hint">可选择多项，也可以补充说明</p>}
            {question.options.length > 0 && (
              <div className="question-inline__options" role="group" aria-labelledby={questionId}>
                {question.options.map((option, index) => {
                  const selected = selections[question.id]?.includes(option.label) ?? false;
                  const recommended = /[（(]\s*(推荐|recommended)\s*[）)]\s*$/i.test(option.label);
                  const label = recommended ? option.label.replace(/[（(]\s*(推荐|recommended)\s*[）)]\s*$/i, "").trim() : option.label;
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
                      aria-keyshortcuts={index < 9 ? String(index + 1) : undefined}
                    >
                      <span className="question-inline__option-number" aria-hidden="true">{index + 1}</span>
                      <span className="question-inline__option-copy">
                        <span className="question-inline__option-label">{label}{recommended && <span className="question-inline__recommended">推荐</span>}</span>
                        {option.description && <span className="question-inline__option-description">{option.description}</span>}
                      </span>
                      <span className="question-inline__option-check" aria-hidden="true">{selected && <Check size={16} />}</span>
                    </button>
                  );
                })}
              </div>
            )}
            {preview && <pre className="question-inline__preview">{preview}</pre>}
          </div>
        )}
        {error && <p className="question-inline__error" role="alert">{error}</p>}
      </div>
      <div className="question-inline__footer">
        <div className="question-inline__answer-row">
          {question && <label className="question-inline__custom">
            <PencilLine size={16} aria-hidden="true" />
            <input
              type="text" className="question-inline__custom-input"
              aria-label={question.multiSelect ? "补充回答" : "自定义回答"}
              placeholder={question.multiSelect ? "也可以补充说明…" : "或自行填写回复…"}
              value={customInputs[question.id] ?? ""}
              onChange={(event) => handleCustomInput(question, event.target.value)}
              onKeyDown={(event) => {
                if (event.nativeEvent.isComposing || event.nativeEvent.keyCode === 229) return;
                if (event.key === "Enter" && hasCurrentAnswer && !busy && !expired) {
                  event.preventDefault();
                  advance();
                }
              }}
              disabled={busy || expired}
            />
          </label>}
          <div className="question-inline__actions">
            {!hasCurrentAnswer && <button type="button" className="question-inline__action"
              onClick={advance} disabled={busy || expired || !question}>
              {lastQuestion && head.questions.length > 1 ? "跳过并提交" : "跳过"}
            </button>}
            <button type="button" className="question-inline__action question-inline__action--primary"
              onClick={advance} disabled={busy || expired || !hasCurrentAnswer}>
              {busy ? "提交中…" : lastQuestion ? "提交" : "下一步"}
            </button>
          </div>
        </div>
        {head.mode === "plan" && (
          <div className="question-inline__plan-actions">
            <button
              type="button" className="question-inline__plan-action"
              onClick={() => void resolve({
                outcome: "chat_about_this",
                partialAnswers: buildPartialAnswers(),
              })}
              disabled={busy || expired}
            >
              聊聊这些选择
            </button>
            <button
              type="button" className="question-inline__plan-action"
              aria-label="跳过访谈直接规划" title="跳过剩余问题，使用已填写的选择直接规划"
              onClick={() => void resolve({
                outcome: "skip_interview",
                partialAnswers: buildPartialAnswers(),
              })}
              disabled={busy || expired}
            >
              直接规划
            </button>
          </div>
        )}
      </div>
    </section>
  );
}
