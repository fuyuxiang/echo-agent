import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const workers = vi.hoisted(() => ({
  editor: vi.fn(function () { return { kind: "editor" }; }),
  css: vi.fn(function () { return { kind: "css" }; }),
  html: vi.fn(function () { return { kind: "html" }; }),
  json: vi.fn(function () { return { kind: "json" }; }),
  typescript: vi.fn(function () { return { kind: "typescript" }; }),
}));
vi.mock("monaco-editor/esm/vs/editor/editor.worker?worker", () => ({ default: workers.editor }));
vi.mock("monaco-editor/esm/vs/language/css/css.worker?worker", () => ({ default: workers.css }));
vi.mock("monaco-editor/esm/vs/language/html/html.worker?worker", () => ({ default: workers.html }));
vi.mock("monaco-editor/esm/vs/language/json/json.worker?worker", () => ({ default: workers.json }));
vi.mock("monaco-editor/esm/vs/language/typescript/ts.worker?worker", () => ({ default: workers.typescript }));
vi.mock("monaco-editor", () => ({ editor: { createDiffEditor: vi.fn(), defineTheme: vi.fn() } }));

describe("local Monaco loading", () => {
  const scope = globalThis as typeof globalThis & { MonacoEnvironment?: import("monaco-editor").Environment };
  beforeEach(() => {
    vi.resetModules();
    vi.clearAllMocks();
    delete scope.MonacoEnvironment;
    document.documentElement.dataset.theme = "light";
  });
  afterEach(() => { vi.restoreAllMocks(); delete scope.MonacoEnvironment; });

  it("实际 loader 绑定本地实例，初始化不请求默认 CDN 的脚本", async () => {
    const { loader } = await import("@monaco-editor/react");
    const { prepareLocalMonaco } = await import("../lib/local-monaco");
    const append = vi.spyOn(document.body, "appendChild");
    const first = prepareLocalMonaco();
    expect(prepareLocalMonaco()).toBe(first);
    await first;
    expect(await loader.init()).toBe(await import("monaco-editor"));
    expect(append).not.toHaveBeenCalled();
  });

  it("编辑、TS/JS、JSON、CSS和HTML路由到同源打包 worker 构造器", async () => {
    const { prepareLocalMonaco } = await import("../lib/local-monaco");
    await prepareLocalMonaco();
    for (const [label, expected] of [["editorWorkerService", "editor"], ["typescript", "typescript"],
      ["javascript", "typescript"], ["json", "json"], ["css", "css"], ["less", "css"],
      ["scss", "css"], ["html", "html"], ["handlebars", "html"], ["razor", "html"]]) {
      expect(scope.MonacoEnvironment?.getWorker?.("module", label)).toEqual({ kind: expected });
    }
    expect(workers.editor).toHaveBeenCalledOnce();
    expect(workers.typescript).toHaveBeenCalledTimes(2);
  });

  it("初始化异常向调用者传播，失败缓存清理后允许真正重试", async () => {
    const { loader } = await import("@monaco-editor/react");
    vi.spyOn(loader, "config").mockImplementationOnce(() => { throw new Error("本地编辑器初始化失败"); });
    const { prepareLocalMonaco } = await import("../lib/local-monaco");
    await expect(prepareLocalMonaco()).rejects.toThrow("本地编辑器初始化失败");
    await expect(prepareLocalMonaco()).resolves.toBeUndefined();
    expect(loader.config).toHaveBeenCalledTimes(2);
  });

  it("重新打开时更新当前主题，并保持单次初始化和共享 Promise", async () => {
    const { loader } = await import("@monaco-editor/react");
    const config = vi.spyOn(loader, "config");
    const { editor } = await import("monaco-editor");
    const { prepareLocalMonaco } = await import("../lib/local-monaco");
    const first = prepareLocalMonaco();
    expect(prepareLocalMonaco()).toBe(first);
    await first;
    expect(editor.defineTheme).toHaveBeenLastCalledWith("echo-light", expect.objectContaining({ base: "vs" }));
    document.documentElement.dataset.theme = "dark";
    expect(prepareLocalMonaco()).toBe(first);
    await first;
    expect(editor.defineTheme).toHaveBeenLastCalledWith("echo-dark", expect.objectContaining({ base: "vs-dark" }));
    expect(config).toHaveBeenCalledOnce();
  });
});
