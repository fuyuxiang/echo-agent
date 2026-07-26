import { describe, it, expect, afterEach } from "vitest";
import i18n from "../i18n";
import { relativeTime, dateTime, timeOfDay, fullTimestamp } from "./datetime";

afterEach(async () => { await i18n.changeLanguage("zh"); });

describe("时间格式化兜底", () => {
  it.each([
    ["relativeTime", relativeTime],
    ["dateTime", dateTime],
    ["timeOfDay", timeOfDay],
    ["fullTimestamp", fullTimestamp],
  ])("%s 对 null/undefined/空串/非法值返回占位符而不抛错", (_name, fn) => {
    // date-fns 遇到 Invalid Date 会抛 RangeError,进而整页白屏——这是兜底的意义。
    expect(fn(null)).toBe("-");
    expect(fn(undefined)).toBe("-");
    expect(fn("")).toBe("-");
    expect(fn("not-a-date")).toBe("-");
    expect(fn(NaN)).toBe("-");
  });

  it("可以自定义占位符", () => {
    expect(relativeTime(null, "从未")).toBe("从未");
    expect(dateTime("garbage", "未知")).toBe("未知");
  });
});

describe("绝对时间格式", () => {
  const iso = "2026-07-26T14:03:27";

  it("dateTime 输出到分钟", () => {
    expect(dateTime(iso)).toBe("2026-07-26 14:03");
  });

  it("timeOfDay 只输出时分秒", () => {
    expect(timeOfDay(iso)).toBe("14:03:27");
  });

  it("fullTimestamp 输出完整日期与秒", () => {
    expect(fullTimestamp(iso)).toBe("2026-07-26 14:03:27");
  });

  it("接受毫秒时间戳(cron next_run_ms)", () => {
    const ms = new Date(iso).getTime();
    expect(dateTime(ms)).toBe("2026-07-26 14:03");
  });

  it("时分秒不随语言变化(仅相对时间随语言)", async () => {
    const zh = timeOfDay(iso);
    await i18n.changeLanguage("en");
    expect(timeOfDay(iso)).toBe(zh);
  });
});

describe("相对时间跟随 i18n 语言", () => {
  const recent = () => new Date(Date.now() - 5 * 60 * 1000).toISOString();

  it("zh 下输出中文", () => {
    expect(relativeTime(recent())).toMatch(/前/);
  });

  it("en 下输出英文", async () => {
    await i18n.changeLanguage("en");
    expect(relativeTime(recent())).toMatch(/ago/);
  });
});
