import {
  saveAttachmentBlob,
  attachmentThumbnail,
  discardAttachmentBlob,
} from "@/lib/agent-client";
import type { CodingIntent } from "../lib/workflow";
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import {
  AlertTriangle,
  ArrowUp,
  ImagePlus,
  FileCode2,
  ListTodo,
  Terminal,
  Plus,
  ChevronDown,
  LoaderCircle,
  X,
} from "lucide-react";

import { ModelSelector, type ModelOption } from "@/components/ModelSelector";
import { PermissionPicker } from "@/components/PermissionPicker";
import { ActionMenu } from "@/components/ActionMenu";
import {
  DRAFT_TTL_MS,
  useAiDraftStore,
} from "@/features/coding/store/ai-draft-store";

import { codingTaskDraftKey } from "../lib/task-draft-key";
import type { CodingTask } from "../lib/types";

interface TheiaAgentComposerProps {
  workspaceRoot: string;
  task: CodingTask | null;
  sessionId: string | null;
  models: ModelOption[];
  modelId?: string;
  intent?: CodingIntent;
  onIntentChange?: (intent: CodingIntent) => void;
  includeEditor?: boolean;
  editorLabel?: string;
  onRemoveEditor?: () => void;
  onCaptureContext?: (kind: "document" | "diagnostics" | "terminal") => void;
  extraContext?: Array<{ id: string; label: string; text: string }>;
  onRemoveExtraContext?: (id: string) => void;
  onPreviewContext?: (id: string) => void;
  onCorrect?: (text: string) => boolean | Promise<boolean>;

  contextPaths: string[];
  apiReady: boolean;
  startError?: string | null;
  starting: boolean;
  sending: boolean;
  streaming: boolean;
  executionBlocked?: boolean;
  awaitingDecision?: boolean;
  onModelChange: (modelId: string) => void | Promise<void>;
  onStart: (
    requirement: string,
    attachments?: string[],
  ) => boolean | void | Promise<boolean | void>;
  onSend: (
    text: string,
    attachments?: string[],
  ) => boolean | void | Promise<boolean | void>;
  onEnqueue?: (
    text: string,
    contextPaths: string[],
    images?: string[],
  ) => boolean | void | Promise<boolean | void>;
  onRemoveContext: (path: string) => void;
  onDraftContextPaths: (paths: string[]) => void;
  onOpenSettings?: () => void;
  onToast?: (message: string) => void;
}

function draftKey(root: string, taskId?: string): string {
  return taskId
    ? `echo-coding-followup-v1:${encodeURIComponent(root)}:${encodeURIComponent(taskId)}`
    : codingTaskDraftKey(root);
}

function readDraft(key: string): string {
  try {
    return localStorage.getItem(key) ?? "";
  } catch {
    return "";
  }
}

