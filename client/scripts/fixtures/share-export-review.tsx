import React, { useState } from "react";
import { createRoot } from "react-dom/client";
import { ShareMenu } from "../../src/components/ShareMenu";
import { Markdown } from "../../src/components/Markdown";
import { buildSessionMarkdown } from "../../src/lib/export-markdown";
import type { ChatMessage } from "../../src/stores/session-store";
import "../../src/styles/index.css";

const query = new URLSearchParams(location.search);
const theme = query.get("theme") === "dark" ? "dark" : "light";
document.documentElement.dataset.theme = theme;
const title = "会话导出 · 阅读效果验收";
const markdown = [
  "# 产品方案与验收结果", "", "**重点结论**：HTML 应直接展示排版后的正文。使用 `exportHtml()` 保存文件。", "",
  "## 功能对照", "", "| 能力 | 客户端 | HTML 导出 |", "| --- | --- | --- |",
  "| 标题与格式 | 支持 | 保留 |", "| 表格和代码 | 支持 | 保留 |", "| 公式与图表 | 支持 | 离线阅读 |", "",
  "- [x] 直接阅读，无需安装应用", "- [ ] 按需勾选执行过程", "", "> 这是一段引用，正文中的链接仍可使用：[参考文档](https://example.test/docs)。", "",
  "```ts", 'const message = "已渲染的代码";', "console.log(message);", "```", "",
  "## 数学公式", "", "行内公式 $E=mc^2$，以及独立公式：", "", "$$", "\\sum_{i=1}^{n} i = \\frac{n(n+1)}{2}", "$$", "",
  "```latex", "\\frac{1}{2} + \\frac{1}{2} = 1", "```", "",
  "## 流程与思维导图", "", "```mermaid", "flowchart LR", " A[生成 Markdown] --> B[渲染正文] --> C[保存 HTML]", "```", "",
  "```mermaid", "mindmap", "  root((离线阅读))", "    表格", "    代码", "    公式与图表", "```", "",
  "## 长内容的阅读", "", "| " + Array.from({ length: 8 }, (_, i) => `较长的列标题${i + 1}`).join(" | ") + " |",
  "| " + Array(8).fill("---").join(" | ") + " |", "| " + Array(8).fill("列中的长内容用于验证局部滚动").join(" | ") + " |", "",
  "```text", "a_very_long_code_line_".repeat(20), "```", "",
  "![内嵌图片](data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/l9sAAAAASUVORK5CYII=)", "",
  "![外部图片](https://example.test/tracker.png)", "", "<script>window.pwned = true</script>", "", "**正文结束**",
].join("\n");
const messages: ChatMessage[] = [
  { id: "question", role: "user", complete: true, parts: [{ kind: "text", text: "请给出可直接阅读的导出方案。" }] },
  { id: "answer", role: "assistant", complete: true, parts: [{ kind: "text", text: `<think>默认隐藏的内部过程</think>\n\n${markdown}` }] },
];
const review = { saves: [] as Record<string, string>[], notices: [] as string[], cancel: query.has("cancel") };
Object.assign(window, {
  exportReview: review,
  __TAURI_INTERNALS__: {
    invoke: async (command: string, args: Record<string, string>) => {
      if (command !== "export_text_file") throw new Error(`Unexpected command: ${command}`);
      review.saves.push(args);
      return review.cancel ? null : `/review/${args.suggestedName}`;
    },
  },
});

function Review() {
  const [open, setOpen] = useState(false);
  return <main style={{ height: "100%", overflow: "auto", padding: "24px 14px", color: "var(--echo-text-strong)", background: "var(--echo-bg-primary)" }}>
    <div style={{ maxWidth: 832, margin: "auto", fontSize: 16 }}>
      <button className="main-topbar__btn" onClick={() => setOpen(true)}>导出与本地分享</button>
      <ShareMenu controlledOpen={open} onClose={() => setOpen(false)} saveToFile={!query.has("browser")} messages={messages} title={title} onDone={notice => review.notices.push(notice)} />
      <div data-testid="client-preview"><Markdown theme={theme}>{buildSessionMarkdown(messages, title)}</Markdown></div>
    </div>
  </main>;
}
createRoot(document.getElementById("root")!).render(<Review />);
