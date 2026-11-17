/**
 * 项目「连接器 / Agent / Skill」拾取器 + 模板选项 + 配置行。
 * 候选数据直接来自当前 EchoAgent 运行时，不展示尚未安装或未启用的占位集成。
 */
import { useEffect, useRef, useState } from "react";
import type { RefItem } from "@/stores/projects-store";
import { CheckIcon } from "@/foundation/components/Icon/icons";
import { agentsList, mcpList, skillsList } from "@/lib/agent-client";
import { useModalFocus } from "@/lib/use-modal-focus";

export type ProjectPickerOptions = Record<"connectors" | "experts" | "skills", RefItem[]>;

const EMPTY_OPTIONS: ProjectPickerOptions = { connectors: [], experts: [], skills: [] };

/** Load only capabilities actually discovered by the live runtime. */
export function useProjectPickerOptions(cwd?: string): {
  options: ProjectPickerOptions;
  loading: boolean;
  error: string | null;
} {
  const [options, setOptions] = useState<ProjectPickerOptions>(EMPTY_OPTIONS);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    Promise.all([mcpList(), agentsList(cwd), skillsList(cwd)])
      .then(([connectors, experts, skills]) => {
        if (cancelled) return;
        setOptions({
          connectors: connectors
            .filter((item) => item.enabled)
            .map((item) => ({ id: item.name, name: item.name })),
          experts: experts.map((item) => ({ id: item.path, name: item.name, scope: item.scope })),
          skills: skills
            .filter((item) => item.enabled)
            .map((item) => ({ id: item.path ?? item.name, name: item.displayName ?? item.name })),
        });
        setError(null);
      })
      .catch((reason) => {
        if (!cancelled) setError(String(reason).replace(/^Error:\s*/, ""));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => { cancelled = true; };
  }, [cwd]);

  return { options, loading, error };
}

export interface ProjectTemplate {
  id: string;
  title: string;
  desc: string;
  instructions: string;
  connectors: RefItem[];
  experts: RefItem[];
  skills: RefItem[];
}

/** 模板选项（自定义空白 + 5 业务模板，文案取自目标截图）。 */
export const TEMPLATE_OPTIONS: ProjectTemplate[] = [
  {
    id: "custom",
    title: "自定义",
    desc: "空白项目",
    instructions: "",
    connectors: [],
    experts: [],
    skills: [],
  },
  {
    id: "product-requirements",
    title: "产品需求全流程",
    desc: "从需求规划、PRD 到研发测试验收",
    instructions:
      "你是一名产品负责人助理。请覆盖需求收集、PRD 撰写、研发排期与测试验收全流程，输出结构化文档与待办清单。",
    connectors: [],
    experts: [],
    skills: [],
  },
  {
    id: "market-research",
    title: "市场调研与竞品分析",
    desc: "深度调研、竞品拆解、报告评审",
    instructions:
      "你是一名市场研究分析师。请进行深度调研与竞品拆解，并产出结构清晰、可评审的调研报告。",
    connectors: [],
    experts: [],
    skills: [],
  },
  {
    id: "team-knowledge-base",
    title: "团队知识库",
    desc: "持续沉淀 SOP、经验和 FAQ",
    instructions:
      "你是一名知识管理助理。请帮助沉淀 SOP、经验与 FAQ，维护并结构化团队知识库。",
    connectors: [],
    experts: [],
    skills: [],
  },
  {
    id: "project-delivery",
    title: "项目交付",
    desc: "管理客户需求、计划、风险和周报",
    instructions:
      "你是一名项目交付经理。请管理客户需求、计划、风险与周报，推动项目按期高质量交付。",
    connectors: [],
    experts: [],
    skills: [],
  },
  {
    id: "bug-tracking-qa",
    title: "Bug 跟踪/测试验收",
    desc: "持续跟踪Bug、统一测试用例和验收结论",
    instructions:
      "你是一名 QA 助理。请持续跟踪 Bug、统一测试用例，并给出明确的验收结论。",
    connectors: [],
    experts: [],
    skills: [],
  },
];

export function getTemplate(id?: string): ProjectTemplate | undefined {
  return TEMPLATE_OPTIONS.find((t) => t.id === id);
}

