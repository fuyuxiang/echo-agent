import { describe, expect, it } from "vitest";
import {
  connectionBaseUrlError, connectionName, parseModelLimits, providerDisplayName, serviceKind, SERVICE_GROUPS, PROVIDER_PRESETS,
} from "../provider-presets";

describe("provider presets", () => {
  it("recognizes Kimi and Zhipu shortcuts on the custom transport", () => {
    expect(serviceKind({ id: "c", providerKind: "custom", baseUrl: "https://api.moonshot.cn/v1/" })).toBe("kimi");
    expect(serviceKind({ id: "c", providerKind: "custom", baseUrl: "https://open.bigmodel.cn/api/paas/v4" })).toBe("zhipu");
    expect(serviceKind({ id: "c", providerKind: "custom", baseUrl: "https://api.moonshot.cn/v1", apiBackend: "responses" })).toBe("custom");
  });

  it("names custom connections by host and organization connections by label", () => {
    expect(connectionName({ id: "c", providerKind: "custom", baseUrl: "https://gw.example.com/v1", label: "OpenAI 兼容" })).toBe("gw.example.com");
    expect(connectionName({ id: "o", providerKind: "custom", source: "organization" })).toBe("组织提供");
  });

  it("displays the provider type for unnamed connections and preserves saved names", () => {
    const gateway = { id: "c", providerKind: "custom" as const, baseUrl: "https://gw.example.com/v1" };
    expect(providerDisplayName(gateway)).toBe("OpenAI 兼容");
    expect(providerDisplayName({ ...gateway, providerKind: "custom_anthropic" })).toBe("Anthropic 兼容");
    expect(providerDisplayName({ ...gateway, label: "团队网关" })).toBe("团队网关");
    expect(providerDisplayName({ ...gateway, baseUrl: "https://api.moonshot.cn/v1" })).toBe("Kimi");
    expect(providerDisplayName({ ...gateway, source: "organization" })).toBe("组织提供");
  });

  it("keeps URL validation messages", () => {
    expect(connectionBaseUrlError("", false)).toBe("请填写 Base URL。");
    expect(connectionBaseUrlError("http://a.test/v1", false)).toMatch(/明文传输/);
    expect(connectionBaseUrlError("https://a.test/:8080/v1", false)).toMatch(/端口号位置不对/);
  });

  it("validates limits", () => {
    expect(parseModelLimits({ contextWindow: "100", maxOutputTokens: "200" })).toBe("最大输出不能超过上下文窗口。");
  });

  it("lists every preset in exactly one group", () => {
    const kinds = SERVICE_GROUPS.flatMap(group => group.kinds);
    expect(new Set(kinds).size).toBe(kinds.length);
    expect(kinds.sort()).toEqual(Object.keys(PROVIDER_PRESETS).sort());
  });
});
