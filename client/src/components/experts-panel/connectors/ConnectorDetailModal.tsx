/**
 * Connector detail modal — shown when clicking a connector card.
 * Shows the icon, name, description, example prompts, the bundled mcp.json
 * config (when present), and a "配置连接" button that opens the MCP 管理 modal.
 */
import { useEffect, useRef, useState } from "react";
import type { ConnectorItem } from "@/lib/types";
import { connectorsReadMcpConfig } from "@/lib/agent-client";
import { ConnectorIcon } from "../shared/ConnectorIcon";
import { ConfigureIcon } from "@/foundation/components/Icon/icons";
import type { ConnectorAuthState } from "./ConnectorsTab";
import { useModalFocus } from "@/lib/use-modal-focus";
import { copyShareText } from "@/lib/share";

interface Props {
  connector: ConnectorItem;
  /** Catalog root (needed to read the bundled mcp.json). */
  root: string;
  /** Current authorization state (drives badge + button labels). */
  authState?: ConnectorAuthState;
  onClose: () => void;
  /** Connect / authorize entry point. */
  onConfigure: () => void;
  /** 取消授权 (CLI connectors only). */
  onUnauth?: () => void;
  onToast?: (m: string) => void;
}

const STATE_LABEL: Record<ConnectorAuthState, string | null> = {
  none: null,
  installed: "已连接",
  authed: "已授权",
  "needs-auth": "待授权",
};

export function ConnectorDetailModal({
  connector, root, authState = "none", onClose, onConfigure, onUnauth, onToast,
}: Props) {
  const [mcpConfig, setMcpConfig] = useState<string>("");
  const [configError, setConfigError] = useState<string | null>(null);
  const [configReloadKey, setConfigReloadKey] = useState(0);
  const [copyingPrompt, setCopyingPrompt] = useState<number | null>(null);
  const copyPendingRef = useRef(false);
  const copyGenerationRef = useRef(0);
  const dialogRef = useModalFocus<HTMLDivElement>(true, onClose);

  useEffect(() => {
    copyGenerationRef.current += 1;
    copyPendingRef.current = false;
    setCopyingPrompt(null);
    return () => {
      copyGenerationRef.current += 1;
      copyPendingRef.current = false;
    };
  }, [connector.id]);

  const copyExample = async (prompt: string, index: number) => {
    if (copyPendingRef.current) return;
    copyPendingRef.current = true;
    setCopyingPrompt(index);
    const generation = copyGenerationRef.current;
    let copied = false;
    try {
      copied = await copyShareText(prompt);
    } catch {
      // A host without either clipboard path still needs retryable feedback.
    }
    if (generation !== copyGenerationRef.current) return;
    copyPendingRef.current = false;
    setCopyingPrompt(null);
    onToast?.(copied ? "已复制到剪贴板" : "复制失败，请检查剪贴板权限");
  };

  // Load the bundled mcp.json (if any) for the config preview.
  useEffect(() => {
    let disposed = false;
    setMcpConfig("");
    setConfigError(null);
    if (!root || !connector.source) return;
    connectorsReadMcpConfig(root, connector.source)
      .then((txt) => { if (!disposed) setMcpConfig(txt); })
      .catch((reason) => {
        if (!disposed) {
          setMcpConfig("");
          setConfigError(String(reason).replace(/^Error:\s*/, ""));
        }
      });
    return () => { disposed = true; };
  }, [root, connector.source, configReloadKey]);

  const examples = (connector.examplesZh ?? []).filter(Boolean).slice(0, 8);
  const stateLabel = STATE_LABEL[authState];
  const primaryLabel =
    authState === "needs-auth" ? "去授权"
    : authState === "installed" || authState === "authed" ? "重新授权"
    : connector.authMode || connector.kind === "cli" ? "授权连接"
    : "配置连接";

  return (
    <div
      className="ec-modal-overlay"
      onClick={(e) => { if (e.target === e.currentTarget) onClose(); }}
    >
      <div ref={dialogRef} className="ec-modal ec-modal--managed" role="dialog" aria-modal="true" aria-label={`${connector.name} 连接器详情`} tabIndex={-1}>
        <button className="ec-modal-close" onClick={onClose} aria-label="关闭" data-modal-initial-focus>×</button>

        <div className="ec-modal-header">
          <ConnectorIcon local={connector.iconLocal} name={connector.name} size={64} shape="square" />
          <div className="ec-modal-info">
            <div className="ec-modal-title">{connector.name}</div>
            <div className="ec-modal-meta">
              <span>{connector.kind === "unknown" ? "连接器" : connector.kind.toUpperCase()}</span>
              {connector.authMode && <><span className="ec-modal-dot">·</span><span>需授权</span></>}
              {stateLabel && (
                <><span className="ec-modal-dot">·</span>
                <span className={`cn-badge ${authState === "needs-auth" ? "cn-badge--warn" : "cn-badge--ok"}`}>
                  {stateLabel}
                </span></>
              )}
              {connector.nameEn && connector.nameEn !== connector.name && (
                <><span className="ec-modal-dot">·</span><span>{connector.nameEn}</span></>
              )}
            </div>
          </div>
        </div>

        <div className="ec-modal-body">
        {connector.desc && (
          <div className="ec-modal-section">
            <div className="ec-modal-section-title">能力介绍</div>
            <p className="ec-modal-desc">{connector.desc}</p>
          </div>
        )}

        {examples.length > 0 && (
          <div className="ec-modal-section">
            <div className="ec-modal-section-title">可以这样问我</div>
            <div className="ec-modal-quick-prompts">
              {examples.map((qp, i) => (
                <button
                  key={i}
                  type="button"
                  className="ec-modal-qp-btn"
                  disabled={copyingPrompt !== null}
                  aria-busy={copyingPrompt === i}
                  aria-label={`复制示例提问：${qp}`}
                  title={copyingPrompt === i ? "正在复制…" : "复制示例提问"}
                  onClick={() => void copyExample(qp, i)}
                >
                  <span className="ec-modal-qp-text">"{qp}"{copyingPrompt === i ? " · 复制中…" : ""}</span>
                </button>
              ))}
            </div>
          </div>
        )}

        {mcpConfig && (
          <div className="ec-modal-section">
            <div className="ec-modal-section-title">MCP 配置</div>
            <pre className="cn-modal-code"><code>{mcpConfig}</code></pre>
          </div>
        )}
        {configError && (
          <div className="cn-auth-error" role="alert">
            <p>MCP 配置预览加载失败：{configError}</p>
            <button type="button" className="um-btn um-btn--grey" onClick={() => setConfigReloadKey((value) => value + 1)}>
              重试加载配置
            </button>
          </div>
        )}

        </div>
        <div className="ec-modal-footer cn-modal-actions">
          <button className="ec-modal-summon-btn" onClick={onConfigure}>
            <ConfigureIcon size="sm" /><span style={{ marginLeft: 6 }}>{primaryLabel}</span>
          </button>
          {connector.kind === "cli" && authState === "authed" && onUnauth && (
            <button className="um-btn um-btn--grey" onClick={onUnauth}>取消授权</button>
          )}
        </div>
      </div>
    </div>
  );
}
