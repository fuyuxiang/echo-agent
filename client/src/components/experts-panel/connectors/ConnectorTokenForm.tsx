/**
 * Token-authorization form — shown when connecting a `auth_mode: "token"`
 * connector (天眼查 / Bugly / 携程问道 etc.). Renders the connector's
 * `token-schema.json` fields; on submit the collected values are injected as
 * env vars into the connector's MCP servers.
 *
 * Mirrors echo-agent's `ConnectorTokenDialog` / detail-panel token form.
 */
import { useRef, useState } from "react";
import type { ConnectorItem } from "@/lib/types";
import { OpenExternalIcon } from "@/foundation/components/Icon/icons";
import { ConnectorIcon } from "../shared/ConnectorIcon";
import { useModalFocus } from "@/lib/use-modal-focus";
import { useUnsavedClose } from "@/lib/use-unsaved-close";
import { openUrl } from "@/lib/agent-client";

interface Props {
  connector: ConnectorItem;
  /** Values saved from a previous install (read back from mcp.json). */
  initialValues?: Record<string, string>;
  onClose: () => void;
  onSubmit: (values: Record<string, string>) => void | Promise<void>;
}

export function ConnectorTokenForm({ connector, initialValues, onClose, onSubmit }: Props) {
  const schema = connector.tokenSchema!;
  const [values, setValues] = useState<Record<string, string>>(initialValues ?? {});
  const [docError, setDocError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [submitError, setSubmitError] = useState<string | null>(null);
  const submittingRef = useRef(false);
  const { requestClose, closeDialog } = useUnsavedClose({
    dirty: schema.fields.some((field) => (values[field.key] ?? "") !== (initialValues?.[field.key] ?? "")),
    busy,
    onClose,
  });
  const close = () => { if (!submittingRef.current) requestClose(); };
  const dialogRef = useModalFocus<HTMLFormElement>(true, close);

  const requiredFields = schema.fields.filter((f) => f.required);
  const allRequiredFilled = requiredFields.every((f) => (values[f.key] ?? "").trim());

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!allRequiredFilled || submittingRef.current) return;
    submittingRef.current = true;
    setBusy(true);
    setSubmitError(null);
    try {
      await onSubmit(values);
    } catch (error) {
      setSubmitError(`连接失败：${String(error).replace(/^Error:\s*/, "")}`);
    } finally {
      submittingRef.current = false;
      setBusy(false);
    }
  };

  const handleOpenDocs = async () => {
    if (!schema.docUrl) return;
    setDocError(null);
    try {
      await openUrl(schema.docUrl);
    } catch (error) {
      setDocError(`无法打开帮助链接：${String(error).replace(/^Error:\s*/, "")}`);
    }
  };

  return (
    <div className="ec-modal-overlay"
      onClick={(e) => { if (e.target === e.currentTarget) close(); }}>
      <form ref={dialogRef} className="ec-modal ec-modal--managed" onSubmit={(event) => void handleSubmit(event)} role="dialog" aria-modal="true" aria-busy={busy} aria-label={schema.title || `${connector.name} 授权`} tabIndex={-1}>
        <button type="button" className="ec-modal-close" onClick={close} disabled={busy} aria-label="关闭">×</button>

        <div className="ec-modal-header">
          <ConnectorIcon local={connector.iconLocal} name={connector.name} size={48} shape="square" />
          <div className="ec-modal-info">
            <div className="ec-modal-title">{schema.title || `${connector.name} 授权`}</div>
          </div>
        </div>

        <div className="ec-modal-body">
        {schema.description && <p className="ec-modal-desc">{schema.description}</p>}
        {schema.docUrl && (
          <button type="button" className="cn-token-doclink" onClick={() => void handleOpenDocs()}>
            <OpenExternalIcon size="sm" /><span>{schema.docLabel || "如何获取？"}</span>
          </button>
        )}
        {docError && <p className="ec-modal-error" role="alert">{docError}</p>}

        <div className="cn-token-fields">
          {schema.fields.map((f) => (
            <label key={f.key} className="cn-token-field">
              <span className="cn-token-label">
                {f.label || f.key}
                {f.required && <span className="cn-token-required">*</span>}
              </span>
              <input
                className="cn-token-input"
                type={f.type === "password" ? "password" : "text"}
                placeholder={f.placeholder || ""}
                value={values[f.key] ?? ""}
                disabled={busy}
                required={f.required}
                onChange={(e) => setValues((v) => ({ ...v, [f.key]: e.target.value }))}
                data-modal-initial-focus={f === schema.fields[0] ? "" : undefined}
              />
              {f.description && <span className="cn-token-help">{f.description}</span>}
            </label>
          ))}
        </div>
        {submitError && <p className="ec-modal-error" role="alert">{submitError}</p>}
        </div>

        <div className="ec-modal-footer">
          <button type="submit" className="ec-modal-summon-btn" disabled={!allRequiredFilled || busy}>
            {busy ? "连接中…" : "连接"}
          </button>
        </div>
      </form>
      {closeDialog}
    </div>
  );
}
