import type { DeliveryReport, TaskPhase } from "./types";

export interface PhasePresentation {
  label: string;
  /** Rendering tone; the UI maps this to a colour token. */
  tone: "idle" | "running" | "waiting" | "good" | "bad";
  /** True while the workbench is actively working. */
  active: boolean;
}

const PHASES: Record<TaskPhase, PhasePresentation> = {
  idle: { label: "待开始", tone: "idle", active: false },
  discovering: { label: "分析工程", tone: "running", active: true },
  implementing: { label: "实现中", tone: "running", active: true },
  verifying: { label: "验证中", tone: "running", active: true },
  diagnosing: { label: "诊断中", tone: "running", active: true },
  repairing: { label: "修复中", tone: "running", active: true },
  paused: { label: "已暂停", tone: "waiting", active: false },
  stopped: { label: "已停止", tone: "idle", active: false },
  delivered: { label: "执行完成", tone: "waiting", active: false },
  blocked: { label: "已阻塞", tone: "bad", active: false },
};

export function describePhase(phase: TaskPhase): PhasePresentation {
  return PHASES[phase] ?? PHASES.idle;
}

/** User-facing progress groups. The persisted phase remains the scheduler's state. */
export function describeTaskProgress(phase: TaskPhase, _report?: DeliveryReport | null): PhasePresentation {
  if (phase === "discovering") return { label: "分析中", tone: "running", active: true };
  if (phase === "implementing") return { label: "开发中", tone: "running", active: true };
  if (["verifying", "diagnosing", "repairing"].includes(phase)) {
    return { label: "验证与修复", tone: "running", active: true };
  }
  if (phase === "blocked") return { label: "需要处理", tone: "bad", active: false };
  if (phase === "delivered") return { label: "本轮完成", tone: "good", active: false };

  return describePhase(phase);
}

/** Whether a task in this phase should block a destructive action. */
export function isBusyPhase(phase?: TaskPhase): boolean {
  return phase ? describePhase(phase).active : false;
}
