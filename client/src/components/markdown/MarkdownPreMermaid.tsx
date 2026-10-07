import { isTauri, invoke } from "@tauri-apps/api/core";
import { memo, useEffect, useId, useState, type ReactNode } from "react";
import { CodeBlockActions } from "./CodeBlockActions";
import { MarkdownPreviewImage } from "./MarkdownPreviewImage";
import {
  createMermaidConfig,
  detectMermaidDiagramKind,
  normalizeStandaloneMermaidSvg,
} from "./mermaid-svg";
import type { MarkdownConfig } from "./types";

type Props = {
  content?: string;
  complete?: boolean;
  language?: string;
  theme?: "light" | "dark";
  children?: ReactNode;
  onDownloadMermaid?: MarkdownConfig["onDownloadMermaid"];
  onPreviewMermaid?: MarkdownConfig["onPreviewMermaid"];
  codeBlockActions?: MarkdownConfig["codeBlockActions"];
  requestId?: string;
  onCodeBlockAction?: MarkdownConfig["onCodeBlockAction"];
  onApplyCode?: MarkdownConfig["onApplyCode"];
  expandThreshold?: number;
};

/**
 * Mermaid fenced block. While streaming (`complete === false`) we only show
 * the source so partial graphs never throw. On complete we lazy-load mermaid.
 */
