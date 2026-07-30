import { createContext, useCallback, useContext, useEffect, useRef, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { AlertTriangle } from "lucide-react";

export interface ConfirmOptions {
  title: string;
  /** What is about to happen and what it costs. Deletes here are irreversible,
   *  so say so rather than relying on the user recognising the icon. */
  message: string;
  /** Label for the confirming action, e.g. "Delete". Defaults to common:confirm. */
  confirmLabel?: string;
  /** Styles the confirm button as destructive. */
  destructive?: boolean;
}

type Resolver = (confirmed: boolean) => void;

const ConfirmContext = createContext<((opts: ConfirmOptions) => Promise<boolean>) | null>(null);

/**
 * Async confirmation dialog.
 *
 * Deletes across Memory / Knowledge / Cron used to fire on a single click of a
 * Trash icon sitting inches from the row's other controls — irreversible, and
 * in the knowledge case triggering a full index rebuild as well. A promise-based
 * dialog keeps call sites as a single `await confirm(...)` line while giving a
 * real, stylable, testable prompt instead of the native window.confirm.
 */
export function ConfirmProvider({ children }: { children: ReactNode }) {
  const [pending, setPending] = useState<ConfirmOptions | null>(null);
  const resolverRef = useRef<Resolver | null>(null);
  // Distinguishes one request from the next so each gets a fresh dialog
  // instance — see the `key` on ConfirmDialog below.
  const [requestId, setRequestId] = useState(0);

  const confirm = useCallback((opts: ConfirmOptions) => {
    // A second request while one is open resolves the first as cancelled, so no
    // caller is left awaiting a promise that never settles.
    resolverRef.current?.(false);
    setPending(opts);
    setRequestId((n) => n + 1);
    return new Promise<boolean>((resolve) => {
      resolverRef.current = resolve;
    });
  }, []);

  const settle = useCallback((confirmed: boolean) => {
    resolverRef.current?.(confirmed);
    resolverRef.current = null;
    setPending(null);
  }, []);

  return (
    <ConfirmContext.Provider value={confirm}>
      {children}
      {/* Keyed by request, so replacing one dialog with another remounts rather
          than re-rendering. Without it React reuses the instance and the focus
          effect only re-runs when its dependencies change — two consecutive
          destructive dialogs (delete a job, then delete another) share the same
          deps, so the second one kept whatever focus the first left behind,
          losing the "destructive dialogs focus the safe action" guarantee
          exactly where it matters. A remount also drops any stale local state
          instead of leaking it into the next question. */}
      {pending && (
        <ConfirmDialog key={requestId} options={pending} onSettle={settle} />
      )}
    </ConfirmContext.Provider>
  );
}

/** Returns a function that opens the dialog and resolves to the user's choice. */
export function useConfirm() {
  const ctx = useContext(ConfirmContext);
  if (!ctx) throw new Error("useConfirm must be used within ConfirmProvider");
  return ctx;
}

function ConfirmDialog({
  options,
  onSettle,
}: {
  options: ConfirmOptions;
  onSettle: (confirmed: boolean) => void;
}) {
  const { t } = useTranslation("common");
  const confirmRef = useRef<HTMLButtonElement>(null);
  const cancelRef = useRef<HTMLButtonElement>(null);

  // Focus the SAFE action for destructive dialogs and honour Escape. Pre-focusing
  // the confirm button meant a stray Enter or Space — or a keyboard user landing
  // here mid-scroll — performed the irreversible action outright. Non-destructive
  // dialogs keep confirm focused, where the fast path is the intended one.
  useEffect(() => {
    if (options.destructive) cancelRef.current?.focus();
    else confirmRef.current?.focus();
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onSettle(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onSettle, options.destructive]);

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/30 p-4"
      onClick={() => onSettle(false)}
    >
      <div
        role="dialog"
        aria-modal="true"
        aria-labelledby="confirm-title"
        aria-describedby="confirm-message"
        className="bg-white rounded-lg shadow-lg max-w-md w-full p-5"
        onClick={(e) => e.stopPropagation()}
      >
        <div id="confirm-title" className="flex items-center gap-2 font-semibold mb-2">
          {options.destructive && <AlertTriangle size={18} className="text-red-500" />}
          {options.title}
        </div>
        <p id="confirm-message" className="text-sm text-gray-600 whitespace-pre-line">
          {options.message}
        </p>
        <div className="flex justify-end gap-2 mt-5">
          <button
            ref={cancelRef}
            onClick={() => onSettle(false)}
            className="px-3 py-1.5 rounded text-sm bg-gray-100 hover:bg-gray-200"
          >
            {t("cancel")}
          </button>
          <button
            ref={confirmRef}
            onClick={() => onSettle(true)}
            className={`px-3 py-1.5 rounded text-sm text-white ${
              options.destructive ? "bg-red-600 hover:bg-red-700" : "bg-blue-600 hover:bg-blue-700"
            }`}
          >
            {options.confirmLabel ?? t("confirm")}
          </button>
        </div>
      </div>
    </div>
  );
}
