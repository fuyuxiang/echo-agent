/**
 * 本地文件预览(Context Viewer 可移植部分)—— 对齐 EchoAgent
 * `context-viewer-components/media-preview`。
 *
 * 轻量、无重型依赖:本地渲染 Markdown/文本/媒体，提取 Office 文本，
 * PDF 交给浏览器内嵌预览，未知类型显示可读占位。
 *
 * 通过 `filename` + `content`(文本或 data: URL)渲染;`onCopyText` 提供复制回调。
 */
import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { FileText, Image as ImageIcon } from "lucide-react";
import { Markdown } from "./markdown/index";
import { MarkdownPreviewImage } from "./markdown/MarkdownPreviewImage";
import { copyShareText } from "@/lib/share";
import {
  detectPreviewKind,
  previewKindLabel,
  codeLanguage,
} from "@/lib/file-kind";
import type { ZipReader } from "@/lib/doc-preview";
import {
  extractDocxFromZip,
  extractPptxFromZip,
  extractSheetFromZip,
} from "@/lib/doc-preview";
import { readZipFromBase64, makeDocZipReader } from "@/lib/zip-reader";

/**
 * 默认文档解压器:把 content(data: URL 或 base64)用内置 zip-reader 解压,
 * 构造 doc-preview 的 ZipReader。解压失败返回 null(降级占位)。
 */
function defaultDocExtractor(content: string): ZipReader | null {
  try {
    const files = readZipFromBase64(content);
    if (Object.keys(files).length === 0) return null;
    return makeDocZipReader(files);
  } catch {
    return null;
  }
}

export type FilePreviewCopyHandler = (text: string) => void | boolean | Promise<void | boolean>;

interface FilePreviewProps {
  /** 文件名(用于类型识别)。 */
  filename: string;
  /** 文本内容(markdown/code/text);image 时为 data: URL 或远程 URL。 */
  content: string;
  /** 默认使用剪贴板；注入回调可返回 false 或拒绝 Promise 表示失败。 */
  onCopyText?: FilePreviewCopyHandler;
  /**
   * 文档预览解压器(对齐 EchoAgent docx/pptx/sheet 预览):对 OOXML 文件,
   * 调用方提供 ZipReader(任意 zip 实现/后端解压),FilePreview 用纯函数提取文本。
   * 未提供时使用内置解压器，读取失败时显示占位。
   */
  docExtractor?: (filename: string) => ZipReader | null;
}

export function FilePreview({ filename, content, onCopyText, docExtractor }: FilePreviewProps) {
  const kind = detectPreviewKind(filename);

  if (kind === "image") {
    return (
      <div className="file-preview file-preview--image">
        <div className="file-preview__head">
          <span className="file-preview__name">{filename}</span>
          <span className="file-preview__kind">{previewKindLabel(kind)}</span>
        </div>
        <MediaPreview filename={filename} content={content} kind="image" />
      </div>
    );
  }

  if (kind === "markdown") {
    return <MarkdownFilePreview filename={filename} content={content} onCopyText={onCopyText} />;
  }

  if (kind === "code" || kind === "text") {
    return (
      <CodePreview
        filename={filename}
        content={content}
        kind={kind}
        onCopyText={onCopyText}
      />
    );
  }

  // 音频/视频:零依赖 HTML5 原生 <audio>/<video> 预览(对齐 EchoAgent media-preview)。
  if (kind === "audio") {
    return (
      <div className="file-preview file-preview--audio">
        <div className="file-preview__head">
          <span className="file-preview__name">{filename}</span>
          <span className="file-preview__kind">{previewKindLabel(kind)}</span>
        </div>
        <div className="file-preview__media">
          {/* eslint-disable-next-line jsx-a11y/media-has-caption */}
          <MediaPreview filename={filename} content={content} kind="audio" />
        </div>
      </div>
    );
  }

  if (kind === "video") {
    return (
      <div className="file-preview file-preview--video">
        <div className="file-preview__head">
          <span className="file-preview__name">{filename}</span>
          <span className="file-preview__kind">{previewKindLabel(kind)}</span>
        </div>
        <div className="file-preview__media">
          {/* eslint-disable-next-line jsx-a11y/media-has-caption */}
          <MediaPreview filename={filename} content={content} kind="video" />
        </div>
      </div>
    );
  }

  // docx/pptx/sheet:OOXML 文本提取(对齐 EchoAgent media-preview)。
  // 默认用内置 zip-reader(纯 JS DEFLATE)从 content(data: URL/base64)解压;
  // 调用方也可注入自定义 docExtractor 覆盖。
  if (kind === "docx" || kind === "pptx" || kind === "sheet") {
    const zip = docExtractor?.(filename) ?? defaultDocExtractor(content);
    const extracted =
      zip && kind === "docx" ? extractDocxFromZip(zip)
      : zip && kind === "pptx" ? extractPptxFromZip(zip)
      : zip && kind === "sheet" ? extractSheetFromZip(zip)
      : null;
    return (
      <DocPreview
        filename={filename}
        kind={kind}
        text={extracted?.text ?? null}
        sheets={kind === "sheet" && extracted ? (extracted as { sheets: Array<{ name: string; rows: string[][] }> }).sheets : undefined}
        onCopyText={onCopyText}
      />
    );
  }

  // PDF:浏览器原生 <iframe> 内嵌预览(零依赖,大多数 WebView2/WKWebView 自带 PDF 渲染)。
  if (kind === "pdf") {
    return (
      <div className="file-preview file-preview--pdf">
        <div className="file-preview__head">
          <span className="file-preview__name">{filename}</span>
          <span className="file-preview__kind">{previewKindLabel(kind)}</span>
        </div>
        <iframe
          className="file-preview__pdf"
          src={content}
          title={filename}
        />
      </div>
    );
  }

  // 其余二进制(未知):占位。
  return (
    <div className="file-preview file-preview--binary">
      <div className="file-preview__head">
        <span className="file-preview__name">{filename}</span>
        <span className="file-preview__kind">{previewKindLabel(kind)}</span>
      </div>
      <div className="file-preview__placeholder">
        <FileText size={28} aria-hidden="true" />
        <span className="file-preview__placeholder-text">
          {previewKindLabel(kind)} 暂不支持内嵌预览,请用本地应用打开。
        </span>
      </div>
    </div>
  );
}