export const MarkdownPreMermaid = memo(function MarkdownPreMermaid({
  content = "",
  complete = true,
  language = "mermaid",
  theme = "light",
  children,
  onDownloadMermaid,
  onPreviewMermaid,
  codeBlockActions,
  requestId,
  onCodeBlockAction,
}: Props) {
  const reactId = useId().replace(/:/g, "");
  const [mode, setMode] = useState<"diagram" | "code">("diagram");
  const [previewOpen, setPreviewOpen] = useState(false);
  const [svg, setSvg] = useState<string | null>(null);
  const [svgUrl, setSvgUrl] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [rendering, setRendering] = useState(false);
  const [downloadStatus, setDownloadStatus] = useState<string | null>(null);
  const [downloading, setDownloading] = useState(false);

  const code = content || "";
  const diagramKind = detectMermaidDiagramKind(code);

  useEffect(() => {
    if (!complete || mode !== "diagram" || !code.trim()) {
      setSvg(null);
      setError(null);
      setRendering(false);
      return;
    }

    let cancelled = false;
    setRendering(true);
    setError(null);
    setDownloadStatus(null);

    (async () => {
      try {
        const mermaid = (await import("mermaid")).default;
        const secure = new Set(mermaid.mermaidAPI.getConfig().secure ?? []);
        mermaid.initialize(createMermaidConfig(diagramKind, theme, [...secure]));
        const id = `md-mermaid-${reactId}-${Date.now()}`;
        const { svg: rendered } = await mermaid.render(id, code);
        const standaloneSvg = normalizeStandaloneMermaidSvg(rendered);
        if (!cancelled) {
          setSvg(standaloneSvg);
          setError(null);
        }
      } catch (err) {
        if (!cancelled) {
          setSvg(null);
          setError(err instanceof Error ? err.message : String(err));
        }
      } finally {
        if (!cancelled) setRendering(false);
      }
    })();

    return () => {
      cancelled = true;
    };
  }, [complete, mode, code, theme, reactId]);

  // Render generated SVG through the browser's image decoder instead of
  // injecting it into the application DOM. Even though Mermaid runs in
  // `strict` mode, model-authored diagrams are untrusted input and future
  // Mermaid regressions must not turn SVG markup into executable WebView DOM.
  useEffect(() => {
    if (!svg || typeof URL.createObjectURL !== "function") {
      setSvgUrl(null);
      return;
    }
    const nextUrl = URL.createObjectURL(
      new Blob([svg], { type: "image/svg+xml;charset=utf-8" }),
    );
    setSvgUrl(nextUrl);
    return () => URL.revokeObjectURL(nextUrl);
  }, [svg]);

  const handleDownload = async () => {
    if (!svg || downloading) return;
    setDownloading(true);
    setDownloadStatus(null);
    try {
      if (onDownloadMermaid) {
        await onDownloadMermaid(svg, code);
        setDownloadStatus("已提交下载");
        return;
      }
      if (isTauri()) {
        const savedPath = await invoke<string | null>("export_text_file", {
          suggestedName: "diagram.svg",
          extension: "svg",
          content: svg,
        });
        setDownloadStatus(savedPath ? `已保存：${savedPath}` : "已取消保存");
        return;
      }
      const url = URL.createObjectURL(new Blob([svg], { type: "image/svg+xml;charset=utf-8" }));
      try {
        const link = document.createElement("a");
        link.href = url;
        link.download = "diagram.svg";
        document.body.appendChild(link);
        try {
          link.click();
        } finally {
          link.remove();
        }
        setDownloadStatus("已发起下载");
      } finally {
        // The browser may only start reading the Blob after the click returns.
        window.setTimeout(() => URL.revokeObjectURL(url), 60_000);
      }
    } catch (downloadError) {
      setDownloadStatus(`下载失败：${String(downloadError).replace(/^Error:\s*/, "")}`);
    } finally {
      setDownloading(false);
    }
  };

  const showSource = !complete || mode === "code" || !!error;
  const viewBox = svg?.match(/\bviewBox\s*=\s*["']([^"']+)["']/i)?.[1]
    .trim()
    .split(/[\s,]+/)
    .map(Number);
  const intrinsicSize = viewBox?.length === 4
    && Number.isFinite(viewBox[2]) && viewBox[2] > 0
    && Number.isFinite(viewBox[3]) && viewBox[3] > 0
    ? { width: viewBox[2], height: viewBox[3] }
    : undefined;
  const openPreview = () => {
    if (!svg || !svgUrl) return;
    if (onPreviewMermaid) onPreviewMermaid(svg, code);
    else setPreviewOpen(true);
  };

  return (
    <div className="md-code-wrapper md-mermaid-wrapper">
      <div className="md-code-container">
        <div className="md-code-header">
          <strong className="md-code-lang">mermaid</strong>
          <div className="md-mermaid-toolbar">
            {mode === "diagram" && svgUrl ? (
              <button
                type="button"
                className="md-code-action"
                onClick={(event) => {
                  event.currentTarget.focus();
                  openPreview();
                }}
                title="放大预览图表"
              >
                <span className="md-code-action-label">放大</span>
              </button>
            ) : null}
            {complete ? (
              <button
                type="button"
                className="md-code-action"
                onClick={() => setMode((m) => (m === "diagram" ? "code" : "diagram"))}
                title={mode === "diagram" ? "查看源码" : "查看图表"}
              >
                <span className="md-code-action-label">
                  {mode === "diagram" ? "源码" : "图表"}
                </span>
              </button>
            ) : (
              <span className="md-mermaid-pending">生成中…</span>
            )}
            {svg ? (
              <button
                type="button"
                className="md-code-action"
                onClick={() => void handleDownload()}
                disabled={downloading}
                title="下载 SVG"
              >
                <span className="md-code-action-label">{downloading ? "保存中…" : "下载"}</span>
              </button>
            ) : null}
            <CodeBlockActions
              code={code}
              language={language}
              actions={codeBlockActions}
              requestId={requestId}
              onAction={onCodeBlockAction}
              copyIconOnly
            />
          </div>
        </div>
        {downloadStatus ? <div className="md-mermaid-download-status" role="status" title={downloadStatus}>{downloadStatus}</div> : null}

        {showSource ? (
          <pre className="md-code-pre md-mermaid-source">
            {children ?? <code className="language-mermaid">{code}</code>}
            {error ? <div className="md-mermaid-error">Mermaid 渲染失败: {error}</div> : null}
          </pre>
        ) : rendering ? (
          <div className="md-mermaid-loading">正在渲染图表…</div>
        ) : svg && svgUrl ? (
          <div className="md-mermaid-diagram">
            <MarkdownPreviewImage
              src={svgUrl}
              alt="Mermaid 图表"
              previewTitle="图表预览"
              previewLabel="放大预览图表"
              intrinsicSize={intrinsicSize}
              onPreview={onPreviewMermaid ? () => onPreviewMermaid(svg, code) : undefined}
              previewOpen={previewOpen}
              onPreviewOpenChange={setPreviewOpen}
            />
          </div>
        ) : (
          <pre className="md-code-pre">
            <code className="language-mermaid">{code}</code>
          </pre>
        )}
      </div>
    </div>
  );
});
