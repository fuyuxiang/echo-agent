import { useState } from "react";
import type { ToolCallView } from "@/stores/session-store";
import { documentProgressCount, documentProgressLabel } from "@/lib/document-progress";
import { isOfficeCreateTool, officeInputFromToolCall, officeReceiptFromToolCall } from "@/lib/document-export";
import type { DiffContent, CommandOutputContent, ImageToolContent } from "@/lib/types";
import { checkCommandRisk, riskLabel } from "@/lib/command-risk";
import { precheckCommand } from "@/lib/sandbox-guard";
import { computeUnifiedDiff, hunksToUnifiedLines, summarizeDiff, type DiffLine } from "@/lib/unified-diff";
import { CheckIcon } from "@/foundation/components/Icon/icons";
import { orgFetchDocument, orgQaFeedback } from "@/lib/org-client";
import {
  detectToolCallRenderer,
  rendererLabel,
  rendererIcon,
  summarizeTool,
} from "@/lib/tool-renderers";

type ToolCallCardProps = {
  tc: ToolCallView;
  /** Open the right-side detail drawer (Phase 2). */
  onOpen?: (tc: ToolCallView) => void;
  showStatusLabel?: boolean;
};

const MAX_TOOL_IMAGE_BASE64_CHARS = 16 * 1024 * 1024;
const SAFE_TOOL_IMAGE_MIME = new Set(["image/png", "image/jpeg", "image/gif", "image/webp"]);

function toolImageSource(image: ImageToolContent): string | null {
  const mimeType = image.mimeType.trim().toLowerCase();
  const data = image.data.trim();
  // Tool output is model/server-controlled. Never auto-load its optional URI:
  // an http/file URI could probe local services or disclose that a message was
  // viewed. Only bounded inline raster bytes are rendered; SVG stays blocked.
  if (
    !SAFE_TOOL_IMAGE_MIME.has(mimeType)
    || data.length === 0
    || data.length > MAX_TOOL_IMAGE_BASE64_CHARS
    || !/^[a-z0-9+/]*={0,2}$/i.test(data)
  ) {
    return null;
  }
  return `data:${mimeType};base64,${data}`;
}

/**
 * Compact inline tool-call row (Phase 1 — EchoAgent `unknown-tool-compact`).
 *
 * Always one line in the transcript: kind + short title + status.
 * Details (command/diff/output) open in the side drawer via `onOpen`.
 */
export function ToolCallCard({ tc, onOpen, showStatusLabel = false }: ToolCallCardProps) {
  if (isSilentOrganizationSkip(tc)) return (
    <button type="button" className="toolcall toolcall--compact" data-tool-call-id={tc.toolCallId} onClick={() => onOpen?.(tc)}
      title="本轮组织知识检索不可用，回答未包含该来源。点击查看原因；可在组织页面重新连接。">
      <span className="toolcall__kind">知识来源</span>
      <span className="toolcall__title">本轮未使用组织知识 · 查看原因</span>
    </button>
  );

  const statusCls =
    tc.status === "completed"
      ? "toolcall--ok"
      : tc.status === "failed"
        ? "toolcall--err"
        : "toolcall--run";

  const statusLabel =
    tc.status === "completed" ? "完成" : tc.status === "failed" ? "失败" : "运行中";

  // 状态符号：完成态用 SVG 对勾（文本 "✓" U+2713 在 macOS WKWebView 下依赖
  // 字体回退，可能渲染成 tofu/emoji 样式）；"!" / "…" 是 ASCII/通用字符，安全。
  const statusMark =
    tc.status === "completed" ? (
      <CheckIcon size={10} strokeWidth={3} />
    ) : tc.status === "failed" ? (
      "!"
    ) : (
      "…"
    );

  const officeTool = isOfficeCreateTool(tc);
  const officeInput = officeInputFromToolCall(tc);
  const officeTitle = typeof officeInput?.title === "string" ? officeInput.title : "办公文档";
  const shortTitle = officeTool ? `生成 ${officeTitle}` : shortenTitle(tc.title, tc.kind);

  // 专用渲染器(对齐 EchoAgent tools/renderers):非 default/unknown 时用图标 +
  // 渲染器标签 + 摘要替代通用 kind 文案。
  const renderer = detectToolCallRenderer(tc);
  const specialized =
    renderer !== "default" && renderer !== "unknown";
  const kindLabel = officeTool ? "文档" : specialized
    ? `${rendererIcon(renderer)} ${rendererLabel(renderer)}`
    : prettyKind(tc.kind);
  const summary = tc.documentProgress ? documentProgressLabel(tc.documentProgress)
    : officeTool ? shortTitle : specialized ? summarizeTool(tc, renderer) : shortTitle;

  return (
    <button
      type="button"
      className={"toolcall toolcall--compact " + statusCls}
      data-tool-call-id={tc.toolCallId}
      onClick={() => onOpen?.(tc)}
      title={`${officeTool ? "办公文档" : tc.kind}: ${officeTool ? officeTitle : tc.title}（${statusLabel}，点击查看详情）`}
      aria-label={`${officeTool ? "办公文档" : tc.kind} ${summary} ${statusLabel}`}
    >
      <span className="toolcall__kind">{kindLabel}</span>
      <span className="toolcall__title">{tc.documentProgress?.source ?? summary}</span>
      {tc.documentProgress && <span className="toolcall__document-count" aria-hidden="true">
        {documentProgressCount(tc.documentProgress)}
      </span>}
      {showStatusLabel && <span className="toolcall__status-label" aria-hidden="true">{statusLabel}</span>}
      <span className={"toolcall__status-mark toolcall__status-mark--" + tc.status}>
        {statusMark}
      </span>
    </button>
  );
}

