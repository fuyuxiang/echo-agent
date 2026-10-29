import { useCallback, useEffect, useId, useMemo, useRef, useState, type CSSProperties } from "react";
import { createPortal } from "react-dom";
import { ChevronDown, Check } from "lucide-react";
import { modelChoiceLabel, modelDisplayName } from "@/lib/model-presentation";
import {
  useAnchoredFloating,
  type FloatingAlignment,
  type FloatingPlacement,
  type FloatingWidth,
} from "@/lib/use-anchored-floating";

/**
 * Model picker dropdown for the Composer. Shows the current model name on a
 * trigger button; clicking opens a small menu listing every available model.
 * Selecting one calls onModelChange.
 */
export interface ModelOption {
  id: string;
  label?: string;
  /** Exact model slug sent to the upstream, independent of the display name. */
  remoteModelId?: string;
  /** Optional provider kind, used to group/sort models in the dropdown. */
  providerKind?: string;
  /** Optional provider id this model belongs to. */
  providerId?: string;
  providerLabel?: string;
  source?: "personal" | "organization" | "builtin" | "legacy";
  /** The selected connection sends credentials and content over plaintext HTTP. */
  insecureHttp?: boolean;
}

export function ModelSelector({
  modelId,
  modelLoading = false,
  disabled = false,
  models,
  onModelChange,
  ariaLabel,
  menuPlacement = "top",
  menuAlign = "end",
  menuWidth = "content",
  menuZIndex,
}: {
  /** Internal model id. The trigger displays its friendly name. */
  modelId?: string;
  modelLoading?: boolean;
  disabled?: boolean;
  models: ModelOption[];
  onModelChange: (id: string) => void;
  /** Distinguishes selectors when a page exposes project and task model choices together. */
  ariaLabel?: string;
  /** Floating-menu preferences for form fields that differ from Composer placement. */
  menuPlacement?: FloatingPlacement;
  menuAlign?: FloatingAlignment;
  menuWidth?: FloatingWidth;
  /** Use the dialog-local layer when this portalled menu is launched inside a modal. */
  menuZIndex?: CSSProperties["zIndex"];
}) {
  const [open, setOpen] = useState(false);
  const reactId = useId();
  const listboxId = `model-selector-${reactId.replace(/:/g, "")}`;
  const ref = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLUListElement>(null);
  const optionRefs = useRef(new Map<string, HTMLButtonElement>());
  const duplicateNames = useMemo(() => {
    const seen = new Set<string>();
    const duplicates = new Set<string>();
    for (const model of models) {
      const name = modelDisplayName(model);
      if (seen.has(name)) duplicates.add(name);
      seen.add(name);
    }
    return duplicates;
  }, [models]);
  // Keep everyday choices concise; only actual name collisions need context.
  const choiceLabel = (model: ModelOption) => duplicateNames.has(modelDisplayName(model))
    ? modelChoiceLabel(model, models) : modelDisplayName(model);
  const connectionLabel = (model: ModelOption) => {
    if (!duplicateNames.has(modelDisplayName(model))) return undefined;
    return choiceLabel(model).slice(modelDisplayName(model).length + " · ".length);
  };
  const { style: menuStyle, placement } = useAnchoredFloating(
    triggerRef,
    menuRef,
    open,
    {
      preferredPlacement: menuPlacement,
      align: menuAlign,
      width: menuWidth,
      maxWidth: menuWidth === "content" ? 320 : undefined,
      estimatedHeight: Math.min(320, Math.max(52, models.length * 48 + 8)),
      offset: 6,
      zIndex: menuZIndex,
    },
  );

  const close = useCallback((restoreFocus = true) => {
    setOpen(false);
    if (restoreFocus) triggerRef.current?.focus();
  }, []);

  useEffect(() => {
    if (!open) return;
    const selected = modelId ? optionRefs.current.get(modelId) : undefined;
    if (selected) selected.focus();
    else if (!ref.current?.contains(document.activeElement) || document.activeElement === triggerRef.current) {
      optionRefs.current.get(models[0]?.id)?.focus();
    }
  }, [modelId, models, open]);

  // Close on outside click.
  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      const target = e.target as Node | null;
      if (!target) return;
      if (ref.current?.contains(target) || menuRef.current?.contains(target)) return;
      close(false);
    };
    document.addEventListener("mousedown", onDown);
    return () => document.removeEventListener("mousedown", onDown);
  }, [close, open]);

  useEffect(() => {
    if (!open) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        close();
      }
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [close, open]);

  const handleMenuKeyDown = (event: React.KeyboardEvent<HTMLUListElement>) => {
    const index = models.findIndex((model) => optionRefs.current.get(model.id) === document.activeElement);
    const focusAt = (nextIndex: number) => {
      if (models.length === 0) return;
      optionRefs.current.get(models[(nextIndex + models.length) % models.length].id)?.focus();
    };
    if (event.key === "ArrowDown") {
      event.preventDefault();
      focusAt(index + 1);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      focusAt(index - 1);
    } else if (event.key === "Home") {
      event.preventDefault();
      focusAt(0);
    } else if (event.key === "End") {
      event.preventDefault();
      focusAt(models.length - 1);
    }
  };

  const current = models.find((m) => m.id === modelId);
  const hasModels = models.length > 0;
  const triggerLabel = modelLoading
    ? "正在同步模型…"
    : hasModels
    ? current ? modelDisplayName(current) : "请选择模型"
    : "未配置模型";

  return (
    <div className="model-selector" ref={ref}>
      <button
        className="model-selector__trigger"
        onClick={(event) => {
          event.stopPropagation();
          setOpen((o) => !o);
        }}
        type="button"
        disabled={disabled || modelLoading || !hasModels}
        aria-busy={modelLoading}
        aria-label={ariaLabel ? `${ariaLabel}：${triggerLabel}` : undefined}
        aria-description={current && !modelLoading ? choiceLabel(current) : undefined}
        title={!hasModels ? "请先在设置中配置模型" : current && !modelLoading ? choiceLabel(current) : triggerLabel}
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-controls={open ? listboxId : undefined}
        ref={triggerRef}
        onKeyDown={(event) => {
          if (event.key === "ArrowDown" || event.key === "ArrowUp") {
            event.preventDefault();
            setOpen(true);
          }
        }}
      >
        <span className="model-selector__label">{triggerLabel}</span>
        <ChevronDown size={14} strokeWidth={1.75} className="model-selector__arrow" />
      </button>
      {open && !modelLoading && typeof document !== "undefined" && createPortal(
        <ul
          ref={menuRef}
          className="model-selector__menu"
          id={listboxId}
          role="listbox"
          aria-label={ariaLabel ?? "选择模型"}
          style={menuStyle}
          data-placement={placement ?? undefined}
          onClick={(event) => event.stopPropagation()}
          onKeyDown={handleMenuKeyDown}
        >
          {models.length === 0 && (
            <li className="model-selector__empty">未配置模型</li>
          )}
          {models.map((m) => (
            <li key={m.id} role="none">
              <button
                type="button"
                className={
                  "model-selector__item" +
                  (m.id === modelId ? " model-selector__item--active" : "")
                }
                onClick={(event) => {
                  event.stopPropagation();
                  onModelChange(m.id);
                  close();
                }}
                role="option"
                data-model-id={m.id}
                aria-label={choiceLabel(m)}
                title={choiceLabel(m)}
                aria-selected={m.id === modelId}
                ref={(element) => {
                  if (element) optionRefs.current.set(m.id, element);
                  else optionRefs.current.delete(m.id);
                }}
              >
                <span className="model-selector__item-label">{modelDisplayName(m)}</span>
                {connectionLabel(m) && <span className="model-selector__connection">{connectionLabel(m)}</span>}
                {m.id === modelId && <Check size={14} className="model-selector__check" />}
              </button>
            </li>
          ))}
        </ul>,
        document.body,
      )}
    </div>
  );
}
