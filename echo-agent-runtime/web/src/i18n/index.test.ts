import { describe, it, expect } from "vitest";
import i18n from "./index";

describe("i18n 初始化", () => {
  it("默认语言可用且回退 zh", () => {
    expect(["zh", "en"]).toContain(i18n.resolvedLanguage);
    expect(i18n.options.fallbackLng).toContain("zh");
  });

  it("切到 en 后 nav 文案变英文", async () => {
    await i18n.changeLanguage("en");
    expect(i18n.t("nav:overview")).toBe("Overview");
    await i18n.changeLanguage("zh");
    expect(i18n.t("nav:overview")).toBe("概览");
  });
});
