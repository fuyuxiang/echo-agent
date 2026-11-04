import { Component, useEffect, useMemo, useRef, useState, type ReactNode, type ComponentProps } from "react";
import { DiffEditor } from "@monaco-editor/react";
import { Check, ExternalLink, LoaderCircle, X } from "lucide-react";
import { codingApi } from "./lib/tauri-api";
import { useCodingActionScope } from "./hooks/useCodingActionScope";
import { prepareLocalMonaco, syncLocalMonacoTheme } from "./lib/local-monaco";

/** The React adapter disposes models before its diff widget. Own these two
 * read-only models so widget teardown finishes before their disposal. */
function ReviewDiffEditor(props: ComponentProps<typeof DiffEditor>) {
  const [prefix] = useState(() => `inmemory://echo-review/${crypto.randomUUID()}`);
  const disposeModels = useRef<(() => void) | null>(null);
  const lifetime = useRef(0);
  useEffect(() => {
    const epoch = ++lifetime.current;
    return () => {
      const dispose = disposeModels.current;
      queueMicrotask(() => { if (lifetime.current === epoch) dispose?.(); });
    };
  }, []);
  return <DiffEditor {...props}
    originalModelPath={`${prefix}/original`} modifiedModelPath={`${prefix}/modified`}
    keepCurrentOriginalModel keepCurrentModifiedModel
    beforeMount={(monaco) => {
      disposeModels.current = () => {
        for (const part of ["original", "modified"]) {
          const model = monaco.editor.getModel(monaco.Uri.parse(`${prefix}/${part}`));
          if (model && !model.isDisposed()) model.dispose();
        }
      };
      props.beforeMount?.(monaco);
    }} />;
}

class DiffEditorBoundary extends Component<{ children: ReactNode; onError: (error: Error) => void }, { failed: boolean }> {
  state = { failed: false };

  static getDerivedStateFromError() { return { failed: true }; }

  componentDidCatch(error: Error) { this.props.onError(error); }

  render() { return this.state.failed ? null : this.props.children; }
}

interface Props {
  root: string;
  taskId: string;
  path: string;
  onClose: () => void;
  onOpenFile: () => void;
  onReviewed: () => Promise<void>;
  onToast?: (message: string) => void;
}

const LANGUAGE_BY_EXTENSION: Record<string, string> = {
  c: "c", cc: "cpp", cpp: "cpp", cs: "csharp", css: "css",
  go: "go", h: "cpp", html: "html", java: "java", js: "javascript",
  json: "json", jsx: "javascript", kt: "kotlin", less: "less",
  md: "markdown", mdx: "mdx", php: "php", py: "python",
  rb: "ruby", rs: "rust", scss: "scss", sh: "shell", sql: "sql",
  swift: "swift", ts: "typescript", tsx: "typescript", xml: "xml",
  yaml: "yaml", yml: "yaml",
};

export function reviewLanguage(path: string): string {
  const name = path.replaceAll("\\", "/").split("/").pop() ?? "";
  const extension = name.includes(".") ? name.slice(name.lastIndexOf(".") + 1).toLowerCase() : "";
  return LANGUAGE_BY_EXTENSION[extension] ?? "plaintext";
}

