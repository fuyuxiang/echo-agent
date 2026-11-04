import { useMemo, useRef, useState } from "react";
import { XCloseIcon, FolderOpenIcon } from "@/foundation/components/Icon/icons";
import {
  filesystemPickDirectory,
  filesystemPickFiles,
  skillsInspectPackages,
  skillsInstallPackage,
} from "@/lib/agent-client";
import type { SkillPackageInspection, SkillRiskLevel } from "@/lib/types";
import { useModalFocus } from "@/lib/use-modal-focus";
import { SkillCapabilityStatus } from "./SkillCapabilityStatus";

const RISK_LABEL: Record<SkillRiskLevel, string> = {
  low: "低风险",
  medium: "中风险",
  high: "高风险",
};

/** 单个待处理技能包在批量安装队列里的状态。 */
interface PendingItem {
  /** 仅用于 React key 和 DOM 测试的本地唯一 id。 */
  id: string;
  /** 用户选中的本地绝对路径。 */
  path: string;
  /** 列表中显示的文件名或包内相对路径。 */
  displayName: string;
  /** 目录/ZIP 中某个独立技能的相对根路径。 */
  packageRoot?: string;
  status: "inspecting" | "ready" | "installing" | "done" | "error";
  inspection: SkillPackageInspection | null;
  error: string | null;
  /** 高风险项需要用户勾选后才能安装。 */
  approvedHighRisk: boolean;
}

let itemIdCounter = 0;
function nextItemId(): string {
  itemIdCounter += 1;
  return `item-${Date.now().toString(36)}-${itemIdCounter}`;
}

function basenameOf(path: string): string {
  const normalized = path.replace(/\\/g, "/").replace(/\/+$/, "");
  return normalized.split("/").pop() || path;
}

async function mapWithConcurrency<T, R>(
  values: T[],
  limit: number,
  task: (value: T) => Promise<R>,
): Promise<R[]> {
  const results = new Array<R>(values.length);
  let cursor = 0;
  const worker = async () => {
    while (cursor < values.length) {
      const index = cursor++;
      results[index] = await task(values[index]);
    }
  };
  await Promise.all(
    Array.from({ length: Math.min(limit, values.length) }, () => worker()),
  );
  return results;
}

