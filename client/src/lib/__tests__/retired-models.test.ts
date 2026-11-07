import { describe, expect, it } from "vitest";
import { isRemovedModelId, withoutRemovedModels } from "../retired-models";
import { flattenModels, filterModelsByRuntimeCatalog, type ProviderListModel } from "../agent-client";

describe("removed chat models", () => {
  it("rejects old catalog metadata while preserving personal models with the same name", () => {
    const catalog = {
      providers: [
        { id: "echoagent-ojlab", source: "builtin" },
        { id: "old-alias", source: "builtin" },
        { id: "personal", source: "personal" },
        { id: "team", source: "organization" },
      ],
      models: [
        { modelId: "echoagent-ojlab/Echo Reed", providerId: "echoagent-ojlab" },
        { modelId: "old-alias/chat", providerId: "old-alias" },
        { modelId: "personal/Echo Reed", providerId: "personal" },
        { modelId: "organization/team", providerId: "team" },
        { modelId: "legacy/orphan", providerId: "missing-personal-connection" },
      ],
    };
    const original = structuredClone(catalog);
    const cleaned = withoutRemovedModels(catalog);
    expect(cleaned.providers.map(provider => provider.id)).toEqual(["personal", "team"]);
    expect(cleaned.models.map(model => model.modelId)).toEqual(["personal/Echo Reed", "organization/team", "legacy/orphan"]);
    expect(catalog).toEqual(original);
    expect(withoutRemovedModels(cleaned)).toEqual(cleaned);
    expect(isRemovedModelId("personal/Echo Reed")).toBe(false);
  });

  it("never returns retired IDs from flattened catalogs or a stale runtime fallback", () => {
    const catalog: ProviderListModel = {
      providers: [{ id: "echoagent-ojlab", providerKind: "custom" }, { id: "mine", providerKind: "custom", source: "personal" }],
      models: [{ modelId: "echoagent-ojlab/Echo Reed", providerId: "echoagent-ojlab" }, { modelId: "mine/chat", providerId: "mine" }],
    };
    const options = flattenModels(catalog);
    expect(options.map(option => option.id)).toEqual(["mine/chat"]);
    const stale = [{ ...options[0], id: "echoagent-ojlab/Echo Reed" }];
    expect(filterModelsByRuntimeCatalog(stale, [])).toEqual([]);
    expect(filterModelsByRuntimeCatalog(stale, ["echoagent-ojlab/Echo Reed"])).toEqual([]);
  });
});
