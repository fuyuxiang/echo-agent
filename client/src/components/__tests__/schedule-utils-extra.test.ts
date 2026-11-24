/**
 * schedule-utils 补充测试：覆盖 automation-schedule.test.ts 未涉及的函数。
 */
import { describe, it, expect } from "vitest";
import {
  pad2,
  formatDateInputValue,
  parseTimeValue,
  sortMonthDays,
  sortMonths,
  getYearlyMaxDay,
  formatRunTime,
  scheduledAtIso,
  defaultSchedule,
  draftFromAutomation,
  automationFromDraft,
  buildDraft,
} from "../automation/schedule-utils";
import type { Automation } from "@/lib/types";

describe("pad2", () => {
  it("个位数补零", () => {
    expect(pad2(0)).toBe("00");
    expect(pad2(5)).toBe("05");
    expect(pad2(9)).toBe("09");
  });
  it("两位数不变", () => {
    expect(pad2(10)).toBe("10");
    expect(pad2(59)).toBe("59");
  });
});

describe("formatDateInputValue", () => {
  it("格式化为 YYYY-MM-DD", () => {
    expect(formatDateInputValue(new Date(2026, 0, 5))).toBe("2026-01-05");
    expect(formatDateInputValue(new Date(2026, 11, 25))).toBe("2026-12-25");
  });
});

describe("parseTimeValue", () => {
  it("解析 HH:MM", () => {
    expect(parseTimeValue("09:30")).toEqual({ hour: 9, minute: 30 });
    expect(parseTimeValue("00:00")).toEqual({ hour: 0, minute: 0 });
    expect(parseTimeValue("23:59")).toEqual({ hour: 23, minute: 59 });
  });
  it("非法值兜底为 0", () => {
    expect(parseTimeValue("")).toEqual({ hour: 0, minute: 0 });
    expect(parseTimeValue("abc")).toEqual({ hour: 0, minute: 0 });
  });
});

describe("sortMonthDays", () => {
  it("去重、过滤非法范围、排序", () => {
    expect(sortMonthDays([15, 1, 31, 1, 0, 32])).toEqual([1, 15, 31]);
  });
  it("空数组", () => {
    expect(sortMonthDays([])).toEqual([]);
  });
});

describe("sortMonths", () => {
  it("去重、过滤非法范围、排序", () => {
    expect(sortMonths([12, 1, 6, 0, 13, 1])).toEqual([1, 6, 12]);
  });
});

describe("getYearlyMaxDay", () => {
  it("大月 31 天", () => {
    for (const m of [1, 3, 5, 7, 8, 10, 12]) {
      expect(getYearlyMaxDay(m)).toBe(31);
    }
  });
  it("小月 30 天", () => {
    for (const m of [4, 6, 9, 11]) {
      expect(getYearlyMaxDay(m)).toBe(30);
    }
  });
  it("二月 29 天", () => {
    expect(getYearlyMaxDay(2)).toBe(29);
  });
});

describe("formatRunTime", () => {
  it("ISO → MM-DD HH:MM", () => {
    // 使用固定时间避免时区问题
    const iso = new Date(2026, 5, 15, 14, 30).toISOString();
    const result = formatRunTime(iso);
    expect(result).toMatch(/^\d{2}-\d{2} \d{2}:\d{2}$/);
  });
  it("空值返回空字符串", () => {
    expect(formatRunTime(undefined)).toBe("");
    expect(formatRunTime("")).toBe("");
  });
  it("非法 ISO 返回原值", () => {
    expect(formatRunTime("not-a-date")).toBe("not-a-date");
  });
});

describe("scheduledAtIso", () => {
  it("组合日期+时间为 ISO", () => {
    const result = scheduledAtIso({ scheduledDate: "2026-07-01", scheduledTime: "10:30" });
    expect(result).toBeTruthy();
    const d = new Date(result!);
    expect(d.getFullYear()).toBe(2026);
    expect(d.getMonth()).toBe(6); // July = 6
    expect(d.getDate()).toBe(1);
  });
  it("无 scheduledDate 返回 undefined", () => {
    expect(scheduledAtIso({ scheduledDate: "", scheduledTime: "10:00" })).toBeUndefined();
  });
  it("scheduledTime 为空时默认 09:00", () => {
    const result = scheduledAtIso({ scheduledDate: "2026-07-01", scheduledTime: "" });
    expect(result).toBeTruthy();
  });
});

describe("defaultSchedule", () => {
  it("默认 DAILY 09:00 全选工作日", () => {
    const s = defaultSchedule();
    expect(s.freq).toBe("DAILY");
    expect(s.interval).toBe(1);
    expect(s.byhour).toBe(9);
    expect(s.byminute).toBe(0);
    expect(s.byday).toHaveLength(7);
    expect(s.intervalHours).toBe(1);
  });
});

describe("draftFromAutomation / automationFromDraft round-trip", () => {
  it("Automation → Draft → Automation 保持核心字段", () => {
    const automation: Automation = {
      id: "a1",
      name: "测试任务",
      prompt: "做点事",
      cwds: ["/home"],
      status: "ACTIVE",
      modelId: "gpt-4",
      modelIsThinking: true,
      skills: ["skill1"],
      expertId: "exp1",
      expertName: "Expert",
      connectorIds: ["c1"],
      permissionMode: "fullAccess",
      scheduleType: "recurring",
      schedule: defaultSchedule(),
      scheduledDate: "2026-07-01",
      scheduledTime: "10:00",
      validFromDate: "",
      validUntilDate: "",
      pushToWeChat: false,
      createdAt: "2026-01-01T00:00:00Z",
      nextRunAt: "2026-07-01T10:00:00Z",
      lastRunAt: undefined,
    };

    const draft = draftFromAutomation(automation);
    expect(draft.id).toBe("a1");
    expect(draft.name).toBe("测试任务");
    expect(draft.skills).toEqual(["skill1"]);
    expect(draft.connectorIds).toEqual(["c1"]);

    const restored = automationFromDraft(draft, automation);
    expect(restored.id).toBe("a1");
    expect(restored.createdAt).toBe("2026-01-01T00:00:00Z");
    expect(restored.nextRunAt).toBe("2026-07-01T10:00:00Z");
    expect(restored.name).toBe("测试任务");
  });

  it("automationFromDraft 无 existing 时 createdAt 为空", () => {
    const draft = buildDraft(undefined, "/workspace/project");
    expect(draft.cwds).toEqual(["/workspace/project"]);
    draft.name = "新任务";
    const result = automationFromDraft(draft);
    expect(result.createdAt).toBe("");
    expect(result.nextRunAt).toBeUndefined();
  });
});
