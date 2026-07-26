import { describe, it, expect } from "vitest";
import { readFileSync, existsSync } from "node:fs";
import { VALID_TRANSITIONS } from "./kanban";

/**
 * 跨端状态机一致性。
 *
 * 前端 VALID_TRANSITIONS 是后端 VALID_TASK_TRANSITIONS 的镜像,用来在拖拽时判定
 * 合法落点。两处各写一份终究会漂移,所以这里直接解析后端源码来对比。
 *
 * 允许的偏差是单向的:前端可以比后端**更保守**(少一条落点只会让界面不提供某个操作,
 * 后端仍是唯一权威),但绝不能更宽松——那会给出会被后端打回的假可落提示,用户拖过去
 * 卡片再弹回来。已知且有意的收紧:所有 *→running。进入 running 是 dispatcher 的专属
 * 职责,api/tasks.py 的 transition 端点硬拒了它,前端一并移除避免假提示。
 */
const RELATIVE_MODELS_PY = "echo_agent/tasks/models.py";

/**
 * 定位 models.py,与从哪个目录启动 vitest 无关。
 *
 * process.cwd() 单独用不行:在 web/ 下启动和在仓库根用 --root web 启动,cwd 不同,
 * 固定的相对路径会失效。import.meta.url 也不行:jsdom 环境下它不是 file: URL。
 * 这里从 cwd 逐级上溯找到该文件,两种启动方式都能命中。
 */
function findModelsPy(): string {
  let dir = process.cwd();
  for (let i = 0; i < 6; i++) {
    const candidate = `${dir}/${RELATIVE_MODELS_PY}`;
    if (existsSync(candidate)) return candidate;
    const parent = dir.replace(/\/[^/]+\/?$/, "");
    if (!parent || parent === dir) break;
    dir = parent;
  }
  throw new Error(`未能从 ${process.cwd()} 上溯找到 ${RELATIVE_MODELS_PY}`);
}

/** 从 models.py 里解析 VALID_TASK_TRANSITIONS,得到 status → 目标状态集合。 */
function parseBackendTransitions(): Record<string, string[]> {
  const source = readFileSync(findModelsPy(), "utf8");
  const start = source.indexOf("VALID_TASK_TRANSITIONS");
  expect(start, "在 models.py 中找不到 VALID_TASK_TRANSITIONS").toBeGreaterThan(-1);

  // 截取到该字典结束(下一个顶层赋值处),避免把后面的常量一起吃进来。
  const tail = source.slice(start);
  const end = tail.indexOf("\nTERMINAL_TASK_STATUSES");
  const body = end === -1 ? tail : tail.slice(0, end);

  const result: Record<string, string[]> = {};
  // 形如:TaskStatus.PENDING: {TaskStatus.QUEUED, TaskStatus.CANCELLED},
  // 也覆盖跨行的集合与 set() 空集。
  const entryRe = /TaskStatus\.([A-Z_]+):\s*(set\(\)|\{[^}]*\})/g;
  for (const m of body.matchAll(entryRe)) {
    const from = m[1].toLowerCase();
    const raw = m[2];
    const targets = raw === "set()"
      ? []
      : [...raw.matchAll(/TaskStatus\.([A-Z_]+)/g)].map((t) => t[1].toLowerCase());
    result[from] = targets;
  }
  return result;
}

describe("前后端状态机一致性", () => {
  const backend = parseBackendTransitions();

  it("成功解析出后端全部 9 个状态", () => {
    expect(Object.keys(backend).sort()).toEqual([
      "blocked", "cancelled", "failed", "pending", "queued",
      "review", "running", "success", "suspended",
    ]);
  });

  it("前端状态集合与后端一致", () => {
    expect(Object.keys(VALID_TRANSITIONS).sort()).toEqual(Object.keys(backend).sort());
  });

  it("前端不含后端没有的流转(不得比后端更宽松)", () => {
    const extra: string[] = [];
    for (const [from, targets] of Object.entries(VALID_TRANSITIONS)) {
      for (const to of targets) {
        if (!(backend[from] ?? []).includes(to)) extra.push(`${from}→${to}`);
      }
    }
    expect(extra).toEqual([]);
  });

  it("前端相比后端只少了 *→running,没有别的意外收紧", () => {
    const missing: string[] = [];
    for (const [from, targets] of Object.entries(backend)) {
      for (const to of targets) {
        if (!(VALID_TRANSITIONS[from] ?? []).includes(to)) missing.push(`${from}→${to}`);
      }
    }
    // queued→running 由 dispatcher 自动完成;blocked/suspended→running 同理。
    expect(missing.sort()).toEqual(["blocked→running", "queued→running", "suspended→running"]);
  });

  it("终态在两端都无出边", () => {
    for (const terminal of ["success", "cancelled"]) {
      expect(backend[terminal]).toEqual([]);
      expect(VALID_TRANSITIONS[terminal]).toEqual([]);
    }
  });
});
