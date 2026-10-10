import {
  useCallback,
  useId,
  useLayoutEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { createPortal } from "react-dom";
import { useModalFocus } from "@/lib/use-modal-focus";

type DialogAction = () => void | Promise<void>;

interface DialogBaseOptions {
  title: string;
  description?: ReactNode;
  confirmLabel?: string;
  cancelLabel?: string;
  danger?: boolean;
  /** Also surfaces the failure elsewhere (for example in the app toast). */
  onError?: (error: unknown) => void;
  /** Focus target when the dialog was opened from a menu that immediately unmounts. */
  returnFocus?: HTMLElement | null;
}

export interface ConfirmationOptions extends DialogBaseOptions {
  action: DialogAction;
}

export interface PromptField {
  name: string;
  label: string;
  defaultValue?: string;
  placeholder?: string;
  multiline?: boolean;
  required?: boolean;
  maxLength?: number;
  rows?: number;
}

export interface PromptOptions extends DialogBaseOptions {
  fields: PromptField[];
  validate?: (values: Record<string, string>) => string | null | undefined;
  action: (values: Record<string, string>) => void | Promise<void>;
}

type DialogRequest =
  | ({ id: number; kind: "confirmation"; resolveDecision?: (confirmed: boolean) => void } & ConfirmationOptions)
  | ({ id: number; kind: "prompt" } & PromptOptions);
type DialogRequestInput =
  | ({ kind: "confirmation"; resolveDecision?: (confirmed: boolean) => void } & ConfirmationOptions)
  | ({ kind: "prompt" } & PromptOptions);

/**
 * Hosts one application-native action dialog for a panel. Actions execute
 * while the dialog remains open, so failures can be read and retried instead
 * of disappearing behind a native browser prompt.
 */
export function useAppDialog(scopeKey?: string | number | null) {
  const [request, setRequest] = useState<DialogRequest | null>(null);
  const nextId = useRef(1);
  const previousScope = useRef(scopeKey);

  useLayoutEffect(() => {
    if (Object.is(previousScope.current, scopeKey)) return;
    previousScope.current = scopeKey;
    // A confirmation belongs to the entity/workspace that opened it. Never
    // let a late click execute its captured action after navigation reuses the
    // same panel instance for another scope. A layout effect removes the stale
    // action before the changed scope can be painted or clicked.
    setRequest((current) => {
      current?.kind === "confirmation" && current.resolveDecision?.(false);
      return null;
    });
  }, [scopeKey]);

  const openRequest = useCallback((next: DialogRequestInput) => {
    setRequest((current) => {
      // There is intentionally only one application dialog per host. If a
      // newer workflow replaces an awaitable confirmation, settle the old one
      // as cancelled instead of leaving its caller suspended forever.
      if (current?.kind === "confirmation") current.resolveDecision?.(false);
      return { ...next, id: nextId.current } as DialogRequest;
    });
    nextId.current += 1;
  }, []);

  useLayoutEffect(() => () => {
    if (request?.kind === "confirmation") request.resolveDecision?.(false);
  }, [request]);

  const requestConfirmation = useCallback((options: ConfirmationOptions) => {
    openRequest({ kind: "confirmation", ...options });
  }, [openRequest]);

  /**
   * Awaitable confirmation for workflows that must continue only after the
   * user has made a decision (for example replace-all and workspace exit).
   * Ordinary callers can keep using `requestConfirmation` with an action.
   */
  const confirm = useCallback((options: Omit<ConfirmationOptions, "action">) =>
    new Promise<boolean>((resolve) => {
      openRequest({
        kind: "confirmation",
        ...options,
        action: () => undefined,
        resolveDecision: resolve,
      });
    }), [openRequest]);

  const requestInput = useCallback((options: PromptOptions) => {
    openRequest({ kind: "prompt", ...options });
  }, [openRequest]);

  const close = useCallback((requestId: number, confirmed: boolean) => {
    // An async action can finish after navigation has invalidated its dialog
    // and a newer request has opened. Only its own completion may close state.
    setRequest((current) => {
      if (current?.id !== requestId) return current;
      if (current.kind === "confirmation") current.resolveDecision?.(confirmed);
      return null;
    });
  }, []);

  return {
    requestConfirmation,
    confirm,
    requestInput,
    dialog: request && typeof document !== "undefined"
      ? createPortal(
        <AppActionDialog
          key={request.id}
          request={request}
          onClose={(confirmed) => close(request.id, confirmed)}
        />,
        document.body,
      )
      : null,
  };
}

function AppActionDialog({
  request,
  onClose,
}: {
  request: DialogRequest;
  onClose: (confirmed: boolean) => void;
}) {
  const titleId = useId();
  const descriptionId = useId();
  const errorId = useId();
  const [busy, setBusy] = useState(false);
  const submittingRef = useRef(false);
  const [error, setError] = useState<string | null>(null);
  const [values, setValues] = useState<Record<string, string>>(() =>
    request.kind === "prompt"
      ? Object.fromEntries(request.fields.map((field) => [field.name, field.defaultValue ?? ""]))
      : {},
  );
  const { requestConfirmation: confirmDiscard, dialog: discardDialog } = useAppDialog();

  const cancel = useCallback(() => {
    if (submittingRef.current) return;
    if (request.kind === "prompt" && request.fields.some((field) => values[field.name] !== (field.defaultValue ?? ""))) {
      confirmDiscard({
        title: "舍弃未保存的修改？",
        description: "当前输入尚未保存。关闭后将丢失这些修改。",
        cancelLabel: "继续编辑",
        confirmLabel: "舍弃修改",
        danger: true,
        action: () => onClose(false),
      });
      return;
    }
    onClose(false);
  }, [confirmDiscard, onClose, request, values]);
  const dialogRef = useModalFocus<HTMLDivElement>(true, cancel, request.returnFocus);

  const execute = async () => {
    if (submittingRef.current) return;
    if (request.kind === "prompt") {
      const missing = request.fields.find((field) => field.required && !values[field.name]?.trim());
      const tooLong = request.fields.find((field) => (
        field.maxLength !== undefined
        && (values[field.name]?.length ?? 0) > field.maxLength
      ));
      const validationError = missing
        ? `请填写“${missing.label}”。`
        : tooLong
          ? `“${tooLong.label}”不能超过 ${tooLong.maxLength} 个字符。`
        : request.validate?.(values);
      if (validationError) {
        setError(validationError);
        return;
      }
    }

    submittingRef.current = true;
    setBusy(true);
    setError(null);
    try {
      if (request.kind === "confirmation") await request.action();
      else await request.action(values);
      onClose(true);
    } catch (actionError) {
      const message = String(actionError).replace(/^Error:\s*/, "") || "操作失败，请重试。";
      setError(message);
      try {
        request.onError?.(actionError);
      } catch {
        // Reporting must never turn a handled action failure into an unhandled
        // promise rejection or make the retry controls unusable.
      }
    } finally {
      submittingRef.current = false;
      setBusy(false);
    }
  };

  const describedBy = [request.description ? descriptionId : null, error ? errorId : null]
    .filter(Boolean)
    .join(" ") || undefined;

  return (
    <>
    <div
      className="app-dialog-overlay"
      onClick={(event) => {
        // Portal events follow the React tree. Stop them here so cancelling a
        // nested prompt cannot also close its owning settings/editor modal.
        event.stopPropagation();
        cancel();
      }}
    >
      <div
        ref={dialogRef}
        className={`app-dialog${request.kind === "prompt" ? " app-dialog--prompt" : ""}${request.danger ? " app-dialog--danger" : ""}`}
        role={request.danger ? "alertdialog" : "dialog"}
        aria-modal="true"
        aria-labelledby={titleId}
        aria-describedby={describedBy}
        aria-busy={busy}
        tabIndex={-1}
        onClick={(event) => event.stopPropagation()}
        onKeyDown={(event) => {
          if (
            request.kind === "prompt"
            && event.key === "Enter"
            && !event.shiftKey
            && !event.nativeEvent.isComposing
            && event.nativeEvent.keyCode !== 229
            // Enter on the cancel/confirm buttons must retain native button
            // semantics. Only a single-line field opts into quick submit.
            && event.target instanceof HTMLInputElement
          ) {
            event.preventDefault();
            void execute();
          }
        }}
      >
        <header className="app-dialog__header">
          <h2 id={titleId} className="app-dialog__title">{request.title}</h2>
          <button type="button" className="app-dialog__close" aria-label="关闭对话框" onClick={cancel} disabled={busy}>×</button>
        </header>
        <div className="app-dialog__body">
        {request.description && (
          <div id={descriptionId} className="app-dialog__description">{request.description}</div>
        )}

        {request.kind === "prompt" && (
          <div className="app-dialog__fields">
            {request.fields.map((field, index) => {
              const inputId = `${titleId}-${field.name}`;
              const common = {
                id: inputId,
                value: values[field.name] ?? "",
                placeholder: field.placeholder,
                maxLength: field.maxLength,
                disabled: busy,
                required: field.required,
                "data-modal-initial-focus": index === 0 ? true : undefined,
                onChange: (event: React.ChangeEvent<HTMLInputElement | HTMLTextAreaElement>) => {
                  setValues((current) => ({ ...current, [field.name]: event.target.value }));
                  if (error) setError(null);
                },
              };
              return (
                <label key={field.name} className="app-dialog__field" htmlFor={inputId}>
                  <span>{field.label}{field.required ? <span aria-hidden="true"> *</span> : null}</span>
                  {field.multiline ? (
                    <textarea {...common} rows={field.rows ?? 5} />
                  ) : (
                    <input {...common} type="text" />
                  )}
                </label>
              );
            })}
          </div>
        )}

        {error && <div id={errorId} className="app-dialog__error" role="alert">{error}</div>}
        </div>

        <div className="app-dialog__actions">
          <button
            type="button"
            className="app-dialog__button app-dialog__button--cancel"
            onClick={cancel}
            disabled={busy}
            data-modal-initial-focus={request.kind === "confirmation" ? true : undefined}
          >
            {request.cancelLabel ?? "取消"}
          </button>
          <button
            type="button"
            className={`app-dialog__button ${request.danger ? "app-dialog__button--danger" : "app-dialog__button--primary"}`}
            onClick={() => void execute()}
            disabled={busy}
          >
            {busy ? "处理中…" : request.confirmLabel ?? "确定"}
          </button>
        </div>
      </div>
    </div>
    {discardDialog}
    </>
  );
}
