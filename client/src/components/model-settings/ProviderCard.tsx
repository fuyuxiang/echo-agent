import { CheckCircle2, CircleAlert, Ellipsis, Loader2, Pencil, Link2 } from "lucide-react";
import { useState } from "react";
import type { ModelEntry, ModelProviderEntry } from "@/lib/agent-client";
import { modelTestDuration, type useModelConnectionTests } from "@/lib/use-model-connection-tests";
import { ActionMenu } from "../ActionMenu";
import { IconButton } from "../Button";
import { ModelIdentity } from "../ModelIdentity";
import { modelName, modelRemoteId, providerDisplayName, serviceKind } from "./provider-presets";

export type ModelTestResult = ReturnType<typeof useModelConnectionTests>["results"][string];

export interface ProviderCardProps {
  provider: ModelProviderEntry;
  models: ModelEntry[];
  tests: Record<string, ModelTestResult>;
  busy: boolean;
  organizationExpired: boolean;
  registerRow: (modelId: string, element: HTMLLIElement | null) => void;
  defaultModelId?: string;
  onSetDefault?: (model: ModelEntry) => void;
  onTest: (model: ModelEntry) => void;
  onEditModel: (model: ModelEntry) => void;
  onDeleteModel: (model: ModelEntry) => void;
}

const MENU_CLASS = "topbar-actions__menu";
const ITEM_CLASS = "topbar-actions__item";
function TestBadge({ name, result }: { name: string; result?: ModelTestResult }) {
  const [open, setOpen] = useState(false);
  if (!result) return null;
  if (result.status === "testing") return <span className="provider-card__test" role="status"><Loader2 className="models-settings-panel__spin" size={13} aria-hidden="true" />测试中</span>;
  const time = new Date(result.completedAt);
  const tip = `测试于 ${time.toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" })}${result.error ? `：${result.error}` : ""}`;
  if (result.status === "success") return <span className="provider-card__test provider-card__test--success" role="status" data-tip={tip}>
    <CheckCircle2 size={13} aria-hidden="true" />{modelTestDuration(result.durationMs).replace(/ 秒$/, "s")}
  </span>;
  return <span className="provider-card__test-wrap">
    <span className="provider-card__test provider-card__test--error" role="status" data-tip={tip}>
      <button type="button" className="provider-card__test-toggle" aria-expanded={open} aria-label={`查看 ${name} 的测试错误`} onClick={() => setOpen(value => !value)}>
        <CircleAlert size={13} aria-hidden="true" />失败
      </button>
    </span>
    {open && result.error && <p className="provider-card__test-detail">{result.error}</p>}
  </span>;
}

export function ProviderCard(props: ProviderCardProps) {
  const { provider, models, tests, busy, organizationExpired } = props;
  const isOrganization = provider.source === "organization";
  const readOnly = isOrganization || Boolean(provider.managed);
  const blocked = isOrganization && organizationExpired;
  const name = providerDisplayName(provider);
  const missingKey = !readOnly && provider.credentialConfigured === false;
  const kind = serviceKind(provider);
  return <section className={`provider-card${blocked ? " provider-card--blocked" : ""}`} aria-label={name}>
    <ul className="provider-card__models">
      {models.map(model => {
        const label = modelName(model);
        const remoteId = modelRemoteId(model);
        const result = tests[model.modelId];
        const editable = !readOnly && !model.managed;
        return <li key={model.modelId} className="provider-card__model" tabIndex={-1} data-model-id={model.modelId} ref={element => props.registerRow(model.modelId, element)}>
          <div className="provider-card__model-main">
            <ModelIdentity model={{ id: model.modelId, providerKind: kind }} />
            <div className="provider-card__model-copy">
              <div className="provider-card__model-title">
                <strong title={label !== remoteId ? remoteId : undefined}>{label}</strong>
                <span className="provider-card__model-source">{name}{isOrganization ? " · 组织托管" : readOnly ? " · 只读" : ""}</span>
                {props.defaultModelId === model.modelId && <span className="provider-card__badge">默认</span>}
                {missingKey && editable && <button type="button" className="provider-card__badge provider-card__badge--error" disabled={busy} onClick={() => props.onEditModel(model)}>未配置密钥</button>}
              </div>
            </div>
          </div>
          <TestBadge name={label} result={result} />
          <div className="provider-card__model-actions">
            <IconButton label={`测试模型 ${label}`} data-tip="测试文本回复，可能消耗少量额度"
              aria-busy={result?.status === "testing"} disabled={busy || result?.status === "testing" || blocked}
              onClick={() => props.onTest(model)}><Link2 size={15} aria-hidden="true" /></IconButton>
            {editable && <IconButton className="provider-card__row-edit" label={`编辑模型 ${label}`} data-tip="编辑模型"
              disabled={busy} onClick={() => props.onEditModel(model)}><Pencil size={13} aria-hidden="true" /></IconButton>}
            <ActionMenu label={`模型操作 ${label}`} menuLabel={`${label} 操作`} busy={busy} resetKey={model.modelId}
              triggerClassName="echo-button echo-button--ghost echo-button--compact echo-button--icon-only" menuClassName={MENU_CLASS}
              menuZIndex="var(--echo-layer-dialog-popover)" trigger={<Ellipsis size={16} aria-hidden="true" />}>
              {({ close }) => <>
                <button type="button" role="menuitem" className={ITEM_CLASS} disabled={result?.status === "testing" || blocked} onClick={() => { close(); props.onTest(model); }}>测试</button>
                {props.onSetDefault && !blocked && props.defaultModelId !== model.modelId && <button type="button" role="menuitem" className={ITEM_CLASS}
                  onClick={() => { close(); props.onSetDefault?.(model); }}>设为默认</button>}
                {editable && <button type="button" role="menuitem" className={ITEM_CLASS} onClick={() => { close(); props.onEditModel(model); }}>编辑</button>}
                {editable && <button type="button" role="menuitem" className={`${ITEM_CLASS} provider-card__danger`} onClick={() => { close(); props.onDeleteModel(model); }}>删除</button>}
              </>}
            </ActionMenu>
          </div>
        </li>;
      })}
    </ul>
  </section>;
}
