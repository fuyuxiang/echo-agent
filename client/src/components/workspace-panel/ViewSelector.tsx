import { useRef, type KeyboardEvent } from "react";
import type { WorkspaceView } from "@/lib/use-unified-tabs";
import { FileText, Folder, Globe, ListChecks, Users, GitCompareArrows, Network } from "lucide-react";

export interface ViewDef {
  value: WorkspaceView;
  label: string;
  icon: () => React.ReactNode;
  hidden?: boolean;
}
const taskViews: WorkspaceView[] = ["plan", "subagents", "teams"];

/** Category navigation and task navigation have separate, visible text labels. */
export function ViewSelector({ view, views, onChange, secondary = false }: {
  view: WorkspaceView; views: ViewDef[]; onChange: (value: WorkspaceView) => void; secondary?: boolean;
}) {
  const lastTask = useRef<WorkspaceView>("plan");
  if (taskViews.includes(view)) lastTask.current = view;
  if (secondary && !taskViews.includes(view)) return null;
  const visible = views.filter(item => !item.hidden);
  const items = secondary
    ? visible.filter(item => taskViews.includes(item.value)).map(item => ({ ...item, label: item.value === "plan" ? "计划" : item.value === "subagents" ? "运行" : "团队" }))
    : visible.filter(item => !taskViews.includes(item.value));
  if (!secondary && visible.some(item => taskViews.includes(item.value))) {
    items.splice(Math.min(3, items.length), 0, { value: lastTask.current, label: "任务", icon: () => <ListChecks size={16} /> });
  }
  const selected = (value: WorkspaceView) => secondary ? view === value
    : taskViews.includes(value) ? taskViews.includes(view) : view === value;
  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (!["ArrowRight", "ArrowLeft", "Home", "End"].includes(event.key)) return;
    const buttons = Array.from(event.currentTarget.querySelectorAll<HTMLButtonElement>("[role=tab]"));
    const current = buttons.indexOf(event.target as HTMLButtonElement);
    const next = event.key === "Home" ? 0 : event.key === "End" ? buttons.length - 1
      : (current + (event.key === "ArrowRight" ? 1 : -1) + buttons.length) % buttons.length;
    event.preventDefault();
    buttons[next]?.focus();
    if (items[next]) onChange(items[next].value);
  };
  return <div className={secondary ? "workspace-task-tabs" : "view-selector workspace-view-tabs"}
    role="tablist" aria-label={secondary ? "任务视图" : "工作区分类"} onKeyDown={onKeyDown}>
    {items.map(item => <button type="button" role="tab" key={item.label} aria-selected={selected(item.value)}
      tabIndex={selected(item.value) ? 0 : -1} onClick={() => onChange(item.value)}><span className="workspace-view-icon" aria-hidden="true">{item.icon()}</span>{item.label}</button>)}
  </div>;
}

export function defaultViews(_opts: { hasChanges: boolean }): ViewDef[] {
  return [
    {value:"artifacts",label:"成果",icon:() => <FileText size={16} />},
    {value:"fileTree",label:"文件",icon:() => <Folder size={16} />},
    {value:"changes",label:"变更",icon:() => <GitCompareArrows size={16} />},
    {value:"plan",label:"执行计划",icon:() => <ListChecks size={16} />},
    {value:"subagents",label:"运行与协作",icon:() => <Network size={16} />},
    {value:"teams",label:"任务团队",icon:() => <Users size={16} />},
    {value:"browser",label:"网页",icon:() => <Globe size={16} />},
  ];
}
