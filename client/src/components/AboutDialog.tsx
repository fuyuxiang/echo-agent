import { modelDisplayName, modelSourceLabel } from "@/lib/model-presentation";
/**
 * 关于 EchoAgent 对话框 - 显示版本、运行时和模型配置状态。
 *
 * 从 agentInit() 的 InitResult 取 agentVersion，从 agentAuthStatus() 取模型状态。
 */
import { useEffect, useState } from "react";
import { XCloseIcon, CheckIcon } from "@/foundation/components/Icon/icons";
import { agentAuthStatus } from "@/lib/agent-client";
import type { InitResult } from "@/lib/agent-client";
import { APP_VERSION } from "@/lib/app-version";
import { isUpstreamBrandedModelId, stripUpstreamBrandedIds } from "@/lib/model-branding";
import { useModalFocus } from "@/lib/use-modal-focus";
const logoMarkUrl = "/app-icon.png";
const RUNTIME_LABEL = "EchoAgent Runtime（内置）";

interface AboutDialogProps {
  open: boolean;
  onClose: () => void;
  init?: InitResult | null;
  onCheckForUpdates?: () => void;
}

export function AboutDialog({ open, onClose, init, onCheckForUpdates }: AboutDialogProps) {
  const [authReady, setAuthReady] = useState<boolean | null>(null);
  const [providers, setProviders] = useState<string[]>([]);
  const [runtimeReason, setRuntimeReason] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    agentAuthStatus()
      .then((s) => {
        setAuthReady(s.ready);
        // 兜底：上游品牌模型不进入这一行。就绪状态仍取后端原值，过滤只影响显示。
        setProviders(stripUpstreamBrandedIds(s.providers));
        setRuntimeReason(s.reason ?? null);
      })
      .catch((error) => {
        setAuthReady(false);
        setRuntimeReason(String(error).replace(/^Error:\s*/, ""));
      });
  }, [open]);
  const dialogRef = useModalFocus<HTMLDivElement>(open, onClose);

  if (!open) return null;

  return (
    <div className="about-dialog__overlay" onClick={onClose}>
      <div
        ref={dialogRef}
        className="about-dialog"
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-modal="true"
        aria-label="关于 EchoAgent"
        tabIndex={-1}
      >
        <button className="about-dialog__close" onClick={onClose} aria-label="关闭" data-modal-initial-focus>
          <XCloseIcon size="md" />
        </button>
        <div className="about-dialog__header">
          <img
            src={logoMarkUrl}
            alt="EchoAgent"
            className="about-dialog__logo"
            width={48}
            height={48}
          />
          <div>
            <h2 className="about-dialog__title">EchoAgent</h2>
            <p className="about-dialog__subtitle">
              开源桌面 Agent 工作台
            </p>
          </div>
        </div>
        <dl className="about-dialog__list">
          <div className="about-dialog__row">
            <dt>版本</dt>
            <dd>v{APP_VERSION}</dd>
          </div>
          <div className="about-dialog__row">
            <dt>构建标识</dt>
            <dd><code>{init?.buildCommit ?? "unknown"}</code></dd>
          </div>
          <div className="about-dialog__row">
            <dt>构建时间</dt>
            <dd title={init?.buildTime}>{init?.buildTime ?? "unknown"}</dd>
          </div>
          <div className="about-dialog__row">
            <dt>提交时间</dt>
            <dd title={init?.buildCommitTime}>{init?.buildCommitTime ?? "unknown"}</dd>
          </div>
          <div className="about-dialog__row">
            <dt>Agent 运行时</dt>
            <dd>{init?.agentVersion ?? "未知"}</dd>
          </div>
          <div className="about-dialog__row">
            <dt>默认模型</dt>
            <dd>
              {isUpstreamBrandedModelId(init?.defaultModelId)
                ? "未指定"
                : modelDisplayName(init?.defaultModelId, "未指定")}
            </dd>
          </div>
          <div className="about-dialog__row">
            <dt>工作目录</dt>
            <dd title={init?.cwd}>{init?.cwd ?? "—"}</dd>
          </div>
          <div className="about-dialog__row">
            <dt>运行时</dt>
            <dd>{RUNTIME_LABEL}</dd>
          </div>
          <div className="about-dialog__row">
            <dt>模型状态</dt>
            <dd>
              {authReady === null ? (
                "检查中…"
              ) : authReady ? (
                <span className="about-dialog__ok">
                  <CheckIcon size="sm" /> 就绪
                </span>
              ) : (
                <span className="about-dialog__warn" title={runtimeReason ?? undefined}>未就绪</span>
              )}
            </dd>
          </div>
          {providers.length > 0 && (
            <div className="about-dialog__row">
              <dt>已配置模型</dt>
              <dd>{providers.map((id) => `${modelDisplayName(id)} · ${modelSourceLabel(id)}`).join(", ")}</dd>
            </div>
          )}
          <div className="about-dialog__row">
            <dt>诊断日志</dt>
            <dd title={init?.logDir}>{init?.logDir ?? "未知"}</dd>
          </div>
        </dl>
        <p className="about-dialog__footer">
          基于 <code>Tauri 2</code> + <code>React</code>。第三方组件及许可详见{" "}
          <code>THIRD_PARTY_NOTICES</code>。
        </p>
        {onCheckForUpdates && (
          <button
            className="settings-btn about-dialog__update"
            onClick={() => {
              onClose();
              onCheckForUpdates();
            }}
          >
            检查更新…
          </button>
        )}
      </div>
    </div>
  );
}
