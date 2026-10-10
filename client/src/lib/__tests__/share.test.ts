import { describe, it, expect, vi } from "vitest";
import {
  buildSharePayload,
  buildShareHtml,
  buildMailtoUrl,
  buildDownloadUrl,
  triggerDownload,
  copyShareText,
  systemShare,
} from "../share";
import type { ChatMessage } from "@/stores/session-store";

const messages: ChatMessage[] = [
  { id: "u1", role: "user", complete: true, parts: [{ kind: "text", text: "你好" }] },
  { id: "a1", role: "assistant", complete: true, parts: [{ kind: "text", text: "你好!" }] },
];

describe("buildSharePayload", () => {
  it("markdown 格式", () => {
    const p = buildSharePayload(messages, "markdown", "测试");
    expect(p.filename).toBe("测试.md");
    expect(p.mime).toContain("text/markdown");
    expect(p.content).toContain("你好");
    expect(p.content).toContain("# 测试");
    expect(p.bytes).toBeGreaterThan(0);
  });

  it("html 格式自包含", () => {
    const p = buildSharePayload(messages, "html", "测试");
    expect(p.filename).toBe("测试.html");
    expect(p.mime).toContain("text/html");
    expect(p.content).toContain("<!DOCTYPE html>");
    expect(p.content).toContain("</html>");
  });

  it("text 格式去掉 markdown 标记", () => {
    const p = buildSharePayload(messages, "text");
    expect(p.filename.endsWith(".txt")).toBe(true);
    expect(p.mime).toContain("text/plain");
    // 不应包含 markdown 标题井号开头行。
    expect(/^\s*#{1,6}\s/.test(p.content)).toBe(false);
  });

  it("文件名被 sanitize", () => {
    const p = buildSharePayload(messages, "markdown", 'a<b>:"c/\\d?*e');
    expect(p.filename).not.toMatch(/[<>:"/\\|?*]/);
  });

  it("默认只导出最终答复，执行过程需显式选择", () => {
    const withProcess: ChatMessage[] = [{
      id: "a-process",
      role: "assistant",
      complete: true,
      parts: [
        {
          kind: "text",
          text: "<think>I should inspect the project before answering.\n不应默认导出的内部思考</think>\n\n用户需要的最终结论",
          streamId: "generation-1",
        },
      ],
    }];

    const normal = buildSharePayload(withProcess, "markdown", "测试");
    expect(normal.content).toContain("用户需要的最终结论");
    expect(normal.content).not.toContain("I should inspect");
    expect(normal.content).not.toContain("内部思考");

    const detailed = buildSharePayload(withProcess, "markdown", "测试", {
      includeProcess: true,
    });
    expect(detailed.content).toContain("### 执行过程");
    expect(detailed.content).toContain("I should inspect the project before answering.");
    expect(detailed.content).toContain("内部思考");
    expect(detailed.content.indexOf("内部思考"))
      .toBeLessThan(detailed.content.indexOf("用户需要的最终结论"));
  });
});

describe("buildShareHtml", () => {
  it("转义 HTML 特殊字符", () => {
    const html = buildShareHtml(
      [{ id: "x", role: "user", complete: true, parts: [{ kind: "text", text: "<script>x</script>" }] }],
      "t",
    );
    expect(html).toContain("&lt;script&gt;");
    expect(html).not.toContain("<script>");
  });
  it("含标题与 viewport", () => {
    const html = buildShareHtml(messages, "标题");
    expect(html).toContain("<title>标题</title>");
    expect(html).toContain("viewport");
  });
});

describe("buildMailtoUrl", () => {
  it("subject + body 被 URL 编码", () => {
    const url = buildMailtoUrl("主题 测试", "正文 内容");
    expect(url.startsWith("mailto:?subject=")).toBe(true);
    expect(url).toContain(encodeURIComponent("主题 测试"));
    expect(url).toContain(encodeURIComponent("正文 内容"));
  });
  it("支持自定义 encode(注入)", () => {
    const enc = vi.fn((s: string) => `E(${s})`);
    const url = buildMailtoUrl("s", "b", { encode: enc });
    expect(enc).toHaveBeenCalled();
    expect(url).toContain("E(");
  });
});

describe("buildDownloadUrl", () => {
  it("用注入的 createObjectURL 构造 blob url", () => {
    const createObjectURL = vi.fn((b: Blob) => `blob:${b.size}`);
    const payload = buildSharePayload(messages, "markdown");
    const { url } = buildDownloadUrl(payload, { createObjectURL });
    expect(createObjectURL).toHaveBeenCalled();
    expect(url.startsWith("blob:")).toBe(true);
  });
  it("revoke 调用 URL.revokeObjectURL", () => {
    // jsdom 运行时未实现 revokeObjectURL(类型有、运行时无),先补一个可 spy 的实现。
    const revokeFn = vi.fn();
    const urlAny = URL as unknown as { revokeObjectURL?: (u: string) => void };
    if (!urlAny.revokeObjectURL) urlAny.revokeObjectURL = revokeFn;
    const revokeSpy = vi.spyOn(URL, "revokeObjectURL").mockImplementation(revokeFn);
    const payload = buildSharePayload(messages, "markdown");
    const { revoke } = buildDownloadUrl(payload, { createObjectURL: () => "blob:x" });
    revoke();
    expect(revokeSpy).toHaveBeenCalledWith("blob:x");
    revokeSpy.mockRestore();
  });
});

describe("复制与系统分享", () => {
  it("优先使用 Clipboard API", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    await expect(copyShareText("hello", { clipboard: { writeText } })).resolves.toBe(true);
    expect(writeText).toHaveBeenCalledWith("hello");
  });

  it("调用注入的系统分享表", async () => {
    const share = vi.fn().mockResolvedValue(undefined);
    const payload = buildSharePayload(messages, "text", "会话");
    await expect(systemShare(payload, "会话", share)).resolves.toBe(true);
    expect(share).toHaveBeenCalledWith(expect.objectContaining({ title: "会话", text: payload.content }));
  });
  it("Clipboard API 被拒绝时回退，结束后移除临时控件并恢复焦点", async () => {
    const button = document.createElement("button");
    document.body.append(button);
    button.focus();
    const execCommand = vi.fn().mockReturnValue(true);
    const original = document.execCommand;
    Object.defineProperty(document, "execCommand", { configurable: true, value: execCommand });
    try {
      await expect(copyShareText("fallback", { clipboard: { writeText: vi.fn().mockRejectedValue(new Error("denied")) }, document })).resolves.toBe(true);
      expect(execCommand).toHaveBeenCalledWith("copy");
      expect(document.querySelector("textarea")).toBeNull();
      expect(document.activeElement).toBe(button);
      execCommand.mockImplementation(() => { throw new Error("denied"); });
      await expect(copyShareText("fallback", { clipboard: null, document })).resolves.toBe(false);
      expect(document.querySelector("textarea")).toBeNull();
      expect(document.activeElement).toBe(button);
    } finally {
      Object.defineProperty(document, "execCommand", { configurable: true, value: original });
      button.remove();
    }
  });
});

describe("triggerDownload", () => {
  it("deps.document 显式为 null 时安全返回(不调 createObjectURL)", () => {
    const createObjectURL = vi.fn();
    const payload = buildSharePayload(messages, "markdown");
    // 用一个无 body 的假 document 触发 doc 分支:null 走「无 document」提前返回。
    expect(() =>
      triggerDownload(payload, { document: null as never, createObjectURL }),
    ).not.toThrow();
    expect(createObjectURL).not.toHaveBeenCalled();
  });

  it("创建 <a>、设置 download、click、移除", () => {
    const fakeAnchor = {
      href: "", download: "", rel: "", click: vi.fn(),
    } as unknown as HTMLAnchorElement;
    const fakeDoc = {
      createElement: vi.fn(() => fakeAnchor),
      body: { appendChild: vi.fn(), removeChild: vi.fn() },
    } as unknown as Document;
    const payload = buildSharePayload(messages, "markdown");
    triggerDownload(payload, {
      document: fakeDoc,
      createObjectURL: () => "blob:fake",
    });
    expect(fakeDoc.createElement).toHaveBeenCalledWith("a");
    expect((fakeAnchor as { download: string }).download).toBe(payload.filename);
    expect((fakeAnchor as { click: () => void }).click).toHaveBeenCalled();
    expect(fakeDoc.body.removeChild).toHaveBeenCalledWith(fakeAnchor);
  });
});