function isSilentOrganizationSkip(tc: ToolCallView): boolean {
  if (!isOrganizationKnowledgeTool(tc.kind) || tc.status !== "completed") return false;
  return tc.content.some((content) => {
    if (content.type !== "text") return false;
    const value = parseOrganizationResult(content.text);
    return value?.available === false && value.skipped === true;
  });
}

function prettyKind(kind: string): string {
  const k = (kind || "tool").toLowerCase();
  if (k.includes("edit") || k === "write" || k === "write_file") return "edit";
  if (k.includes("read")) return "read";
  if (k.includes("shell") || k.includes("terminal") || k.includes("execute") || k === "bash")
    return "shell";
  if (k.includes("search") || k.includes("grep") || k.includes("glob")) return "search";
  if (k.includes("list")) return "list";
  if (k.includes("ask") || k.includes("question") || k === "other") return "ask";
  return k.length > 12 ? k.slice(0, 12) : k;
}

/** Prefer a path / command snippet over the full verbose title. */
function shortenTitle(title: string, kind: string): string {
  const t = (title || "").trim();
  if (!t) return kind || "tool";
  // "Write `path`" / Write "path" / Write path
  const write = t.match(/Write\s+[`'"]?(.+?)[`'"]?\s*$/i);
  if (write?.[1]) return write[1];
  // Execute 'cmd' / Run …
  const exec = t.match(/^(?:Execute|Run)\s+[`'"]?(.+?)[`'"]?\s*$/i);
  if (exec?.[1]) {
    const cmd = exec[1];
    return cmd.length > 64 ? cmd.slice(0, 64) + "…" : cmd;
  }
  return t.length > 72 ? t.slice(0, 72) + "…" : t;
}

// ---------- shared detail body (drawer / artifacts) ----------

export function ToolCallDetailBody({
  tc,
  onOpenPath,
}: {
  tc: ToolCallView;
  onOpenPath?: (path: string) => void;
}) {
  const diff = tc.content.find((c) => c.type === "diff") as DiffContent | undefined;
  const cmd = tc.content.find((c) => c.type === "command_output") as
    | CommandOutputContent
    | undefined;
  const images = tc.content.filter((c) => c.type === "image") as ImageToolContent[];
  const texts = tc.content.filter((c) => c.type === "text") as Array<{
    type: "text";
    text: string;
  }>;
  const officeTool = isOfficeCreateTool(tc);
  const officeReceipt = officeReceiptFromToolCall(tc);
  const parsedOrganizationResult = isOrganizationKnowledgeTool(tc.kind)
    ? parseOrganizationResult(texts[0]?.text)
    : null;
  const organizationResult = parsedOrganizationResult
    && isKnowledgeContextResult(parsedOrganizationResult)
    ? parsedOrganizationResult
    : null;
  const personalResult = isPersonalKnowledgeTool(tc.kind)
    ? parseOrganizationResult(texts[0]?.text)
    : null;

  return (
    <div className="tool-detail">
      <div className="tool-detail__meta">
        <span className="toolcall__kind">{officeTool ? "文档" : prettyKind(tc.kind)}</span>
        <span className={"tool-detail__status tool-detail__status--" + tc.status}>
          {tc.status === "completed"
            ? "已完成"
            : tc.status === "failed"
              ? "失败"
              : "运行中"}
        </span>
      </div>
      <h3 className="tool-detail__title">{officeTool ? "生成办公文档" : tc.title}</h3>
      {tc.documentProgress && (
        <div className="document-read-progress" role="status" aria-label="资料读取进度">
          <span>{documentProgressLabel(tc.documentProgress)}</span>
          {tc.documentProgress.scope && <span>本次范围：{tc.documentProgress.scope}</span>}
          <progress aria-label={`${tc.documentProgress.source} 读取进度`}
            value={tc.documentProgress.readPages ?? tc.documentProgress.readRecords}
            max={Math.max(1, tc.documentProgress.totalPages ?? tc.documentProgress.totalRecords)} />
          <span>{tc.documentProgress.complete ? "已读取全部内容" : "已读取部分内容"}
            {tc.documentProgress.checkpointCount > 0 ? ` · 已保存 ${tc.documentProgress.checkpointCount} 份阶段记录` : ""}</span>
          {tc.documentProgress.checkpointPath && onOpenPath && <button type="button"
            className="document-read-progress__findings"
            onClick={() => onOpenPath(tc.documentProgress!.checkpointPath!)}>查看阶段记录</button>}
        </div>
      )}

      {officeReceipt && (
        <div className="toolcall__text">
          <p>已生成 {officeReceipt.format.toUpperCase()} 文件</p>
          <button type="button" className="diff__path diff__path--clickable"
            onClick={() => onOpenPath?.(officeReceipt.path)}
            title={`打开：${officeReceipt.path}`}>
            {officeReceipt.path}
          </button>
        </div>
      )}

      {diff && (
        <DiffView
          diff={diff.diff}
          onOpenPath={onOpenPath}
        />
      )}
      {cmd && (
        <div className="toolcall__cmd">
          {cmd.command && <CommandRiskBadge command={cmd.command} />}
          {cmd.command && (
            <pre className="toolcall__cmd-line">
              <span className="toolcall__prompt">$</span>
              {cmd.command}
            </pre>
          )}
          {cmd.output && <pre className="toolcall__output">{cmd.output}</pre>}
        </div>
      )}
      {images.length > 0 && (
        <div className="toolcall__images">
          {images.map((img, i) => {
            const src = toolImageSource(img);
            return src ? (
              <img
                key={i}
                className="toolcall__image"
                src={src}
                alt={`工具输出图片 ${i + 1}`}
                loading="lazy"
              />
            ) : (
              <p key={i} className="tool-detail__empty" role="note">
                已拦截不安全或过大的工具图片输出
              </p>
            );
          })}
        </div>
      )}
      {organizationResult && <OrganizationKnowledgeResult value={organizationResult} />}
      {personalResult && <PersonalKnowledgeResult value={personalResult} onOpenPath={onOpenPath} />}
      {!officeReceipt && !organizationResult && !personalResult && texts.map((t, i) => (
        <pre key={i} className="toolcall__text">
          {t.text}
        </pre>
      ))}
      {!officeTool && !diff && !cmd && images.length === 0 && texts.length === 0 && tc.rawInput != null && (
        <pre className="toolcall__text toolcall__raw-input">
          {typeof tc.rawInput === "string"
            ? tc.rawInput
            : JSON.stringify(tc.rawInput, null, 2)}
        </pre>
      )}
      {!diff && !cmd && images.length === 0 && texts.length === 0 && tc.rawInput == null && (
        <p className="tool-detail__empty">暂无详细输出</p>
      )}
    </div>
  );
}

function isOrganizationKnowledgeTool(kind: string): boolean {
  if (/local_knowledge_(search|fetch)/i.test(kind)) return false;
  return /knowledge_(context|ask|feedback|search|fetch|list|who|submit)|organization_memory/i.test(kind);
}

function isPersonalKnowledgeTool(kind: string): boolean {
  return /local_knowledge_(search|fetch)/i.test(kind);
}

function isKnowledgeContextResult(value: Record<string, unknown>): boolean {
  return [
    "answer", "evidence", "chunks", "citations", "memories",
    "missingFacts", "confidence", "sufficient", "insufficient",
  ].some((key) => key in value);
}

function parseOrganizationResult(text: string | undefined): Record<string, unknown> | null {
  if (!text?.trim().startsWith("{")) return null;
  try {
    const value: unknown = JSON.parse(text);
    return value !== null && typeof value === "object" && !Array.isArray(value)
      ? value as Record<string, unknown>
      : null;
  } catch {
    return null;
  }
}

function OrganizationKnowledgeResult({ value }: { value: Record<string, unknown> }) {
  const [preview, setPreview] = useState<{ id: string; text: string } | null>(null);
  const [loadingCitation, setLoadingCitation] = useState<string | null>(null);
  const [interactionError, setInteractionError] = useState<string | null>(null);
  const [feedbackSent, setFeedbackSent] = useState(false);
  const rawEvidence = Array.isArray(value.evidence)
    ? value.evidence
    : Array.isArray(value.chunks)
      ? value.chunks
      : Array.isArray(value.citations)
        ? value.citations
        : [];
  const evidence = rawEvidence.filter(isRecord);
  const memories = Array.isArray(value.memories) ? value.memories.filter(isRecord) : [];
  const missingFacts = Array.isArray(value.missingFacts) ? value.missingFacts : [];
  const confidence = typeof value.confidence === "number" ? value.confidence : null;
  const sufficient = typeof value.sufficient === "boolean"
    ? value.sufficient
    : typeof value.insufficient === "boolean"
      ? !value.insufficient
      : null;
  const qaEventId = typeof value.qaEventId === "string"
    ? value.qaEventId
    : typeof value.qa_event_id === "string"
      ? value.qa_event_id
      : null;
  const openEvidence = async (source: Record<string, unknown>, index: number) => {
    const citation = source.citation && typeof source.citation === "object"
      ? source.citation as Record<string, unknown>
      : {};
    const docId = String(source.docId ?? source.doc_id ?? citation.docId ?? citation.doc_id ?? "").trim();
    const pageValue = citation.page ?? source.page;
    const page = typeof pageValue === "number" && Number.isInteger(pageValue) && pageValue > 0
      ? pageValue
      : undefined;
    if (!docId) return;
    const id = String(source.chunkId ?? source.id ?? `${docId}-${index}`);
    if (preview?.id === id) {
      setPreview(null);
      return;
    }
    setInteractionError(null);
    setLoadingCitation(id);
    try {
      const document = await orgFetchDocument(docId, page);
      setPreview({ id, text: document.text });
    } catch (error) {
      setInteractionError(`引用原文读取失败：${String(error).replace(/^Error:\s*/, "")}`);
    } finally {
      setLoadingCitation(null);
    }
  };
  const sendFeedback = async (feedback: "helpful" | "not_helpful" | "wrong") => {
    if (!qaEventId || feedbackSent) return;
    setInteractionError(null);
    try {
      await orgQaFeedback(qaEventId, feedback);
      setFeedbackSent(true);
    } catch (error) {
      setInteractionError(`反馈提交失败：${String(error).replace(/^Error:\s*/, "")}`);
    }
  };
  return <div className="org-tool-result">
    <div className="org-tool-result__summary">
      {sufficient !== null && <span className={sufficient ? "is-good" : "is-warning"}>{sufficient ? "证据充分" : "证据有缺口"}</span>}
      {confidence !== null && <span>置信度 {Math.round(confidence * 100)}%</span>}
      <span>{evidence.length} 条依据</span><span>{memories.length} 条经验</span>
    </div>
    {typeof value.answer === "string" && value.answer && <p className="org-tool-result__answer">{value.answer}</p>}
    {missingFacts.length > 0 && <div className="org-tool-result__missing"><strong>仍需确认</strong>{missingFacts.slice(0, 5).map((item, index) => <span key={index}>{String(item)}</span>)}</div>}
    {memories.length > 0 && <div className="org-tool-result__section"><strong>相关经验与规则</strong>{memories.slice(0, 8).map((item, index) => {
      const memory = item;
      return <article key={String(memory.id ?? index)}><header><span>{memoryKindName(memory.kind)}</span>{memory.stale === true && <em>已过期</em>}</header><p>{String(memory.content ?? "")}</p>{typeof memory.rationale === "string" && memory.rationale && <small>{memory.rationale}</small>}</article>;
    })}</div>}
    {evidence.length > 0 && <div className="org-tool-result__section"><strong>文档依据</strong>{evidence.slice(0, 12).map((item, index) => {
      const source = item;
      const citation = source.citation && typeof source.citation === "object" ? source.citation as Record<string, unknown> : {};
      const docId = String(source.docId ?? source.doc_id ?? citation.docId ?? citation.doc_id ?? "").trim();
      const id = String(source.chunkId ?? source.id ?? `${docId}-${index}`);
      return <article key={id}><header><span>{String(source.docTitle ?? source.title ?? source.doc ?? `依据 ${index + 1}`)}</span>{(source.stale === true) && <em>可能过时</em>}</header><small>{String(citation.heading ?? source.heading ?? "")}{citation.page != null || source.page != null ? ` · 第 ${String(citation.page ?? source.page)} 页` : ""}</small><p>{clipEvidence(String(source.text ?? source.quote ?? ""))}</p>{docId && <button type="button" className="org-tool-result__source-button" disabled={loadingCitation === id} onClick={() => void openEvidence(source, index)}>{loadingCitation === id ? "正在读取…" : preview?.id === id ? "收起原文" : "查看原文"}</button>}{preview?.id === id && <pre className="org-tool-result__source-preview">{preview.text}</pre>}</article>;
    })}</div>}
    {qaEventId && <div className="org-tool-result__feedback" aria-label="组织知识回答反馈">{feedbackSent ? <span>感谢反馈，已记录</span> : <><span>这些组织知识有帮助吗？</span><button type="button" onClick={() => void sendFeedback("helpful")}>有帮助</button><button type="button" onClick={() => void sendFeedback("not_helpful")}>没帮助</button><button type="button" onClick={() => void sendFeedback("wrong")}>引用有误</button></>}</div>}
    {interactionError && <div className="org-tool-result__error" role="alert">{interactionError}</div>}
  </div>;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}

function PersonalKnowledgeResult({
  value,
  onOpenPath,
}: {
  value: Record<string, unknown>;
  onOpenPath?: (path: string) => void;
}) {
  const items = Array.isArray(value.items)
    ? value.items.filter((item): item is Record<string, unknown> => Boolean(item) && typeof item === "object")
    : [];
  const fetchedPath = typeof value.path === "string" ? value.path : null;
  const fetchedText = typeof value.text === "string" ? value.text : null;
  if (items.length === 0 && !fetchedPath && !fetchedText) {
    return <p className="tool-detail__empty">个人知识库未找到匹配内容</p>;
  }
  return <div className="personal-kb-tool-result">
    {items.map((item, index) => {
      const path = typeof item.path === "string" ? item.path : "";
      const title = typeof item.title === "string" ? item.title : `知识条目 ${index + 1}`;
      const snippet = typeof item.snippet === "string" ? item.snippet : "";
      const sourceLabel = typeof item.sourceLabel === "string" ? item.sourceLabel : "";
      const startLine = typeof item.startLine === "number" ? item.startLine : null;
      const endLine = typeof item.endLine === "number" ? item.endLine : null;
      const location = startLine
        ? startLine === endLine || endLine === null
          ? `第 ${startLine} 行`
          : `第 ${startLine}–${endLine} 行`
        : "";
      return <button
        key={path || `${title}-${index}`}
        type="button"
        className="personal-kb-tool-result__item"
        onClick={() => path && onOpenPath?.(path)}
        disabled={!path || !onOpenPath}
        title={path || title}
      >
        <strong>{title}</strong>
        {(sourceLabel || location) && <small>{[sourceLabel, location].filter(Boolean).join(" · ")}</small>}
        {snippet && <span>{snippet}</span>}
        {path && <small>{path}</small>}
      </button>;
    })}
    {fetchedPath && <div className="personal-kb-tool-result__path">{fetchedPath}</div>}
    {fetchedText && <pre className="toolcall__text">{fetchedText}</pre>}
  </div>;
}

function memoryKindName(value: unknown): string {
  return ({ fact: "事实", decision: "决策", convention: "规范", pitfall: "踩坑", howto: "操作手册" } as Record<string, string>)[String(value)] ?? "经验";
}

function clipEvidence(value: string): string {
  const compact = value.replace(/\s+/g, " ").trim();
  return compact.length > 600 ? `${compact.slice(0, 597)}…` : compact;
}

/**
 * 命令风险徽章 —— 对齐 EchoAgent `command-risk`。
 *
 * 仅在 medium / high 时显示(对齐 EchoAgent 标注而非拦截);low 不渲染任何东西。
 * `reasons` 通过 title 悬浮提示展示命中原因。
 */
function CommandRiskBadge({ command }: { command: string }) {
  const risk = checkCommandRisk(command);
  // Renderer-side policy precheck. This is a warning only; actual execution
  // authorization is enforced by EchoAgent's backend permission rules.
  const sandboxCheck = precheckCommand(command);
  const hasRisk = risk.level !== "low";
  const hasSandboxDeny = sandboxCheck.action === "deny";

  if (!hasRisk && !hasSandboxDeny) return null;

  const badges: React.ReactNode[] = [];
  if (hasRisk) {
    const cls = risk.level === "high" ? "cmd-risk cmd-risk--high" : "cmd-risk cmd-risk--medium";
    const reasons = risk.reasons.length > 0 ? `\n原因:${risk.reasons.join("; ")}` : "";
    badges.push(
      <span key="risk" className={cls} role="status" title={`⚠️ ${riskLabel(risk.level)}命令${reasons}`}>
        ⚠️ {riskLabel(risk.level)}
      </span>,
    );
  }
  if (hasSandboxDeny) {
    badges.push(
      <span key="sandbox" className="cmd-risk cmd-risk--high" role="status"
        title={`策略预检警告：${sandboxCheck.reason} (${sandboxCheck.target})。最终是否执行由后端权限规则决定。`}>
        ⚠️ 敏感路径
      </span>,
    );
  }
  return <>{badges}</>;
}

function DiffView({
  diff,
  onOpenPath,
}: {
  diff: DiffContent["diff"];
  onOpenPath?: (path: string) => void;
}) {
  const path = diff.path || "";
  const oldText = diff.old ?? "";
  const newText = diff.new ?? "";

  // Use proper unified diff algorithm when we have old/new text.
  // Fall back to hunks if only hunks are provided.
  let lines: DiffLine[];
  if (diff.hunks && diff.hunks.length && !oldText && !newText) {
    lines = hunksToUnifiedLines(diff.hunks);
  } else {
    lines = computeUnifiedDiff(oldText, newText, 3);
  }

  const summary = summarizeDiff(lines);

  const pathEl = path ? (
    <button
      type="button"
      className="diff__path diff__path--clickable"
      onClick={() => onOpenPath?.(path)}
      title={`打开：${path}`}
    >
      {path}
    </button>
  ) : (
    <div className="diff__path">(unknown path)</div>
  );

  return (
    <div className="diff">
      {pathEl}
      {lines.length > 0 && (
        <div className="diff__stats">
          <span className="diff__stats-add">+{summary.added}</span>
          <span className="diff__stats-del">-{summary.removed}</span>
        </div>
      )}
      <pre className="diff__body">
        {lines.map((l, i) => (
          <div key={i} className={`diff__line diff__line--${l.kind}`}>
            <span className="diff__line-num">
              {l.oldLine ?? ""}
              {l.newLine != null && l.oldLine != null ? "," : ""}
              {l.newLine ?? ""}
            </span>
            <span className="diff__line-prefix">
              {l.kind === "add" ? "+" : l.kind === "del" ? "-" : " "}
            </span>
            <span className="diff__line-text">{l.text}</span>
          </div>
        ))}
      </pre>
    </div>
  );
}