function MediaPreview({ filename, content, kind }: { filename: string; content: string; kind: "image" | "audio" | "video" }) {
  // A cached/invalid data URL can fail before mount effects run. Keep the
  // failure attached to its resource instead of clearing it in a mount effect.
  const [failedResource, setFailedResource] = useState<{ filename: string; content: string; kind: string } | null>(null);
  const failed = failedResource?.filename === filename && failedResource.content === content && failedResource.kind === kind;
  const onError = () => setFailedResource({ filename, content, kind });
  if (failed || !content) {
    return (
      <div className="file-preview__placeholder" role="alert">
        {kind === "image" ? <ImageIcon size={28} aria-hidden="true" /> : <FileText size={28} aria-hidden="true" />}
        <span className="file-preview__placeholder-text">{!content ? "文件内容为空，无法预览。" : "无法加载预览，文件可能已损坏或格式不受支持。"}</span>
        <span className="file-preview__detail">可通过“用系统应用打开”查看原件。</span>
      </div>
    );
  }
  if (kind === "image") {
    return <MarkdownPreviewImage className="file-preview__img" src={content} alt={filename} onError={onError} />;
  }
  if (kind === "audio") {
    return <audio className="file-preview__audio" src={content} controls onError={onError}>您的浏览器不支持音频预览。</audio>;
  }
  return <video className="file-preview__video" src={content} controls onError={onError}>您的浏览器不支持视频预览。</video>;
}

function usePreviewCopy(text: string, filename: string, onCopyText?: FilePreviewCopyHandler) {
  const [status, setStatus] = useState<"idle" | "copying" | "copied" | "failed">("idle");
  const attempt = useRef(0);
  const pending = useRef(false);
  useEffect(() => {
    attempt.current += 1;
    pending.current = false;
    setStatus("idle");
    return () => { attempt.current += 1; };
  }, [text, filename]);
  useEffect(() => {
    if (status !== "copied" && status !== "failed") return;
    const timer = window.setTimeout(() => setStatus("idle"), 2500);
    return () => window.clearTimeout(timer);
  }, [status]);
  const copy = async () => {
    if (!text || pending.current) return;
    pending.current = true;
    const current = ++attempt.current;
    setStatus("copying");
    try {
      let copied: void | boolean;
      if (onCopyText) copied = await onCopyText(text);
      else {
        try { copied = await copyShareText(text); }
        catch { copied = await copyShareText(text, { clipboard: null }); }
      }
      if (copied === false) throw new Error("clipboard unavailable");
      if (attempt.current === current) setStatus("copied");
    } catch {
      if (attempt.current === current) setStatus("failed");
    } finally {
      if (attempt.current === current) pending.current = false;
    }
  };
  return { status, copy };
}

function CopyFeedback({ status }: { status: "idle" | "copying" | "copied" | "failed" }) {
  return status === "failed" ? <p className="file-preview__feedback" role="alert">复制失败，请重试或检查剪贴板权限。</p> : null;
}

function subscribePreviewTheme(notify: () => void) {
  const observer = new MutationObserver(notify);
  observer.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
  return () => observer.disconnect();
}

function getPreviewTheme(): "light" | "dark" {
  return document.documentElement.dataset.theme === "dark" ? "dark" : "light";
}

