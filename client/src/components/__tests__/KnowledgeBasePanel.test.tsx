import { describe, it, expect, beforeEach, vi } from "vitest";
import { act, render, screen, fireEvent, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

// Mock native filesystem picker + tauri-kb-reader so the secure backend-owned
// selection flow remains testable outside Tauri.
const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...args: unknown[]) => invokeMock(...args) }));
vi.mock("@/lib/tauri-kb-reader", () => ({
  isTauriAvailable: () => true,
  createTauriDirectoryReader: () => ({
    listDir: async () => [],
    readText: async () => null,
  }),
}));

import { KnowledgeBasePanel } from "../KnowledgeBasePanel";
import { registerKbProvider, resetKbRegistry, listKbProviders, unregisterKbProvider } from "@/lib/knowledge-base";
import { resetKnowledgeSourceHydration } from "@/lib/kb-source-storage";

describe("KnowledgeBasePanel", () => {
  beforeEach(() => {
    resetKbRegistry();
    resetKnowledgeSourceHydration();
    localStorage.removeItem("echoagent.knowledge-sources.v1");
    invokeMock.mockReset();
    invokeMock.mockImplementation(async (command: string, args?: { path: string }) => command === "filesystem_resource_identity" ? { id: `test-${args?.path}`, canonicalPath: args?.path } : undefined);
  });

  it("无 provider 显示未配置", async () => {
    await act(async () => {
      render(<KnowledgeBasePanel />);
    });
    expect(screen.getByRole("heading", { name: "还没有添加知识源" })).toBeInTheDocument();
    expect(screen.queryByRole("textbox", { name: "搜索知识库" })).toBeNull();
  });

  it("有 provider 显示源数与名称", async () => {
    registerKbProvider({
      id: "local",
      label: "本地文件夹",
      isEnabled: () => true,
      list: () => [{ id: "1", title: "笔记" }],
    });
    render(<KnowledgeBasePanel />);
    // 源摘要异步加载。
    await waitFor(() => expect(screen.getByText(/1 个知识源/)).toBeInTheDocument());
    // 添加按钮存在。
    expect(screen.getByRole("button", { name: /添加本地文件夹/ })).toBeInTheDocument();
  });

  it("搜索命中显示结果(source + title)", async () => {
    registerKbProvider({
      id: "docs",
      label: "文档库",
      isEnabled: () => true,
      list: (q) =>
        q
          ? [{ id: "1", title: "React 指南", snippet: " Hooks" }].filter((e) =>
              e.title.includes(q),
            )
          : [{ id: "1", title: "React 指南" }],
    });
    render(<KnowledgeBasePanel />);
    fireEvent.change(await screen.findByRole("textbox", { name: "搜索知识库" }), {
      target: { value: "React" },
    });
    await waitFor(() => expect(screen.getByText("React 指南")).toBeInTheDocument());
    expect(document.querySelector(".kb-panel__row-source")).toHaveTextContent("文档库");
  });

  it("修改搜索词后立即移除旧结果，防抖期间不能打开上一次的文档", async () => {
    registerKbProvider({ id: "docs", label: "文档库", isEnabled: () => true,
      list: (q) => [{ id: q || "idle", title: `文档 ${q || "idle"}` }],
    });
    render(<KnowledgeBasePanel />);
    const input = await screen.findByRole("textbox", { name: "搜索知识库" });
    fireEvent.change(input, { target: { value: "first" } });
    expect(await screen.findByRole("button", { name: "打开知识条目：文档 first" })).toBeInTheDocument();
    fireEvent.change(input, { target: { value: "second" } });
    expect(screen.queryByRole("button", { name: "打开知识条目：文档 first" })).toBeNull();
    expect(screen.queryByText("未找到相关内容，请尝试其他关键词")).toBeNull();
    expect(screen.getByText("搜索中…")).toBeInTheDocument();
    expect(await screen.findByRole("button", { name: "打开知识条目：文档 second" })).toBeInTheDocument();
    fireEvent.change(input, { target: { value: "" } });
    expect(screen.queryByRole("button", { name: "打开知识条目：文档 second" })).toBeNull();
    expect(screen.queryByRole("list", { name: "知识库搜索结果" })).toBeNull();
  });

  it("桌面端展示后台排序的搜索结果，不展示后台模型细节", async () => {
    registerKbProvider({
      id: "notes",
      label: "个人笔记",
      isEnabled: () => true,
      list: vi.fn(() => [{ id: "legacy", title: "不应出现的旧结果" }]),
    });
    const status = {
      state: "ready",
      message: "语义索引已就绪",
      fileCount: 1,
      chunkCount: 3,
      embeddedChunkCount: 3,
      pendingEmbeddingCount: 0,
      lastUpdatedAt: Date.now(),
      embeddingModel: "embed-pro",
      rerankModel: "rerank-pro",
    };
    invokeMock.mockImplementation((command: string, args?: { path: string }) => {
      if (command === "filesystem_resource_identity") return Promise.resolve({ id: `test-${args?.path}`, canonicalPath: args?.path });
      if (command === "personal_knowledge_index_status") return Promise.resolve(status);
      if (command === "personal_knowledge_search") {
        return Promise.resolve({
          items: [{
            id: "/notes/policy.md#L4",
            title: "差旅政策",
            snippet: "高铁出行可报销二等座。",
            source: "notes",
            sourceLabel: "个人笔记",
            url: "/notes/policy.md",
            path: "/notes/policy.md",
            startLine: 4,
            endLine: 6,
            score: 0.97,
          }],
          retrievalMode: "hybrid-reranked",
          degradedReason: null,
          index: status,
        });
      }
      return Promise.resolve(undefined);
    });

    render(<KnowledgeBasePanel />);
    fireEvent.change(await screen.findByRole("textbox", { name: "搜索知识库" }), {
      target: { value: "出差坐高铁能报销吗" },
    });

    expect(await screen.findByRole("button", { name: "打开知识条目：差旅政策" })).toBeInTheDocument();
    expect(screen.queryByText("不应出现的旧结果")).toBeNull();
    expect(screen.queryByText("已使用关键词、向量检索和相关性重排")).not.toBeInTheDocument();
    expect(screen.queryByText(/embed-pro · rerank-pro/)).not.toBeInTheDocument();
    expect(invokeMock).toHaveBeenCalledWith("personal_knowledge_search", {
      query: "出差坐高铁能报销吗",
      limit: 20,
      requestId: expect.any(String),
    });
  });

  it("搜索无匹配显示空态", async () => {
    registerKbProvider({
      id: "docs",
      label: "文档库",
      isEnabled: () => true,
      list: () => [],
    });
    render(<KnowledgeBasePanel />);
    fireEvent.change(await screen.findByRole("textbox", { name: "搜索知识库" }), {
      target: { value: "不存在" },
    });
    await waitFor(() => expect(screen.getByText("未找到相关内容，请尝试其他关键词")).toBeInTheDocument());
  });

  it("搜索结果是键盘可达的原生按钮", async () => {
    const onOpen = vi.fn();
    const user = userEvent.setup();
    registerKbProvider({
      id: "docs",
      label: "文档库",
      isEnabled: () => true,
      list: () => [{ id: "9", title: "T", url: "https://x/9" }],
    });
    render(<KnowledgeBasePanel onOpen={onOpen} />);
    fireEvent.change(await screen.findByRole("textbox", { name: "搜索知识库" }), {
      target: { value: "T" },
    });
    const result = await screen.findByRole("button", { name: "打开知识条目：T" });
    result.focus();
    expect(result).toHaveFocus();
    await user.keyboard("{Enter}");
    expect(onOpen).toHaveBeenCalledWith("9", "https://x/9");
  });

  it("部分 provider 失败时保留可用结果并给出明确反馈", async () => {
    registerKbProvider({
      id: "bad",
      label: "云端文档",
      isEnabled: () => true,
      list: () => Promise.reject(new Error("连接超时")),
    });
    registerKbProvider({
      id: "good",
      label: "本地文档",
      isEnabled: () => true,
      list: () => [{ id: "ok", title: "可用结果" }],
    });
    render(<KnowledgeBasePanel />);

    fireEvent.change(await screen.findByRole("textbox", { name: "搜索知识库" }), {
      target: { value: "文档" },
    });

    expect(await screen.findByRole("button", { name: "打开知识条目：可用结果" })).toBeInTheDocument();
    expect(screen.getByRole("alert")).toHaveTextContent("部分知识源搜索失败");
    expect(screen.getByRole("alert")).toHaveTextContent("云端文档：连接超时");
    expect(screen.queryByText("未找到相关内容，请尝试其他关键词")).toBeNull();
    expect(screen.getByRole("button", { name: "重试搜索" })).toBeInTheDocument();
  });

  it("全部 provider 失败时显示可重试错误，重试成功后恢复结果", async () => {
    let attempts = 0;
    registerKbProvider({
      id: "flaky",
      label: "临时离线源",
      isEnabled: () => true,
      list: () => {
        attempts += 1;
        if (attempts === 1) return Promise.reject(new Error("索引暂不可用"));
        return [{ id: "recovered", title: "已恢复文档" }];
      },
    });
    render(<KnowledgeBasePanel />);

    fireEvent.change(await screen.findByRole("textbox", { name: "搜索知识库" }), {
      target: { value: "文档" },
    });

    expect(await screen.findByRole("alert")).toHaveTextContent("所有知识源搜索失败");
    expect(screen.queryByText("未找到相关内容，请尝试其他关键词")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "重试搜索" }));

    expect(await screen.findByRole("button", { name: "打开知识条目：已恢复文档" })).toBeInTheDocument();
    await waitFor(() => expect(screen.queryByRole("alert")).toBeNull());
  });

  it("「添加本地文件夹」弹出目录选择并注册稳定 provider", async () => {
    invokeMock.mockImplementation((command: string) =>
      Promise.resolve(command === "filesystem_pick_directory" ? "/my/notes" : command === "filesystem_resource_identity" ? { id: "test-my-notes", canonicalPath: "/my/notes" } : undefined));
    const before = listKbProviders().length;
    render(<KnowledgeBasePanel onToast={vi.fn()} />);
    fireEvent.click(await screen.findByRole("button", { name: /添加本地文件夹/ }));
    await waitFor(() => expect(listKbProviders().length).toBe(before + 1));
    expect(listKbProviders()).toEqual([
      expect.objectContaining({ id: expect.stringMatching(/^local-/), label: "本地：notes" }),
    ]);
    expect(invokeMock).toHaveBeenCalledWith("filesystem_pick_directory");
  });

  it("取消选择(返回 null)不注册 provider", async () => {
    invokeMock.mockImplementation((command: string) =>
      Promise.resolve(command === "filesystem_pick_directory" ? null : undefined));
    const before = listKbProviders().length;
    render(<KnowledgeBasePanel onToast={vi.fn()} />);
    fireEvent.click(await screen.findByRole("button", { name: /添加本地文件夹/ }));
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("filesystem_pick_directory"));
    expect(listKbProviders().length).toBe(before);
  });

  it("目录选择失败就地重试，待选期间不能重复打开或刷新，取消保持知识源", async () => {
    let finishPick!: (value: null) => void;
    let picks = 0;
    registerKbProvider({id:"existing",label:"现有资料",isEnabled:()=>true,list:()=>[]});
    invokeMock.mockImplementation(async (command: string) => {
      if (command !== "filesystem_pick_directory") return undefined;
      picks += 1;
      if (picks === 1) throw new Error("目录选择器不可用");
      return new Promise<null>(resolve=>{finishPick=resolve;});
    });
    render(<KnowledgeBasePanel />);
    fireEvent.click(await screen.findByRole("button",{name:/添加本地文件夹/}));
    expect(await screen.findByRole("alert")).toHaveTextContent("目录选择器不可用");
    fireEvent.click(screen.getByRole("button",{name:"重试操作"}));
    const pending = await screen.findByRole("button",{name:"选择文件夹中…"});
    expect(pending).toBeDisabled();
    expect(screen.getByRole("button",{name:/刷新索引/})).toBeDisabled();
    expect(screen.getByRole("button",{name:"移除知识源 现有资料"})).toBeDisabled();
    fireEvent.click(pending);
    expect(picks).toBe(2);
    await act(async()=>finishPick(null));
    expect(screen.getByRole("button",{name:/添加本地文件夹/})).toBeEnabled();
    expect(screen.queryByRole("alert")).toBeNull();
    expect(listKbProviders()).toHaveLength(1);
  });

  it("打开文件失败保留结果并可重试，打开期间不重复调用", async () => {
    let finishOpen!: () => void;
    const onOpen = vi.fn().mockRejectedValueOnce(new Error("文件不可读"))
      .mockImplementationOnce(()=>new Promise<void>(resolve=>{finishOpen=resolve;}));
    registerKbProvider({id:"opaque-id",label:"产品资料",isEnabled:()=>true,
      list:()=>[{id:"file",title:"交付记录",url:"/review/交付.md"}]});
    render(<KnowledgeBasePanel onOpen={onOpen} />);
    fireEvent.change(await screen.findByRole("textbox",{name:"搜索知识库"}),{target:{value:"交付"}});
    const result = await screen.findByRole("button",{name:"打开知识条目：交付记录"});
    expect(result).toHaveTextContent("产品资料");
    fireEvent.click(result);
    expect(await screen.findByRole("alert")).toHaveTextContent("文件不可读");
    fireEvent.click(screen.getByRole("button",{name:"重试操作"}));
    expect(result).toBeDisabled();
    fireEvent.click(result);
    expect(onOpen).toHaveBeenCalledTimes(2);
    await act(async()=>finishOpen());
    expect(onOpen).toHaveBeenLastCalledWith("file","/review/交付.md");
    expect(result).toBeEnabled();
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it.each([false, true])("切换查询后不显示旧文件迟到的失败或重试（返回原查询=%s）", async (returnToOriginal) => {
    let rejectOpen!: (error: Error) => void;
    const onOpen = vi.fn(() => new Promise<void>((_, reject) => { rejectOpen = reject; }));
    const onToast = vi.fn();
    registerKbProvider({ id: "docs", label: "资料", isEnabled: () => true,
      list: query => [{ id: query || "idle", title: `文档 ${query}` }],
    });
    render(<KnowledgeBasePanel onOpen={onOpen} onToast={onToast} />);
    const input = await screen.findByRole("textbox", { name: "搜索知识库" });
    fireEvent.change(input, { target: { value: "first" } });
    fireEvent.click(await screen.findByRole("button", { name: "打开知识条目：文档 first" }));
    fireEvent.change(input, { target: { value: "second" } });
    await screen.findByRole("button", { name: "打开知识条目：文档 second" });
    if (returnToOriginal) {
      fireEvent.change(input, { target: { value: "first" } });
      await screen.findByRole("button", { name: "打开知识条目：文档 first" });
    }
    await act(async () => { rejectOpen(new Error("旧文件不可读")); });
    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.queryByRole("button", { name: "重试操作" })).toBeNull();
    expect(onToast).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: /打开知识条目/ })).toBeEnabled();
  });

  it("离开知识库后，迟到的打开失败不再弹出旧文件反馈", async () => {
    let rejectOpen!: (error: Error) => void;
    const onToast = vi.fn();
    registerKbProvider({ id: "docs", label: "资料", isEnabled: () => true, list: () => [{ id: "file", title: "资料" }] });
    const { unmount } = render(<KnowledgeBasePanel onToast={onToast}
      onOpen={() => new Promise<void>((_, reject) => { rejectOpen = reject; })} />);
    fireEvent.change(await screen.findByRole("textbox", { name: "搜索知识库" }), { target: { value: "资料" } });
    fireEvent.click(await screen.findByRole("button", { name: "打开知识条目：资料" }));
    unmount();
    await act(async () => { rejectOpen(new Error("文件不可读")); });
    expect(onToast).not.toHaveBeenCalled();
  });

  it("索引刷新完成不覆盖同时发生的文件打开错误，仍可重试该文件", async () => {
    let finishRebuild!: (status: null) => void;
    invokeMock.mockImplementation(async (command: string) => command === "personal_knowledge_rebuild"
      ? new Promise(resolve => { finishRebuild = resolve; }) : undefined);
    registerKbProvider({ id: "docs", label: "资料", isEnabled: () => true,
      list: () => [{ id: "file", title: "交付说明" }], rebuild: async () => 1,
    });
    const onOpen = vi.fn().mockRejectedValueOnce(new Error("文件不可读")).mockResolvedValue(undefined);
    render(<KnowledgeBasePanel onOpen={onOpen} />);
    fireEvent.change(await screen.findByRole("textbox", { name: "搜索知识库" }), { target: { value: "交付" } });
    const result = await screen.findByRole("button", { name: "打开知识条目：交付说明" });
    fireEvent.click(screen.getByRole("button", { name: /刷新索引/ }));
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("personal_knowledge_rebuild"));
    fireEvent.click(result);
    expect(await screen.findByRole("alert")).toHaveTextContent("文件不可读");
    await act(async () => { finishRebuild(null); });
    expect(screen.getByRole("alert")).toHaveTextContent("文件不可读");
    fireEvent.click(screen.getByRole("button", { name: "重试操作" }));
    await waitFor(() => expect(screen.queryByRole("alert")).toBeNull());
    expect(onOpen).toHaveBeenCalledTimes(2);
    expect(onOpen).toHaveBeenLastCalledWith("file", undefined);
    expect(screen.getByText("知识库已更新，可以搜索最新内容")).toBeInTheDocument();
  });

  it.each([false, true])("慢速索引刷新中可以打开文件和持久移除来源，旧刷新结束不覆盖移除反馈（失败=%s）", async (fails) => {
    let finishRebuild!: () => void;
    let stored = [
      { id: "first", kind: "local-folder", root: "/review/first", label: "第一份资料", enabled: true },
      { id: "second", kind: "local-folder", root: "/review/second", label: "第二份资料", enabled: true },
    ];
    invokeMock.mockImplementation(async (command: string, args?: { sources: typeof stored }) => {
      if (command === "org_local_kb_sources_get") return { exists: true, sources: stored };
      if (command === "org_local_kb_sources_set") { stored = args!.sources; return; }
      if (command === "personal_knowledge_rebuild") return new Promise((resolve, reject) => {
        finishRebuild = () => fails ? reject(new Error("旧目录已移除")) : resolve(null);
      });
      return null;
    });
    for (const source of stored) registerKbProvider({ id: source.id, label: source.label, isEnabled: () => true,
      list: () => [{ id: source.root, title: source.label, url: `${source.root}/file.md` }], rebuild: async () => 1,
    });
    const onOpen = vi.fn();
    const onToast = vi.fn();
    render(<KnowledgeBasePanel onOpen={onOpen} onToast={onToast} />);
    fireEvent.change(await screen.findByRole("textbox", { name: "搜索知识库" }), { target: { value: "资料" } });
    const result = await screen.findByRole("button", { name: "打开知识条目：第一份资料" });
    fireEvent.click(screen.getByRole("button", { name: /刷新索引/ }));
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("personal_knowledge_rebuild"));
    const refreshing = screen.getByRole("button", { name: "刷新中…" });
    expect(refreshing).toBeDisabled();
    fireEvent.click(refreshing);
    expect(invokeMock.mock.calls.filter(([command]) => command === "personal_knowledge_rebuild")).toHaveLength(1);
    expect(result).toBeEnabled();
    fireEvent.click(result);
    await waitFor(() => expect(result).toBeEnabled());
    expect(onOpen).toHaveBeenCalledWith("/review/first", "/review/first/file.md");
    fireEvent.click(screen.getByRole("button", { name: "移除知识源 第一份资料" }));
    await waitFor(() => expect(stored.map(source => source.id)).toEqual(["second"]));
    expect(await screen.findByRole("button", { name: "打开知识条目：第二份资料" })).toBeEnabled();
    expect(screen.queryByRole("button", { name: "打开知识条目：第一份资料" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "移除知识源 第二份资料" }));
    await screen.findByRole("heading", { name: "还没有添加知识源" });
    expect(stored).toEqual([]);
    await act(async () => { finishRebuild(); });
    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.getByText("已移除知识源，原文件仍保留在原目录")).toBeInTheDocument();
    expect(screen.queryByText("知识库已更新，可以搜索最新内容")).toBeNull();
    expect(screen.queryByText(/正在刷新索引/)).toBeNull();
    expect(listKbProviders()).toEqual([]);
    expect(onToast).not.toHaveBeenCalledWith(expect.stringContaining("重建失败"));
  });

  it("「移除知识源」按钮注销已注册 provider", async () => {
    registerKbProvider({
      id: "local",
      label: "本地文件夹",
      isEnabled: () => true,
      list: () => [],
    });
    render(<KnowledgeBasePanel onToast={vi.fn()} />);
    await waitFor(() => expect(screen.getByText("本地文件夹")).toBeInTheDocument());
    fireEvent.click(screen.getByRole("button", { name: "移除知识源 本地文件夹" }));
    await waitFor(() => expect(listKbProviders().some((s) => s.id === "local")).toBe(false));
    // 直接确认 registry 也已移除。
    expect(unregisterKbProvider("local")).toBe(false); // 已移除 → 再移除返回 false
  });

  it("刷新知识库后用简短提示说明内容可搜索", async () => {
    const rebuild = vi.fn(async () => 5);
    registerKbProvider({
      id: "local",
      label: "本地文件夹",
      isEnabled: () => true,
      list: () => [],
      rebuild,
    });
    const onToast = vi.fn();
    render(<KnowledgeBasePanel onToast={onToast} />);
    const refreshBtn = await screen.findByRole("button", { name: /刷新索引/ });
    fireEvent.click(refreshBtn);
    await waitFor(() => expect(rebuild).toHaveBeenCalledTimes(1));
    expect(onToast).toHaveBeenCalledWith("知识库已更新，可以搜索最新内容");
  });

  it("语义索引降级时说明关键词仍可搜索，不提示全部能力已更新", async () => {
    registerKbProvider({ id: "docs", label: "资料", isEnabled: () => true, list: () => [], rebuild: async () => 1 });
    invokeMock.mockImplementation(async (command: string) => command === "personal_knowledge_rebuild" ? {
      state: "degraded", fileCount: 1, chunkCount: 1, embeddedChunkCount: 0,
      pendingEmbeddingCount: 1, embeddingModel: "embedding", rerankModel: "reranker",
    } : undefined);
    const onToast = vi.fn();
    render(<KnowledgeBasePanel onToast={onToast} />);
    fireEvent.click(await screen.findByRole("button", { name: /刷新索引/ }));
    expect(await screen.findByText("索引已刷新，部分搜索能力暂时不可用，可继续使用关键词搜索")).toBeInTheDocument();
    expect(onToast).not.toHaveBeenCalledWith("知识库已更新，可以搜索最新内容");
  });

  it("重建索引后刷新当前查询，移除知识源后不再显示旧命中", async () => {
    let title = "旧索引内容";
    registerKbProvider({
      id: "changing", label: "可更新知识源", isEnabled: () => true,
      list: () => [{ id: "doc", title }],
      rebuild: async () => { title = "新索引内容"; return 1; },
    });
    registerKbProvider({ id: "empty", label: "空知识源", isEnabled: () => true, list: () => [] });
    render(<KnowledgeBasePanel />);
    fireEvent.change(await screen.findByRole("textbox", { name: "搜索知识库" }), { target: { value: "内容" } });
    expect(await screen.findByText("旧索引内容")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /刷新索引/ }));
    expect(await screen.findByText("新索引内容")).toBeInTheDocument();
    expect(screen.queryByText("旧索引内容")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "移除知识源 可更新知识源" }));
    await waitFor(() => expect(screen.queryByText("新索引内容")).toBeNull());
    expect(await screen.findByText("未找到相关内容，请尝试其他关键词")).toBeInTheDocument();
  });

  it("索引重建部分失败显示失败源及可重试入口，不误报全部成功", async () => {
    const rebuild = vi.fn().mockRejectedValueOnce(new Error("目录不可读")).mockResolvedValueOnce(1);
    registerKbProvider({ id: "flaky-index", label: "离线目录", isEnabled: () => true, list: () => [], rebuild });
    const onToast = vi.fn();
    render(<KnowledgeBasePanel onToast={onToast} />);
    fireEvent.click(await screen.findByRole("button", { name: /刷新索引/ }));
    expect(await screen.findByRole("alert")).toHaveTextContent("离线目录：目录不可读");
    expect(onToast).not.toHaveBeenCalledWith(expect.stringContaining("已重建索引"));
    fireEvent.click(screen.getByRole("button", { name: "重试刷新索引" }));
    await waitFor(() => expect(screen.queryByRole("alert")).toBeNull());
    expect(rebuild).toHaveBeenCalledTimes(2);
  });

  it("无知识源时不显示「刷新索引」按钮", async () => {
    await act(async () => {
      render(<KnowledgeBasePanel onToast={vi.fn()} />);
    });
    expect(screen.queryByRole("button", { name: /刷新索引/ })).toBeNull();
  });

  it("索引状态指示:显示已索引文件数 + 最近更新时间", async () => {
    registerKbProvider({
      id: "local",
      label: "本地文件夹",
      isEnabled: () => true,
      list: () => [],
      getStats: async () => ({ fileCount: 12, lastRebuiltAt: Date.now() - 60000 }),
    });
    render(<KnowledgeBasePanel onToast={vi.fn()} />);
    // 已索引 12 个文件。
    await waitFor(() => expect(screen.getByText(/已索引 12 个文件/)).toBeInTheDocument());
    // 最近更新(相对时间,1 分钟前)。
    expect(screen.getByText(/分钟前/)).toBeInTheDocument();
  });

  it("索引状态:无 fileCount 时不显示数字,无 lastRebuiltAt 时不显示时间", async () => {
    registerKbProvider({
      id: "local",
      label: "本地文件夹",
      isEnabled: () => true,
      list: () => [],
      getStats: async () => ({}),
    });
    render(<KnowledgeBasePanel onToast={vi.fn()} />);
    // fileCount 未定义代表尚未扫描，不能误报为知识源中没有文件。
    await waitFor(() => expect(screen.getByText(/等待首次扫描/)).toBeInTheDocument());
    // 无时间。
    expect(screen.queryByText(/分钟前|小时前|刚刚|天前/)).toBeNull();
  });
});
