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

  const confirm = useCallback((opts: ConfirmOptions) => {
    // A second request while one is open resolves the first as cancelled, so no
    // caller is left awaiting a promise that never settles.
    resolverRef.current?.(false);
    setPending(opts);
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
      {pending && <ConfirmDialog options={pending} onSettle={settle} />}
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

  // Focus the confirming action and honour Escape: without a keyboard path the
  // dialog would be a trap for anyone not using a mouse.
  useEffect(() => {
    confirmRef.current?.focus();
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onSettle(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onSettle]);

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
