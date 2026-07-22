import { describe, it, expect, afterEach } from "vitest";
import i18n from "../i18n";
import { statusMeta } from "./kanban";

afterEach(async () => { await i18n.changeLanguage("zh"); });

describe("statusMeta 随语言切换", () => {
  it("zh 下 running 标签为执行中", () => {
    expect(statusMeta("running").label).toBe("执行中");
  });
  it("en 下 running 标签为 Running", async () => {
    await i18n.changeLanguage("en");
    expect(statusMeta("running").label).toBe("Running");
  });
  it("配色不随语言变", async () => {
    const zhChip = statusMeta("running").chip;
    expect(zhChip).toContain("indigo");
    await i18n.changeLanguage("en");
    expect(statusMeta("running").chip).toBe(zhChip);
  });
});
