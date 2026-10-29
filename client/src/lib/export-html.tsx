import { renderToStaticMarkup } from "react-dom/server";
import { Markdown } from "@/components/Markdown";
import { renderStandaloneMermaid } from "@/components/markdown/mermaid-render";
import { isFullLatexDocument } from "@/components/markdown/MarkdownPre";
import contentStyles from "@/styles/markdown-content.css?raw";

const readingStyles = `
:root{color-scheme:light;--echo-bg-primary:#fff;--echo-bg-secondary:#f5f5f5;--echo-bg-tertiary:#f7f7f7;--echo-bg-hover:#eee;--echo-text-strong:#1d1d1f;--echo-text-medium:#606060;--echo-text-secondary:#606060;--echo-text-weak:#737373;--echo-border-default:#dedede;--echo-text-link:#1470b4;--echo-font-code-family:ui-monospace,Menlo,Consolas,monospace}
[data-theme="dark"]{color-scheme:dark;--echo-bg-primary:#212121;--echo-bg-secondary:#181818;--echo-bg-tertiary:#2a2a2a;--echo-bg-hover:#333;--echo-text-strong:#ececec;--echo-text-medium:#bbb;--echo-text-secondary:#bbb;--echo-text-weak:#aaa;--echo-border-default:#444;--echo-text-link:#7aa2ff}
*{box-sizing:border-box}html{background:var(--echo-bg-primary)}body{font:16px/1.65 -apple-system,BlinkMacSystemFont,"Segoe UI","PingFang SC","Microsoft YaHei",sans-serif;color:var(--echo-text-strong);max-width:880px;margin:32px auto;padding:0 24px 40px}main{min-width:0}pre{white-space:pre}code{font-family:var(--echo-font-code-family)}.md-export-diagram{margin:0 0 16px;padding:16px;border:1px solid var(--echo-border-default);border-radius:12px;overflow:auto}.md-export-diagram img{display:block;max-width:100%;height:auto;margin:auto}.md-remote-image-blocked{display:flex;flex-wrap:wrap;gap:8px;padding:8px;border:1px solid var(--echo-border-default);border-radius:8px}.md-export-fallback{margin:0 0 8px;color:var(--echo-text-medium)}
@media(max-width:600px){body{margin:16px auto;padding:0 14px 24px}.md-code-pre{padding:10px!important}}
@media print{html[data-theme],html{color-scheme:light;--echo-bg-primary:#fff;--echo-bg-secondary:#f5f5f5;--echo-bg-tertiary:#f7f7f7;--echo-text-strong:#111;--echo-text-medium:#444;--echo-border-default:#ccc;--md-blockquote-fg:#444;--md-hljs-comment:#555;--md-hljs-keyword:#0000aa;--md-hljs-string:#8a2020;--md-hljs-number:#066650;--md-link:#135d93}body{max-width:none;margin:0;padding:0}pre{white-space:pre-wrap;overflow-wrap:anywhere}h1,h2,h3,h4{break-after:avoid}thead{display:table-header-group}.md-table-wrapper{overflow:visible}.markdown-body table{width:100%;min-width:0}.markdown-body th,.markdown-body td{overflow-wrap:anywhere}.md-export-diagram{break-inside:avoid}}
`;

function escapeHtml(value: string): string {
  return value.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
}

/** Shares the client parser and styles, but saves only portable reading content. */
export async function renderShareHtml(markdown: string, title: string, theme: "light" | "dark"): Promise<string> {
  let content = renderToStaticMarkup(<Markdown config={{ staticRendering: true }} theme={theme}>{markdown}</Markdown>);
  const doc = new DOMParser().parseFromString(content, "text/html");
  for (const block of doc.querySelectorAll<HTMLElement>('[data-export-language="latex"]')) {
    const code = block.querySelector("code")?.textContent ?? "";
    if (isFullLatexDocument(code)) continue;
    const katex = (await import("katex")).default;
    const formula = doc.createElement("div");
    formula.className = "md-latex-display";
    formula.innerHTML = katex.renderToString(code, { displayMode: true, throwOnError: false, strict: false, trust: false });
    block.replaceWith(formula);
  }
  for (const block of doc.querySelectorAll<HTMLElement>('[data-export-language="mermaid"]')) {
    const source = block.querySelector("code")?.textContent ?? "";
    try {
      const svg = await renderStandaloneMermaid(`export-${crypto.randomUUID()}`, source, theme);
      const figure = doc.createElement("figure");
      figure.className = "md-export-diagram";
      const image = doc.createElement("img");
      image.alt = "Mermaid 图表";
      image.src = `data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg)}`;
      figure.append(image);
      block.replaceWith(figure);
    } catch {
      const note = doc.createElement("p");
      note.className = "md-export-fallback";
      note.textContent = "此图表未能渲染，已保留原始代码。";
      block.prepend(note);
    }
  }
  content = doc.body.innerHTML;
  const mathStyles = doc.querySelector(".katex")
    ? (await import("./html-export-fonts")).htmlExportMathStyles : "";
  return `<!DOCTYPE html>
<html lang="zh-CN" data-theme="${theme}">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="referrer" content="no-referrer">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src data:; style-src 'unsafe-inline'; font-src data:; base-uri 'none'; form-action 'none'">
<title>${escapeHtml(title)}</title>
<style>${readingStyles}\n${contentStyles}\n${mathStyles}</style>
</head><body><main aria-label="导出的会话">${content}</main></body></html>`;
}