/** Managed local Skill installer: discover, inspect, then install atomically. */
export function ImportSkillModal({
  onClose, onToast, onInstalled,
}: {
  onClose: () => void;
  onToast?: (m: string) => void;
  onInstalled?: () => void;
}) {
  const [autoInstall, setAutoInstall] = useState(false);
  const [items, setItems] = useState<PendingItem[]>([]);
  const [globalError, setGlobalError] = useState("");
  // commitInstall 必须能同步读到当前 items;用 ref 跟踪最新值,避免依赖 setState
  // updater(React 18 不保证同步执行)导致的空读 race。
  const itemsRef = useRef<PendingItem[]>(items);
  const updateItems = (
    next: PendingItem[] | ((previous: PendingItem[]) => PendingItem[]),
  ) => {
    const value = typeof next === "function" ? next(itemsRef.current) : next;
    itemsRef.current = value;
    setItems(value);
  };
  const dialogRef = useModalFocus<HTMLDivElement>(true, () => {
    if (!isBusy(items)) onClose();
  });

  const updateItem = (id: string, patch: Partial<PendingItem>) => {
    updateItems((prev) => prev.map((it) => (it.id === id ? { ...it, ...patch } : it)));
  };

  const createPendingItem = (path: string): PendingItem => ({
    id: nextItemId(),
    path,
    displayName: basenameOf(path),
    status: "inspecting",
    inspection: null,
    error: null,
    approvedHighRisk: false,
  });

  const inspectOne = async (item: PendingItem): Promise<PendingItem[]> => {
    try {
      const outcomes = await skillsInspectPackages(item.path);
      if (outcomes.length === 0) throw new Error("未发现可安装的技能");
      const completedItems = outcomes.map((outcome, index): PendingItem => {
        const inspection = outcome.inspection ?? null;
        const error = outcome.error
          ?? (inspection ? null : "该技能未返回检查结果");
        const packageRoot = outcome.packageRoot ?? inspection?.packageRoot;
        const outcomeLabel = outcome.label || packageRoot || item.displayName;
        return {
          ...item,
          id: index === 0 ? item.id : nextItemId(),
          displayName: packageRoot
            ? `${item.displayName} / ${outcomeLabel}`
            : outcomeLabel,
          packageRoot,
          status: inspection ? "ready" : "error",
          inspection,
          error,
          approvedHighRisk: false,
        };
      });
      updateItems((prev) =>
        prev.flatMap((it) => (it.id === item.id ? completedItems : [it])),
      );
      return completedItems;
    } catch (e) {
      const message = String(e).replace(/^Error:\s*/, "");
      const failed: PendingItem = { ...item, status: "error", error: message };
      updateItems((prev) => prev.map((it) => (it.id === item.id ? failed : it)));
      return [failed];
    }
  };

  const commitInstall = async (
    id: string,
    approved?: boolean,
    snapshot?: PendingItem,
  ) => {
    // 同步读最新 item,避免 setState 异步导致空 read。
    const target = snapshot ?? itemsRef.current.find((it) => it.id === id);
    if (!target || !target.inspection) return;
    const inspection = target.inspection;
    const approve = approved ?? target.approvedHighRisk;
    updateItems((prev) =>
      prev.map((it) => (it.id === id ? { ...it, status: "installing" } : it)),
    );
    try {
      const result = target.packageRoot === undefined
        ? await skillsInstallPackage(target.path, inspection.sourceHash, approve)
        : await skillsInstallPackage(
            target.path,
            inspection.sourceHash,
            approve,
            target.packageRoot,
          );
      updateItems((prev) =>
        prev.map((it) =>
          it.id === id ? { ...it, status: "done", inspection: result.inspection } : it,
        ),
      );
      onToast?.(`${result.updated ? "已更新" : "已安装"}技能「${result.inspection.name}」`);
      onInstalled?.();
    } catch (e) {
      const message = String(e).replace(/^Error:\s*/, "");
      updateItems((prev) =>
        prev.map((it) =>
          it.id === id ? { ...it, status: "error", error: message } : it,
        ),
      );
    }
  };

  const installLowRiskItems = async (candidates: PendingItem[]) => {
    const readyLow = candidates.filter(
      (it) => it.status === "ready" && it.inspection?.riskLevel === "low",
    );
    const names = new Set<string>();
    for (const item of readyLow) {
      const normalizedName = item.inspection!.name.trim().toLocaleLowerCase();
      if (names.has(normalizedName)) {
        updateItem(item.id, {
          status: "error",
          error: `已跳过同名技能「${item.inspection!.name}」，请只保留一个版本后重试`,
        });
        continue;
      }
      names.add(normalizedName);
      await commitInstall(item.id, undefined, item);
    }
  };

  const installAllLowRisk = async () => {
    await installLowRiskItems(itemsRef.current);
  };

  const removeItem = (id: string) => {
    updateItems((prev) => prev.filter((it) => it.id !== id));
  };

  const pickFile = async () => {
    setGlobalError("");
    try {
      const selected = await filesystemPickFiles({
        title: "选择技能文件（Markdown 或 ZIP）",
        extensions: ["md", "markdown", "zip"],
        multiple: true,
        maxFiles: 50,
      });
      if (!selected || selected.length === 0) return;
      const pending = selected.map(createPendingItem);
      updateItems((prev) => [...prev, ...pending]);
      // 本地解压和风险扫描最多 3 路并发，避免大批量包同时占用内存和线程。
      const inspected = (await mapWithConcurrency(pending, 3, inspectOne)).flat();
      // 先等待整批检查完成,才能对 Skill 名做去重并串行安装。
      if (autoInstall) await installLowRiskItems(inspected);
    } catch (cause) {
      setGlobalError(`选择技能文件失败：${String(cause).replace(/^Error:\s*/, "")}`);
    }
  };

  const pickFolder = async () => {
    setGlobalError("");
    try {
      const selected = await filesystemPickDirectory();
      if (!selected) return;
      const pending = createPendingItem(selected);
      updateItems((prev) => [...prev, pending]);
      const inspected = await inspectOne(pending);
      if (autoInstall) await installLowRiskItems(inspected);
    } catch (cause) {
      setGlobalError(`选择技能文件夹失败：${String(cause).replace(/^Error:\s*/, "")}`);
    }
  };

  const readyLowCount = useMemo(
    () =>
      items.filter((it) => it.status === "ready" && it.inspection?.riskLevel === "low")
        .length,
    [items],
  );
  const busy = isBusy(items);
  const identifying = items.some((item) => item.status === "inspecting");

  return (
    <div className="modal-overlay sk-import-overlay" onClick={(event) => {
      if (event.target === event.currentTarget && !busy) onClose();
    }}>
      <div
        ref={dialogRef}
        className={`sk-import sk-import--managed${items.length ? " sk-import--inspected" : ""}`}
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-modal="true"
        aria-label="安装本地技能"
        tabIndex={-1}
      >
        <div className="sk-import-head">
          <div>
            <h3>安装本地技能</h3>
            <p>可多选 Markdown / ZIP，也可从文件夹或 ZIP 中自动识别多个技能</p>
          </div>
          <button
            type="button"
            className="sk-import-close"
            onClick={onClose}
            disabled={busy}
            aria-label="关闭"
            data-modal-initial-focus
          >
            <XCloseIcon size="md" />
          </button>
        </div>
        <div className="sk-import-body">
          <div
            className={`sk-drop${busy ? " sk-drop--busy" : ""}`}
            role="button"
            tabIndex={busy ? -1 : 0}
            aria-label="选择 Markdown 或 ZIP 技能文件"
            onClick={() => { if (!busy) void pickFile(); }}
            onKeyDown={(event) => {
              if (!busy && (event.key === "Enter" || event.key === " ")) {
                event.preventDefault();
                void pickFile();
              }
            }}
          >
            <FolderOpenIcon size="xl" className="sk-drop-icon" />
            <div className="sk-drop-title">
              {busy ? "正在处理…" : "点击选择 Markdown / ZIP（可多选）"}
            </div>
          </div>
          <button type="button" className="sk-import-folder" onClick={pickFolder} disabled={busy}>
            或选择技能文件夹（自动识别全部技能）
          </button>

          <label className="sk-import-check">
            <input
              type="checkbox"
              checked={autoInstall}
              disabled={busy}
              onChange={(e) => setAutoInstall(e.target.checked)}
            />
            <span>仅在检查结果为低风险时自动安装</span>
          </label>

          {globalError && <div className="sk-install-error" role="alert">{globalError}</div>}

          {items.length > 0 && (
            <div className="sk-batch">
              <div className="sk-batch-head">
                <span>
                  {identifying
                    ? `正在识别技能，当前 ${items.length} 项`
                    : `已识别 ${items.length} 个技能`}
                </span>
                {readyLowCount > 0 && (
                  <button
                    type="button"
                    className="um-btn um-btn--primary sk-batch-install-all"
                    onClick={() => void installAllLowRisk()}
                    disabled={busy}
                  >
                    安装全部低风险（{readyLowCount}）
                  </button>
                )}
              </div>
              <ul className="sk-batch-list">
                {items.map((item) => (
                  <SkillBatchRow
                    key={item.id}
                    item={item}
                    busy={busy}
                    onApprove={(approved) =>
                      updateItem(item.id, { approvedHighRisk: approved })
                    }
                    onInstall={() => void commitInstall(item.id)}
                    onRemove={() => removeItem(item.id)}
                  />
                ))}
              </ul>
            </div>
          )}

          {items.length === 0 && !globalError && (
            <div className="sk-import-req">
              <div className="sk-import-req-title">安装检查</div>
              <ul className="sk-import-req-list">
                <li>验证 SKILL.md、文件数量、大小和目录安全</li>
                <li>自动区分并列技能与技能内嵌套的参考文档</li>
                <li>检查技能运行所需的程序、依赖、账号和输出要求</li>
                <li>扫描脚本、敏感文件访问、网络和依赖安装风险</li>
                <li>记录版本校验信息，便于安全更新和卸载</li>
              </ul>
              <details className="sk-import-req-details">
                <summary>查看开发者校验细节</summary>
                <p>校验 echo.skill.json 的执行入口、依赖、账号和产物契约；生成内容指纹并支持原子更新。</p>
              </details>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

function isBusy(items: PendingItem[]): boolean {
  return items.some((it) => it.status === "inspecting" || it.status === "installing");
}

function SkillBatchRow({
  item, busy, onApprove, onInstall, onRemove,
}: {
  item: PendingItem;
  busy: boolean;
  onApprove: (approved: boolean) => void;
  onInstall: () => void;
  onRemove: () => void;
}) {
  const inspection = item.inspection;
  const fileName = item.displayName;
  const requiresApproval =
    inspection?.riskLevel === "high" && item.status === "ready";
  const canInstall =
    inspection
    && item.status === "ready"
    && (inspection.riskLevel !== "high" || item.approvedHighRisk)
    && !busy;
  const installLabel = item.status === "installing"
    ? "安装中…"
    : inspection?.alreadyInstalled ? "更新技能" : "安装技能";

  return (
    <li className={`sk-batch-row sk-batch-row--${item.status}`}>
      <div className="sk-batch-row-main">
        <div
          className="sk-batch-row-name"
          title={item.packageRoot ? `${item.path} / ${item.packageRoot}` : item.path}
        >
          {fileName}
        </div>
        <div className="sk-batch-row-status">
          {item.status === "inspecting" && <span>检查中…</span>}
          {item.status === "ready" && inspection && (
            <span className="sk-batch-row-tagline">
              <span className={`sk-risk sk-risk--${inspection.riskLevel}`}>
                {RISK_LABEL[inspection.riskLevel]}
              </span>
              <span>{inspection.name}</span>
              {inspection.version && <span>v{inspection.version}</span>}
            </span>
          )}
          {item.status === "installing" && <span>安装中…</span>}
          {item.status === "done" && (
            <span className="sk-batch-row-done">已安装</span>
          )}
          {item.status === "error" && (
            <span className="sk-batch-row-error" role="alert">{item.error}</span>
          )}
        </div>
        {item.status === "ready" && inspection && (
          <details className="sk-batch-row-detail">
            <summary>详情</summary>
            <p className="sk-inspection-desc">{inspection.description}</p>
            <SkillCapabilityStatus report={inspection.capability} />
            {inspection.findings.length > 0 && (
              <div className="sk-findings">
                {inspection.findings.map((finding, index) => (
                  <div
                    key={`${finding.code}-${finding.path ?? index}`}
                    className={`sk-finding sk-finding--${finding.level}`}
                  >
                    <span>{finding.message}</span>
                    {finding.path && <code>{finding.path}</code>}
                  </div>
                ))}
              </div>
            )}
            {inspection.warnings.map((warning) => (
              <div key={warning} className="sk-install-warning">{warning}</div>
            ))}
          </details>
        )}
      </div>
      <div className="sk-batch-row-actions">
        {requiresApproval && (
          <label className="sk-high-risk-confirm">
            <input
              type="checkbox"
              checked={item.approvedHighRisk}
              onChange={(e) => onApprove(e.target.checked)}
              disabled={busy}
            />
            <span>我已查看风险</span>
          </label>
        )}
        {item.status === "ready" && (
          <button
            type="button"
            className="um-btn um-btn--primary sk-batch-row-install"
            onClick={onInstall}
            disabled={!canInstall}
            aria-label={installLabel}
          >
            {installLabel}
          </button>
        )}
        {(item.status === "done" || item.status === "error") && (
          <button
            type="button"
            className="sk-batch-row-remove"
            onClick={onRemove}
            aria-label={`移除 ${fileName}`}
          >
            <XCloseIcon size="sm" />
          </button>
        )}
      </div>
    </li>
  );
}
