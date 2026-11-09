import { beforeEach, describe, expect, it, vi } from "vitest";
import { preparePromptWithPersonalKnowledge } from "../knowledge-context";
import { registerKbProvider, resetKbRegistry } from "../knowledge-base";
import { useKnowledgeStore } from "@/stores/knowledge-store";

const semanticSearchMock = vi.hoisted(() => vi.fn());
const cancelSemanticSearchMock = vi.hoisted(() => vi.fn());
vi.mock("../personal-knowledge", () => ({
  searchPersonalKnowledge: semanticSearchMock,
  cancelPersonalKnowledgeSearch: cancelSemanticSearchMock,
  createPersonalKnowledgeSearchRequestId: () => "personal-search-test",
}));

describe("personal knowledge prompt preparation", () => {
  beforeEach(() => {
    resetKbRegistry();
    semanticSearchMock.mockReset();
    semanticSearchMock.mockResolvedValue(null);
    cancelSemanticSearchMock.mockReset();
    cancelSemanticSearchMock.mockResolvedValue(true);
    useKnowledgeStore.setState({
      defaultSources: [],
      defaultOrganizationScopeIds: [],
      sessionSources: {},
      sessionOrganizationScopeIds: {},
      sourceCount: 0,
      retrievals: {},
      turnTraces: {},
    });
  });

  it("优先使用语义检索结果注入任务，不再重复扫描 provider", async () => {
    useKnowledgeStore.getState().setSessionSources("session-semantic", ["personal"]);
    const legacyList = vi.fn(() => [{ id: "legacy", title: "旧结果" }]);
    registerKbProvider({
      id: "local-notes",
      label: "本地笔记",
      isEnabled: () => true,
      list: legacyList,
    });
    semanticSearchMock.mockResolvedValue({
      items: [{
        id: "/notes/travel.md#L8",
        title: "差旅制度",
        snippet: "住宿标准为每晚 500 元。",
        source: "local-notes",
        sourceLabel: "本地笔记",
        url: "/notes/travel.md",
        path: "/notes/travel.md",
        startLine: 8,
        endLine: 10,
        score: 0.96,
      }],
      retrievalMode: "hybrid-reranked",
      degradedReason: null,
      index: {
        state: "ready",
        fileCount: 1,
        chunkCount: 1,
        embeddedChunkCount: 1,
        pendingEmbeddingCount: 0,
        embeddingModel: "embed-pro",
        rerankModel: "rerank-pro",
      },
    });

    const result = await preparePromptWithPersonalKnowledge(
      "session-semantic",
      "请回答差旅标准",
      "出差住宿可以报销多少？",
      "prompt-semantic",
    );

    expect(semanticSearchMock).toHaveBeenCalledWith(
      "出差住宿可以报销多少？",
      5,
      "personal-search-test",
    );
    expect(legacyList).not.toHaveBeenCalled();
    expect(result.promptText).toContain("住宿标准为每晚 500 元");
    expect(result.promptText).toContain("来源：本地笔记 · /notes/travel.md");
    expect(useKnowledgeStore.getState().retrievals["session-semantic"]).toMatchObject({
      state: "used",
      resultCount: 1,
      titles: ["差旅制度"],
    });
    expect(useKnowledgeStore.getState().turnTraces["session-semantic"]["prompt-semantic"])
      .toMatchObject({
        personal: {
          state: "used",
          items: [{
            title: "差旅制度",
            path: "/notes/travel.md",
            sourceLabel: "本地笔记",
            startLine: 8,
            endLine: 10,
          }],
        },
      });
  });

  it("自然语言问题命中关键词后注入带来源的知识片段", async () => {
    useKnowledgeStore.getState().setSessionSources("session-1", ["personal"]);
    registerKbProvider({
      id: "handbook",
      label: "员工手册",
      isEnabled: () => true,
      list: vi.fn((query?: string) => query?.includes("年假") ? [{
        id: "/docs/leave.md",
        title: "休假制度",
        snippet: "员工每年享有十天年假。",
        url: "/docs/leave.md",
      }] : []),
    });

    const result = await preparePromptWithPersonalKnowledge(
      "session-1",
      "公司的年假政策是什么？",
      "公司的年假政策是什么？",
    );

    expect(result.resultCount).toBe(1);
    expect(result.promptText).toContain("<echoagent_personal_knowledge>");
    expect(result.promptText).toContain("[个人知识 1] 休假制度");
    expect(result.promptText).toContain("员工每年享有十天年假");
    expect(result.promptText).toContain("公司的年假政策是什么？");
    expect(useKnowledgeStore.getState().retrievals["session-1"]).toMatchObject({
      state: "used",
      resultCount: 1,
    });
  });

  it("尚无可检索索引时保留任务，并在轮次里说明稍后重试", async () => {
    useKnowledgeStore.getState().setSessionSources("session-indexing", ["personal"]);
    registerKbProvider({ id: "local", label: "本地", isEnabled: () => true, list: vi.fn() });
    semanticSearchMock.mockResolvedValue({ items: [], retrievalMode: "none",
      degradedReason: "个人知识索引正在后台建立", index: { state: "indexing" } });
    const result = await preparePromptWithPersonalKnowledge("session-indexing", "原始任务", "查找制度", "turn");
    expect(result).toMatchObject({ promptText: "原始任务", resultCount: 0 });
    expect(useKnowledgeStore.getState().turnTraces["session-indexing"].turn.personal).toEqual({
      state: "blocked", message: "个人知识索引正在后台建立。索引完成后可重新提问",
    });
  });

  it("区分真正未命中与仅使用关键词索引后的未命中", async () => {
    useKnowledgeStore.getState().setSessionSources("session-empty", ["personal"]);
    registerKbProvider({ id: "local", label: "本地", isEnabled: () => true, list: vi.fn() });
    semanticSearchMock.mockResolvedValueOnce({ items: [], retrievalMode: "keyword",
      degradedReason: "语义索引正在后台生成", index: { state: "degraded" } });
    semanticSearchMock.mockResolvedValueOnce({ items: [], retrievalMode: "hybrid",
      degradedReason: null, index: { state: "ready" } });
    await preparePromptWithPersonalKnowledge("session-empty", "原始任务", "查找制度", "keyword");
    await preparePromptWithPersonalKnowledge("session-empty", "原始任务", "查找制度", "hybrid");
    const traces = useKnowledgeStore.getState().turnTraces["session-empty"];
    expect(traces.keyword.personal).toMatchObject({ state: "no-match", keywordOnly: true, notice: "语义索引正在后台生成" });
    expect(traces.hybrid.personal).toEqual({ state: "no-match", sourceCount: 1 });
  });

  it("会话关闭知识库后完全跳过检索", async () => {
    const list = vi.fn(() => [{ id: "x", title: "不应读取" }]);
    registerKbProvider({ id: "local", label: "本地", isEnabled: () => true, list });
    useKnowledgeStore.getState().setSessionSources("session-off", []);

    const result = await preparePromptWithPersonalKnowledge("session-off", "原始提示", "用户问题");

    expect(result.promptText).toBe("原始提示");
    expect(list).not.toHaveBeenCalled();
  });

  it("知识源失败不会阻止原任务发送，并留下可见错误状态", async () => {
    useKnowledgeStore.getState().setSessionSources("session-2", ["personal"]);
    registerKbProvider({
      id: "broken",
      label: "失效目录",
      isEnabled: () => true,
      list: async () => { throw new Error("目录不可读"); },
    });

    const result = await preparePromptWithPersonalKnowledge("session-2", "继续完成任务", "查找规范");

    expect(result.promptText).toBe("继续完成任务");
    expect(useKnowledgeStore.getState().retrievals["session-2"]).toMatchObject({
      state: "error",
      message: expect.stringContaining("目录不可读"),
    });
  });

  it("原生语义检索失败后不通过第二条 JS 路径重复读取文件", async () => {
    const legacyList = vi.fn(() => [{ id: "x", title: "不应读取" }]);
    registerKbProvider({ id: "local", label: "本地", isEnabled: () => true, list: legacyList });
    useKnowledgeStore.getState().setSessionSources("session-native-error", ["personal"]);
    semanticSearchMock.mockRejectedValue(new Error("原生索引不可用"));

    const result = await preparePromptWithPersonalKnowledge(
      "session-native-error",
      "原始提示",
      "查询内部制度",
    );

    expect(result.promptText).toBe("原始提示");
    expect(legacyList).not.toHaveBeenCalled();
    expect(useKnowledgeStore.getState().retrievals["session-native-error"]).toMatchObject({
      state: "error",
      message: "原生索引不可用",
    });
  });

  it("语义检索超时时取消原生任务，且仍保留原提示词", async () => {
    vi.useFakeTimers();
    try {
      useKnowledgeStore.getState().setSessionSources("session-timeout", ["personal"]);
      registerKbProvider({
        id: "local",
        label: "本地文档",
        isEnabled: () => true,
        list: () => [],
      });
      semanticSearchMock.mockReturnValue(new Promise(() => {}));

      const pending = preparePromptWithPersonalKnowledge(
        "session-timeout",
        "原始任务",
        "查询内部制度",
        "prompt-timeout",
      );
      await vi.advanceTimersByTimeAsync(12_000);

      await expect(pending).resolves.toMatchObject({ promptText: "原始任务", resultCount: 0 });
      expect(cancelSemanticSearchMock).toHaveBeenCalledWith("personal-search-test");
      expect(useKnowledgeStore.getState().retrievals["session-timeout"]).toMatchObject({
        state: "error",
        message: "个人知识库检索超时",
      });
    } finally {
      vi.useRealTimers();
    }
  });
});
