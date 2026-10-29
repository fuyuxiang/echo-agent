import { describe, expect, it } from "vitest";
import { flattenModels } from "../agent-client";
import { findModelDisplayName, modelChoiceLabel, modelDisplayName, modelSourceLabel } from "../model-presentation";

describe("model presentation", () => {
  it("同一网关中的同名别名按远端模型区分，远端也相同时保留独立路由身份", () => {
    const models = [
      {id:"gateway/fast",label:"主力",remoteModelId:"fast",providerId:"gateway",providerLabel:"我的网关"},
      {id:"gateway/strong",label:"主力",remoteModelId:"strong",providerId:"gateway",providerLabel:"我的网关"},
    ];
    expect(models.map(model => modelChoiceLabel(model, models))).toEqual(["主力 · 个人 · 我的网关 · fast", "主力 · 个人 · 我的网关 · strong"]);
    const duplicateRoutes = models.map(model => ({...model,remoteModelId:"shared"}));
    expect(new Set(duplicateRoutes.map(model => modelChoiceLabel(model, duplicateRoutes))).size).toBe(2);
  });
  it("旧记录缺少目录信息时仅去除已知管理前缀", () => {
    expect(modelDisplayName("echoagent-ojlab/Echo Brook")).toBe("Echo Brook");
    expect(modelDisplayName("organization/Echo Reed")).toBe("Echo Reed");
    expect(modelDisplayName("Qwen/Qwen3-32B")).toBe("Qwen/Qwen3-32B");
    expect(modelDisplayName(undefined, "未选择模型")).toBe("未选择模型");
  });

  it("优先使用别名或真实远端名称，保留实际模型名中的斜杠", () => {
    expect(modelDisplayName({ id: "organization/Qwen/Qwen3", remoteModelId: "Qwen/Qwen3" })).toBe("Qwen/Qwen3");
    expect(modelDisplayName({ id: "custom-2/Echo Reed", remoteModelId: "Echo Reed" })).toBe("Echo Reed");
    expect(modelDisplayName({ id: "organization/Echo Reed", label: "团队主力模型", remoteModelId: "Echo Reed" })).toBe("团队主力模型");
    expect(modelDisplayName({ id: "echoagent-ojlab/Echo Reed", label: "echoagent-ojlab/Echo Reed" })).toBe("Echo Reed");
  });

  it("来源由显式元数据决定，历史记录可从保留前缀恢复", () => {
    expect(modelSourceLabel("echoagent-ojlab/Echo Reed")).toBe("内置");
    expect(modelSourceLabel("organization/Echo Reed")).toBe("组织");
    expect(modelSourceLabel({ id: "legacy", source: "legacy" })).toBe("个人");
    expect(modelChoiceLabel({ id: "organization/model", label: "主力模型", source: "organization", providerLabel: "研发团队" })).toBe("主力模型 · 组织 · 研发团队");
  });

  it("同名模型的目录身份、远端请求名称及连接信息不因展示而改变", () => {
    const list = {
      providers: [
        { id: "echoagent-ojlab", providerKind: "custom" as const, source: "builtin" as const, label: "内置模型" },
        { id: "organization", providerKind: "custom" as const, source: "organization" as const, label: "研发团队" },
        { id: "my-gateway", providerKind: "custom" as const, source: "personal" as const, label: "我的网关" },
      ],
      models: [
        { modelId: "echoagent-ojlab/Echo Reed", providerId: "echoagent-ojlab", remoteModelId: "Echo Reed" },
        { modelId: "organization/Echo Reed", providerId: "organization", remoteModelId: "Echo Reed" },
        { modelId: "my-gateway/Echo Reed", providerId: "my-gateway", remoteModelId: "Echo Reed" },
      ],
    };
    const options = flattenModels(list);
    expect(options.map((model) => model.label)).toEqual(["Echo Reed", "Echo Reed", "Echo Reed"]);
    expect(options.map((model) => model.id)).toEqual(list.models.map((model) => model.modelId));
    expect(options.map((model) => model.remoteModelId)).toEqual(["Echo Reed", "Echo Reed", "Echo Reed"]);
    expect(options.map(model => modelChoiceLabel(model, options))).toEqual(["Echo Reed · 内置", "Echo Reed · 组织 · 研发团队", "Echo Reed · 个人 · 我的网关"]);
    expect(findModelDisplayName(options, options[0].id)).toBe("Echo Reed");
    expect(findModelDisplayName([], "organization/已下线模型")).toBe("已下线模型");
  });
});
