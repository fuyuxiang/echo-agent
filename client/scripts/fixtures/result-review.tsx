// Isolated review data rendered through production result components.
// No model, native filesystem, or external service is called here.
import { useState } from "react";
import { FilePreview } from "../../src/components/FilePreview";
import { ArtifactTabsBar } from "../../src/components/workspace-panel/ArtifactTabsBar";
import type { UnifiedTab } from "../../src/lib/use-unified-tabs";

const longWord = "release_2026_10_customer_delivery_with_a_very_long_directory_and_filename_".repeat(4);
const wideTable = [
  "| 模块 | 正常状态 | 空状态 | 加载状态 | 错误状态 | 复制 | 打开 | 长内容 | 小窗口 | 深色主题 | 验收结果 |",
  "| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |",
  ...Array.from({ length: 24 }, (_, i) => `| 第 ${i + 1} 项 | 可用 | 已说明 | 有反馈 | 可恢复 | 可复制 | 可访问 | 300 行文档 | 768 × 720 | 清晰 | 已完成 |`),
].join("\n");
const markdown = `# 界面评审结果\n\n主要交付包含统一弹窗、可阅读的结果预览，以及可靠的操作反馈。\n\n## 重要结论\n\n- 标题、内容与操作按用途组织。\n- 长路径不会撑开页面：\`${longWord}.md\`。\n- [相关文档](https://example.com/docs) 与生成文件保持清晰入口。\n\n## 覆盖矩阵\n\n${wideTable}\n\n## 示例代码\n\n\`\`\`typescript\nconst artifactPath = "${longWord}";\nexport async function openArtifact() {\n  await openPath(artifactPath);\n}\n\`\`\`\n\n## 详细说明\n\n${"这是一段用于验证文档持续增长、排版层级与滚动行为的隔离测试内容。完成任务后，用户可以找到结论、检查细节并继续使用生成文件。\n\n".repeat(35)}`;
const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="960" height="360"><rect width="960" height="360" fill="#f5f7fb"/><text x="40" y="60" font-family="sans-serif" font-size="28" fill="#28344a">Result preview • isolated fixture</text><rect x="60" y="115" width="190" height="150" rx="18" fill="#b5d4fe"/><rect x="305" y="115" width="320" height="150" rx="18" fill="#88cdb6"/><rect x="680" y="115" width="190" height="150" rx="18" fill="#f3cc83"/><text x="155" y="202" text-anchor="middle" font-family="sans-serif" font-size="24" fill="#26364c">Review</text><text x="465" y="202" text-anchor="middle" font-family="sans-serif" font-size="24" fill="#26364c">Improve</text><text x="775" y="202" text-anchor="middle" font-family="sans-serif" font-size="24" fill="#26364c">Verify</text></svg>`;
const resultTabs: UnifiedTab[] = [
  ["markdown", "UI 评审完整结果.md"],
  ["short", "交付摘要.md"],
  ["code", "very_long_result_implementation_and_delivery_notes.ts"],
  ["sheet", "验证覆盖矩阵.xlsx"],
  ["image", "结果流程图.svg"],
  ["diagram", "流程图说明.md"],
  ["empty", "空文档.docx"],
  ["error", "失效图片.png"],
].map(([id, label]) => ({ id, kind: "file", label, subtitle: `/review/generated/${label}`, viewWhenActive: "fileTree" }));

function contentFor(id: string) {
  if (id === "markdown") return markdown;
  if (id === "short") return "# 交付完成\n\n已修复复制反馈、预览状态与标签键盘操作。\n\n- 完整结果见评审报告。\n- 生成文件可以复制、预览并继续使用。";
  if (id === "diagram") return "# 结果交付流程\n\n图表应遵循当前应用主题，放大后可继续检查细节。\n\n```mermaid\nflowchart LR\n  A[任务完成] --> B[查看最终结论]\n  B --> C[预览生成文件]\n  C --> D[复制或继续使用]\n```";
  if (id === "code") return Array.from({ length: 160 }, (_, i) => `const line${i + 1} = "${i === 0 ? longWord : `结果预览第 ${i + 1} 行`}";`).join("\n");
  if (id === "image") return `data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg)}`;
  if (id === "error") return "data:image/png;base64,invalid";
  return "isolated-office-data";
}

export default function ResultReviewFixture() {
  const initial = new URLSearchParams(location.search).get("resultVariant") ?? "markdown";
  const [active, setActive] = useState(initial);
  const [tabs, setTabs] = useState(resultTabs);
  const current = tabs.find((tab) => tab.id === active) ?? tabs[0];
  const [copyFailure, setCopyFailure] = useState(false);
  return <div className="result-review-fixture" style={{ display: "flex", flexDirection: "column", height: "100%", minWidth: 0, background: "var(--echo-bg-primary)" }}>
    <div style={{ display: "flex", flexWrap: "wrap", alignItems: "center", justifyContent: "space-between", gap: 8, padding: "12px 16px", borderBottom: "1px solid var(--echo-border-default)" }}>
      <strong>任务成果与文件预览</strong><label style={{ display: "inline-flex", gap: 6, alignItems: "center", color: "var(--echo-text-medium)" }}><input type="checkbox" checked={copyFailure} onChange={(event) => setCopyFailure(event.target.checked)} />模拟复制失败</label>
    </div>
    <div className="tool-side-panel__header"><div className="tool-side-panel__tabs"><ArtifactTabsBar tabs={tabs} activeTabId={current?.id} onSelect={setActive} onClose={(id) => setTabs((items) => items.filter((tab) => tab.id !== id))} onReorder={(ids) => setTabs((items) => ids.map((id) => items.find((tab) => tab.id === id)!))} /></div></div>
    <div className="tool-side-panel__body" style={{ padding: 16 }}>
      {current ? <div className="artifact-file-preview"><FilePreview filename={current.label} content={contentFor(current.id)} onCopyText={async () => !copyFailure}
        docExtractor={current.id === "empty" ? () => ({ readText: () => "<w:document />", listEntries: () => ["word/document.xml"] })
          : current.id === "sheet" ? () => ({
            readText: (path) => path.includes("sharedStrings") ? "<sst><si><t>验证项目</t></si></sst>" : `<worksheet>${Array.from({ length: 40 }, (_, row) => `<row>${Array.from({ length: 10 }, (_, col) => `<c><v>${row * 10 + col}</v></c>`).join("")}</row>`).join("")}</worksheet>`,
            listEntries: () => ["xl/sharedStrings.xml", "xl/worksheets/sheet1.xml"],
          }) : undefined} /></div> : <div className="tool-side-panel__empty">所有预览标签已关闭</div>}
    </div>
  </div>;
}