function MarkdownFilePreview({ filename, content, onCopyText }: { filename: string; content: string; onCopyText?: FilePreviewCopyHandler }) {
  const { status, copy } = usePreviewCopy(content, filename, onCopyText);
  // File previews also live outside chat. Use the applied app theme so diagrams
  // stay consistent when a settings change does not rerender their parent.
  const theme = useSyncExternalStore<"light" | "dark">(subscribePreviewTheme, getPreviewTheme, () => "light");
  return <div className="file-preview file-preview--markdown">
    <div className="file-preview__head">
      <span className="file-preview__name" title={filename}>{filename}</span>
      <span className="file-preview__kind">Markdown</span>
      <button type="button" className="file-preview__copy" onClick={() => void copy()} aria-label="复制 Markdown 内容" disabled={status === "copying" || !content} aria-busy={status === "copying"}>
        {status === "copying" ? "复制中…" : status === "copied" ? "已复制" : "复制"}
      </button>
    </div>
    <CopyFeedback status={status} />
    <div className="file-preview__body">
      {content.trim() ? <Markdown complete theme={theme}>{content}</Markdown> : <div className="file-preview__placeholder"><FileText size={28} aria-hidden="true" /><span>文件内容为空</span></div>}
    </div>
  </div>;
}

function CodePreview({
  filename,
  content,
  kind,
  onCopyText,
}: {
  filename: string;
  content: string;
  kind: "code" | "text";
  onCopyText?: FilePreviewCopyHandler;
}) {
  const { status, copy } = usePreviewCopy(content, filename, onCopyText);
  const lang = kind === "code" ? codeLanguage(filename) : "text";
  return (
    <div className="file-preview file-preview--code">
      <div className="file-preview__head">
        <span className="file-preview__name">{filename}</span>
        <span className="file-preview__lang">{lang}</span>
        <button
          type="button"
          className="file-preview__copy"
          onClick={() => void copy()}
          aria-label="复制内容"
          disabled={status === "copying" || !content}
          aria-busy={status === "copying"}
        >
          {status === "copying" ? "复制中…" : status === "copied" ? "已复制" : "复制"}
        </button>
      </div>
      <CopyFeedback status={status} />
      {content ? <pre className="file-preview__code">
        <code>{content}</code>
      </pre> : <div className="file-preview__placeholder"><FileText size={28} aria-hidden="true" /><span>文件内容为空</span></div>}
    </div>
  );
}

/**
 * 文档预览(docx/pptx/sheet)—— 对齐 EchoAgent media-preview。
 * 有提取文本则渲染(段落/幻灯片/表格);无解压器则显示降级占位。
 */
function DocPreview({
  filename,
  kind,
  text,
  sheets,
  onCopyText,
}: {
  filename: string;
  kind: "docx" | "pptx" | "sheet";
  text: string | null;
  sheets?: Array<{ name: string; rows: string[][] }>;
  onCopyText?: FilePreviewCopyHandler;
}) {
  const { status, copy } = usePreviewCopy(text ?? "", filename, onCopyText);
  return (
    <div className={"file-preview file-preview--doc"}>
      <div className="file-preview__head">
        <span className="file-preview__name">{filename}</span>
        <span className="file-preview__kind">{previewKindLabel(kind)}</span>
        {text?.trim() && (
          <button
            type="button"
            className="file-preview__copy"
            onClick={() => void copy()}
            aria-label="复制文本"
            disabled={status === "copying"}
            aria-busy={status === "copying"}
          >
            {status === "copying" ? "复制中…" : status === "copied" ? "已复制" : "复制文本"}
          </button>
        )}
      </div>
      <CopyFeedback status={status} />
      {text == null ? (
        <div className="file-preview__placeholder">
          <FileText size={28} aria-hidden="true" />
          <span className="file-preview__placeholder-text">
            暂时无法读取此文件的内容，请用系统应用打开原件。
          </span>
        </div>
      ) : !text.trim() ? (
        <div className="file-preview__placeholder" role="note"><FileText size={28} aria-hidden="true" /><span>未提取到可阅读的文本</span><span className="file-preview__detail">文档可能为空，或仅包含图片。请用系统应用查看原件。</span></div>
      ) : kind === "sheet" && sheets && sheets.length > 0 ? (
        <div className="file-preview__doc-body">
          {sheets.map((s, si) => (
            <div key={si} className="file-preview__sheet">
              <div className="file-preview__sheet-name">{s.name}</div>
              <table className="file-preview__table">
                <tbody>
                  {s.rows.map((row, ri) => (
                    <tr key={ri}>
                      {row.map((cell, ci) => (
                        <td key={ci}>{cell}</td>
                      ))}
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          ))}
        </div>
      ) : (
        <pre className="file-preview__doc-text">{text}</pre>
      )}
    </div>
  );
}
