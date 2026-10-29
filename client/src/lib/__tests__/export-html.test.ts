import { describe, expect, it, vi } from "vitest";
import { buildShareHtml, buildSharePayload } from "../share";
import type { ChatMessage } from "@/stores/session-store";

vi.mock("@/components/markdown/mermaid-render", () => ({
  renderStandaloneMermaid: vi.fn(async (_id: string, code: string) => {
    if (code.includes("broken")) throw new Error("invalid graph");
    return '<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50"><text>图表</text></svg>';
  }),
}));

const answer = (text: string): ChatMessage[] => [{ id: "answer", role: "assistant", complete: true, parts: [{ kind: "text", text }] }];
const parse = (html: string) => new DOMParser().parseFromString(html, "text/html");

describe("rendered HTML export", () => {
  it("renders readable headings, formatting, tables, checkboxes and highlighted code", async () => {
    const payload = await buildSharePayload(answer("# 方案\n\n**重点**与 `inline`\n\n| 列 | 值 |\n| --- | --- |\n| 状态 | 已完成 |\n\n- [x] 核验\n- [ ] 发布\n\n> 引用\n\n```ts\nconst total = 3;\n```"), "html", "导出测试");
    const doc = parse(payload.content);
    expect(doc.querySelector("main h1")?.textContent).toBe("导出测试");
    expect(doc.querySelector("strong")?.textContent).toBe("重点");
    expect(doc.querySelector("table td")?.textContent).toBe("状态");
    expect(doc.querySelectorAll('input[type="checkbox"]')).toHaveLength(2);
    expect(doc.querySelector('input[checked]')).not.toBeNull();
    expect(doc.querySelector(".hljs-keyword")?.textContent).toBe("const");
    expect(doc.querySelector(".md-inline-code")?.textContent).toBe("inline");
    expect(doc.querySelectorAll("button,script,iframe")).toHaveLength(0);
    expect(doc.querySelector("body > pre")).toBeNull();
    expect(payload.bytes).toBe(new TextEncoder().encode(payload.content).length);
  });

  it("embeds rendered math and its fonts for offline reading", async () => {
    const doc = parse(await buildShareHtml(answer("公式 $E=mc^2$\n\n```latex\n\\frac{1}{2}\n```"), "数学"));
    expect(doc.querySelectorAll(".katex")).toHaveLength(2);
    expect(doc.querySelector("style")?.textContent).toContain("data:font/woff2;base64,");
    expect(doc.querySelector("style")?.textContent).not.toMatch(/url\(["']?fonts\//);
  });

  it("embeds diagram images, retains invalid diagrams, and renders the rest", async () => {
    const doc = parse(await buildShareHtml(answer("```mermaid\ngraph TD; A-->B\n```\n\n```mermaid\nbroken\n```\n\n**仍可阅读**")));
    expect(doc.querySelector(".md-export-diagram img")?.getAttribute("src")).toMatch(/^data:image\/svg\+xml/);
    expect(doc.querySelector(".md-export-fallback")?.textContent).toContain("保留原始代码");
    expect(doc.querySelector("strong")?.textContent).toBe("仍可阅读");
  });

  it("keeps untrusted content inert without fetching remote images", async () => {
    const doc = parse(await buildShareHtml(answer('<script>window.pwned=true</script>\n\n[攻击](javascript:alert(1))\n\n![远程图](https://example.test/tracker.png)\n\n[文档](https://example.test/docs)'), '</title><script>alert(1)</script>'));
    expect(doc.querySelectorAll("script,img,iframe,object")).toHaveLength(0);
    expect(doc.querySelector('a[href^="javascript:"]')).toBeNull();
    expect(doc.querySelectorAll('a[href^="https://example.test/"]')).toHaveLength(2);
    expect(doc.querySelector('meta[http-equiv="Content-Security-Policy"]')?.getAttribute("content")).toContain("default-src 'none'");
    expect(doc.querySelector("title")?.textContent).toContain("</title><script>");
  });

  it("exports only final answers by default and follows the chosen theme", async () => {
    const messages = answer("<think>不默认分享的过程</think>\n\n最终内容");
    const normal = parse(await buildShareHtml(messages, "会话", { theme: "dark" }));
    expect(normal.documentElement.dataset.theme).toBe("dark");
    expect(normal.body.textContent).not.toContain("不默认分享的过程");
    expect(normal.body.textContent).toContain("最终内容");
    const detailed = parse(await buildShareHtml(messages, "会话", { includeProcess: true }));
    expect(detailed.body.textContent).toContain("不默认分享的过程");
  });
});