export function TheiaTaskReview({ root, taskId, path, onClose, onOpenFile, onReviewed, onToast }: Props) {
  type ReviewDiff = { original: string; modified: string; binary: boolean; modifiedHash: string };
  const viewSequence = useRef(0);
  const diffSequence = useRef(0);
  const [storedDiff, setStoredDiff] = useState<{ view: number; instance: number; value: ReviewDiff } | null>(null);
  const [error, setError] = useState("");
  const [reviewing, setReviewing] = useState(false);
  const [binaryAcknowledged, setBinaryAcknowledged] = useState(false);
  const [reloadVersion, setReloadVersion] = useState(0);
  const [editorReadyScope, setEditorReadyScope] = useState("");
  const [editorMountedScope, setEditorMountedScope] = useState("");
  const [editorFailure, setEditorFailure] = useState<{ scope: string; message: string } | null>(null);
  // A → B → A is a fresh view, even when its paths and reload counter match.
  // Hide a previous view's payload synchronously, before any effect or paint.
  const view = useMemo(() => ++viewSequence.current, [root, taskId, path, reloadVersion]);
  const activeDiff = storedDiff?.view === view ? storedDiff : null;
  const diff = activeDiff?.value ?? null;
  const editorScope = `${view}:${activeDiff?.instance ?? "pending"}`;
  const editorReady = editorReadyScope === editorScope;
  const editorMounted = editorMountedScope === editorScope;
  const editorError = editorFailure?.scope === editorScope ? editorFailure.message : "";
  const [theme, setTheme] = useState(() => document.documentElement.getAttribute("data-theme") === "dark" ? "vs-dark" : "vs");
  const isCurrent = useCodingActionScope(root, taskId, path);

  useEffect(() => {
    const observer = new MutationObserver(() => {
      syncLocalMonacoTheme();
      setTheme(document.documentElement.getAttribute("data-theme") === "dark" ? "vs-dark" : "vs");
    });
    observer.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    let cancelled = false;
    setStoredDiff(null);
    setError("");
    setBinaryAcknowledged(false);
    setReviewing(false);
    codingApi.changeDiff(root, taskId, path).then((result) => {
      if (!cancelled) setStoredDiff({ view, instance: ++diffSequence.current, value: result });
    }).catch((reason) => {
      if (!cancelled) setError(String(reason));
    });
    return () => { cancelled = true; };
  }, [root, taskId, path, reloadVersion, view]);

  useEffect(() => {
    if (!diff || diff.binary) return;
    let cancelled = false;
    setEditorFailure(null);
    void prepareLocalMonaco().then(() => {
      if (!cancelled) setEditorReadyScope(editorScope);
    }).catch((reason: unknown) => {
      if (!cancelled) {
        setEditorFailure({ scope: editorScope, message: String(reason).replace(/^Error:\s*/, "") });
      }
    });
    return () => { cancelled = true; };
  }, [diff, editorScope]);

  const markReviewed = async () => {
    if (!diff?.modifiedHash || (!diff.binary && (!editorMounted || !!editorError))) {
      onToast?.("无法确认当前差异版本，请重新打开差异后再审阅");
      return;
    }
    setReviewing(true);
    try {
      await codingApi.markReviewed(root, taskId, path, diff.modifiedHash);
      if (!isCurrent()) return;
      await onReviewed();
      if (!isCurrent()) return;
    } catch (reason) {
      if (!isCurrent()) return;
      const message = String(reason).replace(/^Error:\s*/, "");
      setError(`审阅失败：${message}`);
      setStoredDiff(null);
      onToast?.(`审阅失败：${message}`);
    } finally {
      if (isCurrent()) setReviewing(false);
    }
  };

  return (
    <section className="echo-theia-review" aria-label={`任务差异 ${path}`}>
      <header className="echo-theia-review__header">
        <div><strong>任务差异</strong><span>{path}</span></div>
        <div>
          <button type="button" onClick={onOpenFile}><ExternalLink size={14} />打开文件</button>
          <button type="button" disabled={!diff?.modifiedHash || reviewing || (diff.binary ? !binaryAcknowledged : !editorMounted || !!editorError)} onClick={() => void markReviewed()}>
            {reviewing ? <LoaderCircle size={14} className="is-spinning" /> : <Check size={14} />}
            标记已审阅
          </button>
          <button type="button" aria-label="关闭差异" onClick={onClose}><X size={16} /></button>
        </div>
      </header>
      {error && <div className="echo-theia-review__state">{error}<button type="button" onClick={() => setReloadVersion((value) => value + 1)}>刷新差异</button></div>}
      {!error && !diff && <div className="echo-theia-review__state"><LoaderCircle size={17} className="is-spinning" />正在读取任务差异…</div>}
      {diff?.binary && (
        <div className="echo-theia-review__state echo-theia-review__binary">
          <p>此文件无法显示文本差异。请先在 IDE 或专用工具中核对文件内容，再确认已审阅。</p>
          <button type="button" onClick={onOpenFile}><ExternalLink size={14} /> 在 IDE 中打开文件</button>
          <label>
            <input type="checkbox" checked={binaryAcknowledged} onChange={(event) => setBinaryAcknowledged(event.target.checked)} />
            我已核对当前版本的二进制文件
          </label>
        </div>
      )}
      {diff && !diff.binary && editorError && <div className="echo-theia-review__state" role="alert">
        差异编辑器加载失败：{editorError}
        <button type="button" onClick={() => setReloadVersion((value) => value + 1)}>重试加载差异</button>
      </div>}
      {diff && !diff.binary && !editorError && !editorReady && <div className="echo-theia-review__state">
        <LoaderCircle size={17} className="is-spinning" />正在加载本地差异编辑器…
      </div>}
      {diff && !diff.binary && !editorError && editorReady && <DiffEditorBoundary key={editorScope} onError={(reason) => {
        if (isCurrent()) setEditorFailure({ scope: editorScope, message: reason.message });
      }}>
        <ReviewDiffEditor
          original={diff.original}
          modified={diff.modified}
          language={reviewLanguage(path)}
          theme={theme === "vs-dark" ? "echo-dark" : "echo-light"}
          loading="正在显示文本差异…"
          onMount={() => setEditorMountedScope(editorScope)}
          options={{ readOnly: true, renderSideBySide: true, minimap: { enabled: false }, automaticLayout: true }}
        />
      </DiffEditorBoundary>}
    </section>
  );
}