/** One composer stays at the same position while the task's views change. */
export function TheiaAgentComposer({
  workspaceRoot,
  task,
  sessionId,
  models,
  modelId,
  intent = "agent",
  onIntentChange,
  includeEditor = false,
  editorLabel = "当前文件",
  onRemoveEditor,
  onCaptureContext,
  extraContext = [],
  onRemoveExtraContext,
  onPreviewContext,
  onCorrect,
  contextPaths,
  apiReady,
  startError,
  starting,
  sending,
  streaming,
  executionBlocked = false,
  awaitingDecision = false,
  onModelChange,
  onStart,
  onSend,
  onEnqueue,
  onRemoveContext,
  onDraftContextPaths,
  onOpenSettings,
  onToast,
}: TheiaAgentComposerProps) {
  const key = draftKey(workspaceRoot, task?.id);
  const [text, setText] = useState(() => readDraft(key));
  const [submitting, setSubmitting] = useState(false);
  const submittingRef = useRef(false);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const imageInput = useRef<HTMLInputElement>(null);
  const addingImagesRef = useRef(false);
  const [addingImages, setAddingImages] = useState(false);
  const [images, setImages] = useState<
    Array<{ path: string; preview: string; size?: number }>
  >(() => {
    try {
      const stored = JSON.parse(localStorage.getItem(`${key}:images`) ?? "[]");
      return Array.isArray(stored)
        ? stored
            .filter((image) => image && typeof image.path === "string")
            .slice(0, 5)
        : [];
    } catch {
      return [];
    }
  });
  const attachmentScope = useRef(key);
  useEffect(() => {
    attachmentScope.current = key;
  }, [key]);
  useEffect(() => {
    localStorage.setItem(
      `${key}:images`,
      JSON.stringify(images.map((image) => ({ ...image, preview: "" }))),
    );
  }, [images, key]);
  const addImages = async (files: File[]) => {
    if (!files.length || addingImagesRef.current) return;
    const scope = key;
    if (
      images.length + files.length > 5 ||
      images.reduce((sum, image) => sum + (image.size ?? 10 * 1024 * 1024), 0) +
        files.reduce((sum, file) => sum + file.size, 0) >
        20 * 1024 * 1024 ||
      files.some(
        (file) =>
          !/^image\/(png|jpeg|webp|gif)$/.test(file.type) ||
          file.size > 10 * 1024 * 1024,
      )
    ) {
      onToast?.(
        "最多添加 5 张 PNG/JPEG/WebP/GIF 图片，单张 10MB、本次总计 20MB",
      );
      return;
    }
    addingImagesRef.current = true;
    setAddingImages(true);
    const added: Array<{ path: string; preview: string; size: number }> = [];
    try {
      for (const file of files) {
        const path = await saveAttachmentBlob({
          bytes: new Uint8Array(await file.arrayBuffer()),
          mime: file.type,
          suggestedName: file.name,
        });
        if (attachmentScope.current !== scope) {
          await discardAttachmentBlob(path);
          continue;
        }
        const preview = await attachmentThumbnail(path).catch(() => "");
        added.push({ path, preview, size: file.size });
      }
      if (attachmentScope.current === scope)
        setImages((current) => [...current, ...added]);
    } catch (error) {
      await Promise.allSettled(
        added.map((image) => discardAttachmentBlob(image.path)),
      );
      onToast?.(`图片添加失败：${String(error)}`);
    } finally {
      addingImagesRef.current = false;
      setAddingImages(false);
    }
  };

  const queuedDraft = useAiDraftStore((state) => state.draft);
  const consumeDraft = useAiDraftStore((state) => state.consume);

  useLayoutEffect(() => {
    const input = inputRef.current;
    if (!input) return;
    const resize = () => {
      input.style.height = "0px";
      // CSS bounds the field; measuring the draft also lets it shrink after
      // deleting text. Only the input scrolls, keeping the toolbar visible.
      input.style.height = `${input.scrollHeight}px`;
    };
    resize();
    let width = input.getBoundingClientRect().width;
    const observer =
      typeof ResizeObserver === "undefined"
        ? null
        : new ResizeObserver(() => {
            const next = input.getBoundingClientRect().width;
            if (next !== width) {
              width = next;
              resize();
            }
          });
    observer?.observe(input);
    // Settings changes rem-based typography without changing the field width.
    // Watch the root style used by applyFontSize so an existing draft resizes
    // before the next paint, preserving the textarea's focus and selection.
    const typographyObserver = new MutationObserver(resize);
    typographyObserver.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["style"],
    });
    window.addEventListener("resize", resize);
    return () => {
      observer?.disconnect();
      typographyObserver.disconnect();
      window.removeEventListener("resize", resize);
    };
  }, [text]);

  useEffect(() => {
    if (!queuedDraft) return;
    const draft = consumeDraft();
    if (!draft || draft.createdAt + DRAFT_TTL_MS < Date.now()) return;
    if (draft.prompt) setText(draft.prompt);
    if (draft.contextPaths.length > 0) onDraftContextPaths(draft.contextPaths);
  }, [queuedDraft, consumeDraft, onDraftContextPaths]);

  useEffect(() => {
    try {
      if (text) localStorage.setItem(key, text);
      else localStorage.removeItem(key);
    } catch {
      // Draft recovery never blocks editing or sending.
    }
  }, [key, text]);

  const sessionUnavailable = Boolean(
    task && (!sessionId || task.sessionId !== sessionId),
  );
  const verifying = task?.phase === "verifying";
  const busy =
    addingImages ||
    submitting ||
    (sending && !streaming) ||
    (!task && starting);
  const queueing = Boolean(task && onEnqueue && (streaming || verifying));
  const canSend =
    !executionBlocked &&
    !awaitingDecision &&
    !submitting &&
    Boolean(text.trim()) &&
    (task
      ? !sessionUnavailable &&
        (queueing || (!sending && !streaming && !verifying))
      : !starting && apiReady && Boolean(modelId));

  const submit = async () => {
    const prompt = text.trim();
    if (!prompt || !canSend || submittingRef.current || addingImagesRef.current)
      return;
    submittingRef.current = true;
    setSubmitting(true);
    try {
      if (!task) {
        const accepted = await onStart(
          prompt,
          images.map((image) => image.path),
        );
        if (accepted !== false) setImages([]);
        return;
      }
      const accepted = queueing
        ? await onEnqueue?.(
            prompt,
            [...contextPaths],
            images.map((image) => image.path),
          )
        : await onSend(
            prompt,
            images.map((image) => image.path),
          );
      if (accepted !== false) {
        setText((current) => (current === text ? "" : current));
        setImages([]);
      }
    } catch (error) {
      onToast?.(`发送失败：${String(error).replace(/^Error:\s*/, "")}`);
    } finally {
      submittingRef.current = false;
      setSubmitting(false);
    }
  };

  return (
    <div
      className="echo-theia-agent__composer"
      aria-label={task ? "任务追问" : "新建开发任务"}
    >
      <div className="echo-theia-agent__composer-box">
        {(includeEditor || extraContext.length > 0 || images.length > 0 || contextPaths.length > 0 || streaming) &&
          <div className="echo-theia-agent__attachments">
            {(includeEditor || extraContext.length > 0) && <div className="coding-context-controls" aria-label="已添加的编辑器上下文">
              {includeEditor && (
                <span>
                  <button
                    type="button"
                    onClick={() => onCaptureContext?.("document")}
                  >
                    {editorLabel} · 预览
                  </button>
                  <button
                    type="button"
                    aria-label="移除编辑器上下文"
                    onClick={onRemoveEditor}
                  >
                    <X size={11} />
                  </button>
                </span>
              )}
              {extraContext.map((item) => (
                <span key={item.id}>
                  <button type="button" onClick={() => onPreviewContext?.(item.id)}>
                    {item.label}
                  </button>
                  <button
                    type="button"
                    aria-label={`移除 ${item.label}`}
                    onClick={() => onRemoveExtraContext?.(item.id)}
                  >
                    <X size={11} />
                  </button>
                </span>
              ))}
            </div>}
            {images.length > 0 && (
              <div className="coding-image-context" aria-label="图片上下文">
                {images.map((image) => (
                  <span key={image.path} title={image.path}>
                    {image.preview ? (
                      <img
                        src={image.preview}
                        alt={image.path.split(/[\\/]/).pop()}
                      />
                    ) : (
                      <small>{image.path.split(/[\\/]/).pop()}</small>
                    )}
                    <button
                      type="button"
                      aria-label="移除图片"
                      onClick={() => {
                        setImages((current) =>
                          current.filter((item) => item.path !== image.path),
                        );
                        void discardAttachmentBlob(image.path).catch(() => undefined);
                      }}
                    >
                      <X size={11} />
                    </button>
                  </span>
                ))}
              </div>
            )}
            {contextPaths.length > 0 && (
              <div
                className="echo-theia-agent__context-chips"
                aria-label="任务上下文"
              >
                {contextPaths.map((path) => (
                  <span
                    key={path}
                    className="echo-theia-agent__context-chip"
                    title={path}
                  >
                    <span>{path.split(/[\\/]/).pop()}</span>
                    <button
                      type="button"
                      aria-label={`移除上下文 ${path}`}
                      onClick={() => onRemoveContext(path)}
                    >
                      <X size={11} />
                    </button>
                  </span>
                ))}
              </div>
            )}
            {streaming && (
              <small className="coding-panel__hint">
                模式选择用于下一轮；停止当前执行需等待停止确认。
              </small>
            )}
            {images.length > 0 && (
              <small role="status">
                将发送真实图片；模型视觉能力由当前服务决定，不支持时显示限制。
              </small>
            )}
          </div>}
        <textarea
          ref={inputRef}
          className="echo-theia-agent__input"
          value={text}
          onChange={(event) => setText(event.target.value)}
          onPaste={(event) => {
            const files = Array.from(event.clipboardData.files).filter((file) =>
              file.type.startsWith("image/"),
            );
            if (files.length) {
              event.preventDefault();
              void addImages(files);
            }
          }}
          onKeyDown={(event) => {
            if (
              !event.nativeEvent.isComposing &&
              event.nativeEvent.keyCode !== 229 &&
              !event.shiftKey &&
              event.key === "Enter"
            ) {
              event.preventDefault();
              void submit();
            }
          }}
          rows={3}
          placeholder={
            !task
              ? intent === "ask"
                ? "提问，或添加代码来讨论…"
                : intent === "plan"
                  ? "描述目标，一起制定方案…"
                  : "描述你想完成的改动…"
              : sessionUnavailable
                ? "当前任务未绑定 Agent 会话"
                : verifying
                  ? queueing
                    ? "验证中可补充要求，Enter 加入队列…"
                    : "可写入草稿，验证完成后手动发送…"
                  : streaming
                    ? queueing
                      ? "补充开发要求，Enter 加入队列…"
                      : "可写入草稿，本轮完成后手动发送…"
                    : "给 Agent 补充要求或让它修改当前代码…"
          }
          aria-label={task ? "补充开发要求" : "任务描述"}
          disabled={
            awaitingDecision || sessionUnavailable || (sending && !queueing)
          }
        />
        {!task && !apiReady && (
          <div className="coding-agent__warning">
            <AlertTriangle size={13} /> 尚未配置可用模型。
            <button type="button" onClick={onOpenSettings}>
              前往设置
            </button>
          </div>
        )}
        {startError && (
          <div className="coding-agent__warning" role="alert">
            <AlertTriangle size={13} />
            {startError}
          </div>
        )}
        <div
          className="echo-theia-agent__composer-tools"
          role="group"
          aria-label="开发任务输入选项"
        >
          <div className="coding-intent-control">
            <select
              className="coding-intent-select"
              aria-label="开发意图"
              value={intent}
              onChange={(event) =>
                onIntentChange?.(event.target.value as CodingIntent)
              }
              title={
                streaming ? "用于下一轮；当前运行保持原模式" : "本轮执行权限"
              }
            >
              <option value="ask">Ask</option>
              <option value="plan">Plan</option>
              <option value="agent">Agent</option>
            </select>
          <ChevronDown size={11} aria-hidden />
          </div>
          <ModelSelector
            modelId={modelId}
            models={models}
            onModelChange={onModelChange}
            ariaLabel="开发任务模型"
            disabled={busy || sessionUnavailable}
          />
          <input
            ref={imageInput}
            type="file"
            accept="image/png,image/jpeg,image/webp,image/gif"
            multiple
            hidden
            onChange={(event) => {
              void addImages(Array.from(event.target.files ?? []));
              event.target.value = "";
            }}
          />
          <ActionMenu
            label="添加上下文"
            menuLabel="添加到对话"
            resetKey={key}
            trigger={<Plus size={16} />}
            triggerClassName="coding-composer-icon"
            menuClassName="topbar-actions__menu coding-composer-context-menu"
            busy={addingImages}
          >
            {({ close }) => <>
              <button type="button" className="topbar-actions__item" role="menuitem" disabled={!onCaptureContext} onClick={() => { close(); onCaptureContext?.("document"); }}>
                <FileCode2 size={15} /><span>当前文件或选区</span>
              </button>
              <button type="button" className="topbar-actions__item" role="menuitem" disabled={!onCaptureContext} onClick={() => { close(); onCaptureContext?.("diagnostics"); }}>
                <ListTodo size={15} /><span>诊断（Problems）</span>
              </button>
              <button type="button" className="topbar-actions__item" role="menuitem" disabled={!onCaptureContext} onClick={() => { close(); onCaptureContext?.("terminal"); }}>
                <Terminal size={15} /><span>终端输出</span>
              </button>
              <button type="button" className="topbar-actions__item" role="menuitem" onClick={() => { close(); imageInput.current?.click(); }}>
                <ImagePlus size={15} /><span>图片或截图</span>
              </button>
            </>}
          </ActionMenu>
          <PermissionPicker
            onToast={onToast}
            sessionId={sessionId ?? undefined}
            compact
          />
          {streaming && onCorrect && (
            <button
              type="button"
              disabled={
                executionBlocked || !text.trim() || images.length > 0 || submitting || addingImages
              }
              onClick={async () => {
                if (executionBlocked) return;
                if (await onCorrect(text.trim())) setText("");
              }}
              title="通过 Runtime 原子控制替换当前要求，保留已经发生的修改"
            >
              纠偏
            </button>
          )}
          <button
            type="button"
            className="echo-theia-agent__send"
            disabled={!canSend}
            onClick={() => void submit()}
            aria-label={
              queueing
                ? "加入待发送队列"
                : task
                  ? "发送给 Agent"
                  : "开始 Agent 任务"
            }
            aria-busy={busy}
            title={
              queueing
                ? "Enter 排队；本轮结束后自动发送"
                : busy
                  ? "正在发送…"
                  : "Enter 发送；Shift+Enter 换行"
            }
          >
            {busy ? (
              <LoaderCircle size={16} className="is-spinning" />
            ) : (
              <ArrowUp size={17} />
            )}
          </button>
        </div>
      </div>
    </div>
  );
}