/** 配置行：label + (可选) + 右「+ 添加」+ 已选 chip（对照目标新建弹窗/配置抽屉）。 */
export function ConfigRow({
  label,
  items,
  onAdd,
  onRemove,
}: {
  label: string;
  items: RefItem[];
  onAdd: () => void;
  onRemove: (id: string) => void;
}) {
  return (
    <div className="proj-config-row">
      <div className="proj-config-row__head">
        <span className="proj-config-row__label">
          {label} <span className="proj-config-row__opt">（可选）</span>
        </span>
        <button type="button" className="proj-config-row__add" onClick={onAdd}>
          + 添加
        </button>
      </div>
      {items.length > 0 && (
        <div className="proj-config-row__chips">
          {items.map((it) => (
            <span key={it.id} className="proj-chip">
              <span className="proj-chip__name">{it.name}</span>
              <button
                type="button"
                className="proj-chip__x"
                aria-label={`移除 ${it.name}`}
                onClick={() => onRemove(it.id)}
              >
                ×
              </button>
            </span>
          ))}
        </div>
      )}
    </div>
  );
}

/** 通用运行时资源多选弹窗。 */
export function RefPickerDialog({
  title, emptyHint,
  options,
  selected,
  onCancel,
  onConfirm,
}: {
  title: string;
  emptyHint?: string;
  options: RefItem[];
  selected: RefItem[];
  onCancel: () => void;
  onConfirm: (items: RefItem[]) => void;
}) {
  const [picked, setPicked] = useState<RefItem[]>(selected);
  const dialogRef = useModalFocus<HTMLDivElement>(true, onCancel);
  const toggle = (o: RefItem) =>
    setPicked((prev) =>
      prev.some((p) => p.id === o.id) ? prev.filter((p) => p.id !== o.id) : [...prev, o],
    );

  return (
    <div className="modal-overlay" onClick={onCancel}>
      <div
        ref={dialogRef}
        className="create-colleague-dialog proj-picker-dialog"
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-modal="true"
        aria-label={`添加${title}`}
        tabIndex={-1}
      >
        <div className="create-colleague-header">
          <h3>添加{title}</h3>
          <button className="create-colleague-close" onClick={onCancel} aria-label="关闭" data-modal-initial-focus>
            ×
          </button>
        </div>
        <div className="create-colleague-body proj-picker-body">
          {options.length === 0 && (
            <div className="proj-picker-empty">{emptyHint ?? `当前没有可用的${title}`}</div>
          )}
          {picked.filter(item => !options.some(option => option.id === item.id)).map(item => <div className="proj-picker-item" key={`retained:${item.id}`}><span>{item.name}<small> · 暂不可用，原配置已保留</small></span><button type="button" className="pd-btn pd-btn--small" onClick={() => toggle(item)} aria-label={`移除${item.name}`}>移除</button></div>)}
          {options.map((o) => {
            const on = picked.some((p) => p.id === o.id);
            return (
              <button
                key={o.id}
                type="button"
                className={`proj-picker-item${on ? " proj-picker-item--on" : ""}`}
                onClick={() => toggle(o)}
              >
                <span className={`proj-picker-check${on ? " proj-picker-check--on" : ""}`}>
                  {on && <CheckIcon size="sm" />}
                </span>
                <span>{o.name}{o.scope && <small> · {o.scope === "project" ? "当前项目" : o.scope === "user" ? "用户专家" : "内置来源"}</small>}</span>
              </button>
            );
          })}
        </div>
        <div className="create-colleague-footer">
          <button className="btn btn--ghost" onClick={onCancel}>取消</button>
          <button className="btn btn--primary" onClick={() => onConfirm(picked)}>确定</button>
        </div>
      </div>
    </div>
  );
}

/** 点击外部关闭的简单 hook（模板下拉用）。 */
export function useOutsideClose<T extends HTMLElement>(open: boolean, onClose: () => void) {
  const ref = useRef<T>(null);
  useEffect(() => {
    if (!open) return;
    const h = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose();
    };
    document.addEventListener("mousedown", h);
    return () => document.removeEventListener("mousedown", h);
  }, [open, onClose]);
  return ref;
}
