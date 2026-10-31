import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import {
  AlertTriangle,
  Archive,
  BrainCircuit,
  Building2,
  ChevronDown,
  MoreHorizontal,
  Search,
  FileText,
  Download,
  Eye,
  FolderUp,
  Loader2,
  LockKeyhole,
  LogOut,
  RefreshCw,
  History,
  LayoutDashboard,
  MessageSquareText,
  PackageOpen,
  Plus,
  Upload,
  UserRound,
  UsersRound,
  Trash2,
  X,
  XCircle,
} from "lucide-react";
import {
  orgArchiveDocument,
  orgDownloadDocument,
  orgFetchDocument,
  orgListScopes,
  orgLogin,
  orgLogout,
  orgPreviewDocument,
  orgRemoveOwnSubmission,
  orgScanDocumentFolder,
  orgSession,
  orgSubmitDocument,
  orgSubmitMemoryCandidate,
  orgNewDocumentVersion,
  orgPublishDocument,
  orgPublishSkill,
  orgSetSkillPreference,
  orgSubmitSkill,
  orgSyncSkills,
  type OrgDocument,
  type OrgFolderFile,
  type OrgMemory,
  type OrgMemoryKind,
  type OrgScope,
  type OrgSession,
  type OrgSkill,
  type OrgLibraryResource,
  type MemoryPromotion,
  type Submission,
} from "@/lib/org-client";
import { organizationSessionBoundary, useOrgSessionStore } from "@/stores/org-session-store";
import { filesystemPickDirectory, filesystemPickFiles } from "@/lib/agent-client";
import { FilePreview } from "./FilePreview";
import { Button } from "./Button";
import { CapabilityPage } from "./experts-panel/shared/CapabilityPage";
import organizationHeroImg from "@/assets/organization-hero.png";
import { PageToolbar } from "./PageToolbar";
import { ActionMenu } from "./ActionMenu";
import { useOrganizationLibrary } from "./organization/use-organization-library";
import { useModalFocus } from "@/lib/use-modal-focus";

type Tab = "overview" | "memories" | "documents" | "skills";

function OrganizationError({ reason, onRetry, busy = false, onDismiss }: {
  reason: string; onRetry?: () => void; busy?: boolean; onDismiss?: () => void;
}) {
  const connection = /error sending request|fetch|network|connect|timed?\s*out|timeout|dns|certificate|tls|ssl/i.test(reason);
  const expired = /\b401\b|unauthorized|token.*expired/i.test(reason);
  const message = connection ? "暂时无法连接组织服务，请检查网络和服务地址后重试。"
    : expired ? "组织登录状态已过期，请重新登录。"
      : /[\u4e00-\u9fff]/.test(reason) && !/https?:\/\//.test(reason) ? reason.replace(/^Error:\s*/, "").slice(0, 200)
        : "组织操作未完成，请重试或查看技术详情。";
  return <div className="org-memory__error org-connection-error" role="alert">
    <XCircle size={16} aria-hidden="true" /><div className="org-connection-error__copy"><p>{message}</p>
      {reason.replace(/^Error:\s*/, "") !== message && <details><summary>技术详情</summary><pre>{reason}</pre></details>}
    </div>
    {onRetry && <Button onClick={onRetry} disabled={busy}>{busy ? "重试中…" : "重试"}</Button>}
    {onDismiss && <button type="button" aria-label="关闭错误提示" onClick={onDismiss}><X size={16} /></button>}
  </div>;
}

const memoryKindLabel: Record<OrgMemoryKind, string> = {
  fact: "事实",
  decision: "决策",
  convention: "规范",
  pitfall: "问题处理经验",
  howto: "操作手册",
};

const scopeLabel = (kind: OrgScope["kind"]) =>
  kind === "personal" ? "仅自己" : kind === "team" ? "团队" : "全组织";

const stateLabel = (state: string) => ({
  pending: "待审核",
  approved: "已通过",
  rejected: "已拒绝",
  withdrawn: "已撤回",
  revoked: "已撤销",
  queued: "排队中",
  scanning: "扫描中",
  passed: "通过",
  processing: "解析中",
  parsing: "解析中",
  chunking: "切分中",
  embedding: "建立索引",
  ready: "可检索",
  failed: "处理失败",
  archived: "已归档",
}[state] ?? state);

const readableBytes = (bytes: number) => {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
};

const submissionIsActive = (submission: Submission) => (
  submission.state === "pending"
  || submission.scanStatus === "queued"
  || submission.scanStatus === "scanning"
);

const documentSubmissionLabel = (submission: Submission) => {
  if (submission.state === "approved") return "已发布";
  if (submission.scanStatus === "scanning") return "安全扫描中";
  if (submission.scanStatus === "queued") return "等待安全扫描";
  if (submission.state === "pending") return "历史待处理";
  return stateLabel(submission.state);
};

const documentIsProcessing = (document: OrgDocument) => (
  ["pending", "parsing", "chunking", "embedding"].includes(document.status)
);

async function settleWithConcurrency<T, I>(
  items: I[],
  limit: number,
  task: (item: I) => Promise<T>,
  onProgress: (completed: number) => void,
): Promise<PromiseSettledResult<T>[]> {
  const results = new Array<PromiseSettledResult<T>>(items.length);
  let cursor = 0;
  let completed = 0;
  const worker = async () => {
    while (cursor < items.length) {
      const index = cursor++;
      try {
        results[index] = { status: "fulfilled", value: await task(items[index]) };
      } catch (reason) {
        results[index] = { status: "rejected", reason };
      } finally {
        completed += 1;
        onProgress(completed);
      }
    }
  };
  await Promise.all(
    Array.from({ length: Math.min(limit, items.length) }, () => worker()),
  );
  return results;
}

function ScopeIcon({ kind }: { kind: OrgScope["kind"] }) {
  if (kind === "personal") return <UserRound size={14} />;
  if (kind === "team") return <UsersRound size={14} />;
  return <Building2 size={14} />;
}

export function OrganizationMemoryPanel({
  onToast,
  cwd,
  onStartConversation,
}: {
  onToast?: (message: string) => void;
  cwd?: string;
  onStartConversation?: () => void;
}) {
  const library = useOrganizationLibrary();
  const { pages, queries, summary: librarySummary } = library;
  const documents = pages.documents.items;
  const memories = pages.memories.items;
  const skills = pages.skills.items;
  const memoryPromotions = pages["memory-submissions"].items;
  const documentSubmissions = pages["document-submissions"].items;
  const skillSubmissions = pages["skill-submissions"].items;
  const [resourceViews, setResourceViews] = useState<Record<string, "published" | "mine">>({});
  const contentRef = useRef<HTMLDivElement>(null);
  const scrollPositions = useRef<Record<string, number>>({});
  const [actionInvoker, setActionInvoker] = useState<HTMLElement | null>(null);
  const [memoryDetail, setMemoryDetail] = useState<OrgMemory | null>(null);
  const memoryDetailRef = useModalFocus<HTMLDivElement>(!!memoryDetail, () => setMemoryDetail(null));
  const [skillDialog, setSkillDialog] = useState<{ kind: "upload" } | { kind: "publish"; skill: OrgSkill } | null>(null);
  const skillDialogRef = useModalFocus<HTMLDivElement>(!!skillDialog, () => { if (!busyRef.current) setSkillDialog(null); }, actionInvoker);
  const [session, setSession] = useState<OrgSession | null>(null);
  const mirroredSession = useOrgSessionStore((state) => state.session);
  const mirrorOrgSession = useOrgSessionStore((state) => state.setSession);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const busyRef = useRef(busy);
  busyRef.current = busy;
  const [uploadProgress, setUploadProgress] = useState<{
    label: string;
    completed: number;
    total: number;
  } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [workspaceLoaded, setWorkspaceLoaded] = useState(false);
  const [workspaceUnavailable, setWorkspaceUnavailable] = useState(false);
  const [serverUrl, setServerUrl] = useState("https://");
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [tab, setTab] = useState<Tab>("overview");
  const [scopes, setScopes] = useState<OrgScope[]>([]);
  const [writeScope, setWriteScope] = useState("");
  const [publishScope, setPublishScope] = useState("");
  const [folderBatch, setFolderBatch] = useState<{ folder: string; files: OrgFolderFile[]; skipped: number; totalBytes: number; scopeId: string; scopeName: string } | null>(null);
  const folderBatchRef = useModalFocus<HTMLDivElement>(!!folderBatch, () => { if (!busyRef.current) setFolderBatch(null); });
  const [uploadIntent, setUploadIntent] = useState<"files" | "folder" | null>(null);
  const uploadDialogRef = useModalFocus<HTMLDivElement>(Boolean(uploadIntent), () => setUploadIntent(null));
  const [preview, setPreview] = useState<{ name: string; content: string; parsed: boolean; docId?: string; nextChunk?: number; more?: boolean; resourceId: string; submission: boolean; sourceType: string; unavailableReason?: string } | null>(null);
  const [previewLoading, setPreviewLoading] = useState(false);
  const [removeTarget, setRemoveTarget] = useState<{ id: string; name: string; submission: boolean } | null>(null);
  const [publishDocumentTarget, setPublishDocumentTarget] = useState<OrgDocument | null>(null);
  const previewDialogRef = useModalFocus<HTMLDivElement>(Boolean(preview), () => closePreview());
  const closePublishDialog = () => { if (!busy) setPublishDocumentTarget(null); };
  const closeRemoveDialog = () => { if (!busy) setRemoveTarget(null); };
  const publishDialogRef = useModalFocus<HTMLDivElement>(Boolean(publishDocumentTarget), closePublishDialog, actionInvoker);
  const removeDialogRef = useModalFocus<HTMLDivElement>(Boolean(removeTarget), closeRemoveDialog, actionInvoker);
  const [showMemoryForm, setShowMemoryForm] = useState(false);
  const memoryFormRef = useModalFocus<HTMLDivElement>(showMemoryForm, () => { if (!busyRef.current) setShowMemoryForm(false); });
  const [memoryKind, setMemoryKind] = useState<OrgMemoryKind>("howto");
  const [memoryContent, setMemoryContent] = useState("");
  const [memoryRationale, setMemoryRationale] = useState("");
  const [memoryOutcome, setMemoryOutcome] = useState("");
  const [memoryWorkspace, setMemoryWorkspace] = useState("");
  const [memoryValidUntil, setMemoryValidUntil] = useState("");
  const workspaceGeneration = useRef(0);
  const workspaceLoad = useRef<{ boundary: number; promise: Promise<void> } | null>(null);
  const previewGeneration = useRef(0);
  const currentBoundary = () => useOrgSessionStore.getState().boundaryRevision;
  const isCurrentBoundary = (revision: number) => currentBoundary() === revision;
  const closePreview = () => {
    previewGeneration.current += 1;
    setPreview(null);
    setPreviewLoading(false);
  };

  const clearWorkspace = useCallback(() => {
    setWorkspaceLoaded(false);
    setWorkspaceUnavailable(false);
    workspaceGeneration.current += 1;
    workspaceLoad.current = null;
    previewGeneration.current += 1;
    setScopes([]);
    library.clear();
    setResourceViews({});
    scrollPositions.current = {};
    setMemoryDetail(null);
    setSkillDialog(null);
    setActionInvoker(null);
    setWriteScope("");
    setPublishScope("");
    setFolderBatch(null);
    setUploadIntent(null);
    setPreview(null);
    setPreviewLoading(false);
    setRemoveTarget(null);
    setPublishDocumentTarget(null);
    setShowMemoryForm(false);
    setMemoryContent("");
    setMemoryRationale("");
    setMemoryOutcome("");
    setMemoryWorkspace("");
    setMemoryValidUntil("");
    setPassword("");
    setTab("overview");
  }, [library.clear]);

  useEffect(() => {
    if (mirroredSession) setSession(mirroredSession);
  }, [mirroredSession]);

  const loadWorkspace = useCallback((force = false): Promise<void> => {
    const boundary = useOrgSessionStore.getState().boundaryRevision;
    if (!force && workspaceLoad.current?.boundary === boundary) return workspaceLoad.current.promise;
    const generation = ++workspaceGeneration.current;
    const isCurrent = () => useOrgSessionStore.getState().boundaryRevision === boundary
      && workspaceGeneration.current === generation;
    const promise = (async () => {
      const nextScopes = await orgListScopes();
      if (!isCurrent()) return;
      await library.refresh();
      if (!isCurrent()) return;
      setScopes(nextScopes);
      setWriteScope((current) => nextScopes.some((scope) => scope.id === current && scope.canPublishDocuments !== false)
        ? current
        : nextScopes.find((scope) => scope.kind === "personal")?.id || nextScopes[0]?.id || "");
      setPublishScope((current) => nextScopes.some((scope) => scope.id === current && scope.kind !== "personal")
        ? current
        : nextScopes.find((scope) => scope.kind === "team")?.id || nextScopes.find((scope) => scope.kind === "org")?.id || "");
      setWorkspaceLoaded(true);
      setWorkspaceUnavailable(false);
    })().catch((reason) => {
      if (isCurrent()) setWorkspaceUnavailable(true);
      throw reason;
    }).finally(() => {
      if (workspaceLoad.current?.promise === promise) workspaceLoad.current = null;
    });
    workspaceLoad.current = { boundary, promise };
    return promise;
  }, [library.refresh]);

  useEffect(() => useOrgSessionStore.subscribe((next, previous) => {
    if (next.boundaryRevision === previous.boundaryRevision) return;
    clearWorkspace();
    if (!next.session?.loggedIn) setLoading(false);
    // Native login events arrive before the login IPC result. Every signed-in
    // boundary needs a workspace; concurrent callers share its current load.
    if (next.session?.loggedIn) {
      void loadWorkspace().catch((reason) => {
        if (useOrgSessionStore.getState().boundaryRevision === next.boundaryRevision) {
          setError(String(reason));
        }
      });
    }
  }), [clearWorkspace, loadWorkspace]);

  useEffect(() => () => {
    workspaceGeneration.current += 1;
    previewGeneration.current += 1;
  }, []);

  useEffect(() => {
    let alive = true;
    const boundary = currentBoundary();
    orgSession()
      .then(async (nextSession) => {
        if (!alive || !isCurrentBoundary(boundary)) return;
        setSession(nextSession);
        mirrorOrgSession(nextSession);
        if (nextSession.serverUrl) setServerUrl(nextSession.serverUrl);
        if (nextSession.username || nextSession.user?.username) {
          setUsername(nextSession.username ?? nextSession.user?.username ?? "");
        }
        if (nextSession.loggedIn) await loadWorkspace();
      })
      .catch((reason) => alive && isCurrentBoundary(boundary) && setError(String(reason)))
      .finally(() => alive && setLoading(false));
    return () => { alive = false; };
  }, [loadWorkspace, mirrorOrgSession]);

  useEffect(() => {
    if (!session?.loggedIn) return;
    const active = documentSubmissions.some(submissionIsActive)
      || skillSubmissions.some(submissionIsActive)
      || memoryPromotions.some((item) => item.state === "pending")
      || documents.some(documentIsProcessing)
      || !!librarySummary && (librarySummary.memoryPending + librarySummary.documentPending + librarySummary.skillPending + librarySummary.documentProcessing > 0);
    if (!active) return;
    let disposed = false;
    let refreshInFlight = false;
    const timer = window.setInterval(() => {
      if (refreshInFlight) return;
      refreshInFlight = true;
      void loadWorkspace()
        .catch((reason) => {
          if (!disposed) console.warn("[EchoAgent] Organization workspace polling failed:", reason);
        })
        .finally(() => { refreshInFlight = false; });
    }, 3000);
    return () => {
      disposed = true;
      window.clearInterval(timer);
    };
  }, [
    session?.loggedIn,
    documentSubmissions,
    documents,
    loadWorkspace,
    memoryPromotions,
    skillSubmissions,
    librarySummary,
  ]);

  const uploadScope = scopes.find((scope) => scope.id === writeScope);
  const allowSkillSubmission = session?.bootstrap?.policy.allowSkillSubmission !== false;
  const availableDocumentScopes = scopes.filter((scope) => scope.canPublishDocuments !== false
    && (scope.kind !== "personal" || session?.bootstrap?.policy.allowPersonalCloud !== false));
  const allowDocumentUpload = !!uploadScope && availableDocumentScopes.some((scope) => scope.id === uploadScope.id);
  const handleLogin = async (event: React.FormEvent) => {
    event.preventDefault();
    const boundary = currentBoundary();
    setBusy(true);
    setError(null);
    try {
      const next = await orgLogin(serverUrl, username, password);
      if (!isCurrentBoundary(boundary)
        && organizationSessionBoundary(useOrgSessionStore.getState().session) !== organizationSessionBoundary(next)) return;
      setSession(next);
      mirrorOrgSession(next);
      setPassword("");
      await loadWorkspace();
    } catch (reason) {
      setError(String(reason).replace(/^Error:\s*/, ""));
    } finally {
      setBusy(false);
      setLoading(false);
    }
  };

  const refreshWorkspace = async () => {
    const boundary = currentBoundary();
    setBusy(true);
    setError(null);
    try {
      const nextSession = await orgSession();
      if (!isCurrentBoundary(boundary)) return;
      setSession(nextSession);
      mirrorOrgSession(nextSession);
      if (nextSession.serverUrl) setServerUrl(nextSession.serverUrl);
      if (nextSession.username || nextSession.user?.username) {
        setUsername(nextSession.username ?? nextSession.user?.username ?? "");
      }
      if (!nextSession.loggedIn) {
        resetWorkspace(nextSession);
        return;
      }
      await loadWorkspace(true);
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(false);
    }
  };

  const resetWorkspace = (signedOutSession: OrgSession = { loggedIn: false }) => {
    setSession(signedOutSession);
    mirrorOrgSession(signedOutSession);
    clearWorkspace();
  };

  const handleLogout = async () => {
    setBusy(true);
    setError(null);
    // Hide the previous user's workspace immediately. Native logout clears its
    // local session before making a short best-effort server revocation call.
    resetWorkspace({
      loggedIn: false,
      serverUrl: serverUrl.trim() || undefined,
      username: username.trim() || session?.user?.username,
      requiresReauthentication: false,
    });
    try {
      await orgLogout();
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(false);
    }
  };

  const pickAndUploadDocument = async () => {
    if (!uploadScope) return;
    const boundary = currentBoundary();
    setError(null);
    try {
      const paths = await filesystemPickFiles({
        multiple: true,
        maxFiles: 50,
        title: "选择要提交的可检索文档（可多选）",
        extensions: ["md", "txt", "pdf", "docx", "xlsx", "pptx", "png", "jpg", "jpeg"],
      });
      if (!isCurrentBoundary(boundary) || !paths || paths.length === 0) return;
      await uploadDocumentsInBatch(paths.map((path) => ({ path })), uploadScope.id);
    } catch (reason) {
      setError(`选择文档失败：${String(reason).replace(/^Error:\s*/, "")}`);
    }
  };

  const pickDocumentFolder = async () => {
    if (!uploadScope) return;
    const boundary = currentBoundary();
    setError(null);
    try {
      const folder = await filesystemPickDirectory();
      if (!isCurrentBoundary(boundary) || !folder) return;
      const result = await orgScanDocumentFolder(folder);
      if (!isCurrentBoundary(boundary)) return;
      if (result.items.length === 0) {
        setError("文件夹中没有可上传的文档（支持 MD、TXT、PDF、Office 文档和图片）");
        return;
      }
      setFolderBatch({ folder, files: result.items, skipped: result.skipped, totalBytes: result.totalBytes, scopeId: uploadScope.id, scopeName: uploadScope.name });
    } catch (reason) {
      setError(`扫描文件夹失败：${String(reason).replace(/^Error:\s*/, "")}`);
    }
  };

  const openDocumentUpload = (intent: "files" | "folder") => {
    if (!allowDocumentUpload && availableDocumentScopes[0]) setWriteScope(availableDocumentScopes[0].id);
    setUploadIntent(intent);
  };

  const startDocumentUpload = async () => {
    const intent = uploadIntent;
    if (!intent || !allowDocumentUpload) return;
    setUploadIntent(null);
    if (intent === "folder") await pickDocumentFolder();
    else await pickAndUploadDocument();
  };

  const uploadDocumentsInBatch = async (files: Array<{ path: string; title?: string }>, scopeId: string) => {
    const boundary = currentBoundary();
    setBusy(true);
    setError(null);
    setUploadProgress({ label: "正在上传文档", completed: 0, total: files.length });
    let batchError: string | null = null;
    try {
      const settled = await settleWithConcurrency(
        files,
        3,
        (file) => {
          if (!isCurrentBoundary(boundary)) throw new Error("组织账号或权限已改变，已停止后续上传");
          return orgSubmitDocument(file.path, scopeId, file.title);
        },
        (completed) => isCurrentBoundary(boundary) && setUploadProgress({
          label: "正在上传文档",
          completed,
          total: files.length,
        }),
      );
      if (!isCurrentBoundary(boundary)) return;
      const failures: string[] = [];
      let done = 0;
      let skipped = 0;
      settled.forEach((result, index) => {
        if (result.status === "fulfilled") {
          if (result.value.dedup || result.value.state === "duplicate") skipped += 1;
          else done += 1;
        }
        else failures.push(`${files[index].title ?? files[index].path}: ${String(result.reason).replace(/^Error:\s*/, "")}`);
      });
      batchError = failures.length > 0 ? failures.slice(0, 3).join("\n") : null;
      setError(batchError);
      if (done > 0 || skipped > 0) {
        onToast?.(`新增 ${done} 个文档，跳过重复 ${skipped} 个${failures.length ? `，失败 ${failures.length} 个` : ""}${done ? "；正在处理文档" : ""}`);
      } else {
        onToast?.(`上传失败：${failures[0] ?? "未知原因"}`);
      }
      setUploadProgress({ label: "正在刷新列表", completed: files.length, total: files.length });
      await loadWorkspace(true);
      setFolderBatch(null);
    } catch (reason) {
      const refreshError = `刷新列表失败：${String(reason).replace(/^Error:\s*/, "")}`;
      setError(batchError ? `${batchError}\n${refreshError}` : refreshError);
    } finally {
      setUploadProgress(null);
      setBusy(false);
    }
  };

  const showDocumentPreview = async (id: string, name: string, sourceType: string, submission: boolean, publishedDocId?: string | null) => {
    const boundary = currentBoundary();
    const generation = ++previewGeneration.current;
    const isCurrent = () => isCurrentBoundary(boundary) && previewGeneration.current === generation;
    setPreviewLoading(true);
    setPreview(null);
    setError(null);
    const parsedDocId = submission ? publishedDocId : id;
    const officeDocument = ["docx", "xlsx", "pptx"].includes(sourceType.toLowerCase());
    try {
      if (officeDocument && parsedDocId) {
        const parsed = await orgFetchDocument(parsedDocId, null, "0:199");
        if (!isCurrent()) return;
        if (parsed.chunks.length > 0) {
          setPreview({ name, content: parsed.text, parsed: true, docId: parsedDocId, nextChunk: (parsed.nextSeq ?? 199) + 1, more: parsed.hasMore ?? parsed.chunks.length === 200, resourceId: id, submission, sourceType });
          return;
        }
      }
      if (officeDocument) {
        setPreview({ name, content: "", parsed: false, resourceId: id, submission, sourceType, unavailableReason: "文档内容尚未完成解析。可先下载原件查看，解析完成后再在线查看。" });
        return;
      }
      const result = await orgPreviewDocument(id, submission, sourceType, name);
      if (!isCurrent()) return;
      setPreview({ name, content: result.content, parsed: false, resourceId: id, submission, sourceType });
    } catch (reason) {
      if (!isCurrent()) return;
      if (officeDocument) {
        setPreview({ name, content: "", parsed: false, resourceId: id, submission, sourceType, unavailableReason: "暂时无法读取文档内容。可下载原件在本地查看，稍后重试在线预览。" });
        return;
      }
      if (parsedDocId && !officeDocument) {
        try {
          const parsed = await orgFetchDocument(parsedDocId, null, "0:199");
          if (!isCurrent()) return;
          if (parsed.chunks.length === 0) throw new Error("尚无解析内容");
          setPreview({ name, content: parsed.text, parsed: true, docId: parsedDocId, nextChunk: (parsed.nextSeq ?? 199) + 1, more: parsed.hasMore ?? parsed.chunks.length === 200, resourceId: id, submission, sourceType });
          return;
        } catch { /* Show the original preview error. */ }
      }
      if (isCurrent()) setError(`无法在线预览文档：${String(reason).replace(/^Error:\s*/, "")}。可下载原件在本地查看。`);
    } finally { if (isCurrent()) setPreviewLoading(false); }
  };

  const loadMorePreview = async () => {
    if (!preview?.docId || preview.nextChunk === undefined) return;
    const boundary = currentBoundary();
    const generation = previewGeneration.current;
    const docId = preview.docId;
    setPreviewLoading(true);
    try {
      const start = preview.nextChunk;
      const parsed = await orgFetchDocument(preview.docId, null, `${start}:${start + 199}`);
      if (!isCurrentBoundary(boundary) || previewGeneration.current !== generation) return;
      setPreview((current) => current?.docId === docId ? {
        ...current,
        content: `${current.content}\n\n${parsed.text}`,
        nextChunk: (parsed.nextSeq ?? start + 199) + 1,
        more: parsed.hasMore ?? parsed.chunks.length === 200,
      } : current);
    } catch (reason) {
      if (isCurrentBoundary(boundary) && previewGeneration.current === generation) setError(String(reason));
    } finally {
      if (isCurrentBoundary(boundary) && previewGeneration.current === generation) setPreviewLoading(false);
    }
  };

  const downloadDocument = async (id: string, name: string, submission: boolean) => {
    setError(null);
    try {
      const saved = await orgDownloadDocument(id, submission, name);
      if (saved) onToast?.(`已下载“${saved.split(/[\\/]/).pop()}”`);
    } catch (reason) { setError(`下载失败：${String(reason).replace(/^Error:\s*/, "")}`); }
  };

  const removeOwnDocument = async () => {
    if (!removeTarget) return;
    setBusy(true);
    setError(null);
    try {
      if (removeTarget.submission) await orgRemoveOwnSubmission(removeTarget.id);
      else await orgArchiveDocument(removeTarget.id);
      setRemoveTarget(null);
      setPreview(null);
      onToast?.("已停止共享此文档，服务器上的原件仍保留");
      await loadWorkspace(true);
    } catch (reason) { setError(String(reason)); } finally { setBusy(false); }
  };

  const submitMemoryCandidate = async (event: React.FormEvent) => {
    event.preventDefault();
    if (!writeScope || !memoryContent.trim()) return;
    setBusy(true);
    setError(null);
    try {
      await orgSubmitMemoryCandidate({
        targetScopeId: writeScope,
        kind: memoryKind,
        content: memoryContent.trim(),
        rationale: memoryRationale.trim(),
        outcome: memoryOutcome.trim(),
        workspaceRef: memoryWorkspace.trim(),
        validUntil: memoryValidUntil
          ? new Date(`${memoryValidUntil}T23:59:59`).getTime()
          : undefined,
      });
      setMemoryContent("");
      setMemoryRationale("");
      setMemoryOutcome("");
      setMemoryWorkspace("");
      setMemoryValidUntil("");
      setShowMemoryForm(false);
      onToast?.("经验已提交，审核通过后可供团队使用");
      await loadWorkspace(true);
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(false);
    }
  };

  const pickAndUploadSkill = async () => {
    if (!uploadScope) return;
    const boundary = currentBoundary();
    setError(null);
    let paths: string[];
    try {
      paths = await filesystemPickFiles({
        multiple: true,
        maxFiles: 50,
        title: "选择要提交的 Skill ZIP（可多选）",
        extensions: ["zip"],
      });
    } catch (reason) {
      setError(`选择 Skill ZIP 失败：${String(reason).replace(/^Error:\s*/, "")}`);
      return;
    }
    if (!isCurrentBoundary(boundary) || !paths || paths.length === 0) return;
    setBusy(true);
    setUploadProgress({ label: "正在上传 Skill", completed: 0, total: paths.length });
    let batchError: string | null = null;
    try {
      const settled = await settleWithConcurrency(
        paths,
        3,
        (path) => {
          if (!isCurrentBoundary(boundary)) throw new Error("组织账号或权限已改变，已停止后续上传");
          return orgSubmitSkill(path, uploadScope.id);
        },
        (completed) => isCurrentBoundary(boundary) && setUploadProgress({
          label: "正在上传 Skill",
          completed,
          total: paths.length,
        }),
      );
      if (!isCurrentBoundary(boundary)) return;
      const failures: string[] = [];
      let done = 0;
      settled.forEach((result, index) => {
        if (result.status === "fulfilled") done += 1;
        else failures.push(`${paths[index]}: ${String(result.reason).replace(/^Error:\s*/, "")}`);
      });
      batchError = failures.length > 0 ? failures.slice(0, 3).join("\n") : null;
      setError(batchError);
      if (done > 0) {
        onToast?.(
          failures.length > 0
            ? `已上传 ${done} 个 Skill，失败 ${failures.length} 个`
            : "Skill 已提交，等待组织审核",
        );
      } else {
        onToast?.(`技能上传失败：${failures[0] ?? "未知原因"}`);
      }
      setUploadProgress({ label: "正在刷新列表", completed: paths.length, total: paths.length });
      await loadWorkspace(true);
    } catch (reason) {
      const refreshError = `刷新列表失败：${String(reason).replace(/^Error:\s*/, "")}`;
      setError(batchError ? `${batchError}\n${refreshError}` : refreshError);
    } finally {
      setUploadProgress(null);
      setBusy(false);
    }
  };

  const uploadNewVersion = async (document: OrgDocument) => {
    const boundary = currentBoundary();
    const extensions = document.sourceType === "image" ? ["png", "jpg", "jpeg"] : [document.sourceType];
    let path: string | undefined;
    try {
      [path] = await filesystemPickFiles({
        multiple: false,
        title: "选择同类型新版本",
        extensions,
      });
    } catch (reason) {
      if (isCurrentBoundary(boundary)) setError(`选择新版本失败：${String(reason).replace(/^Error:\s*/, "")}`);
      return;
    }
    if (!isCurrentBoundary(boundary) || !path) return;
    setBusy(true);
    try {
      await orgNewDocumentVersion(document.id, path);
      onToast?.("新版本已上传，处理完成后会自动替换旧版本");
      await loadWorkspace(true);
    } catch (reason) { setError(String(reason)); } finally { setBusy(false); }
  };

  const publishDocument = async (document: OrgDocument) => {
    if (!publishScope) return;
    setBusy(true);
    try {
      const result = await orgPublishDocument(document.id, publishScope);
      onToast?.(result.state === "pending" ? "已生成副本，等待安全扫描" : "文档副本已发布");
      setPublishDocumentTarget(null);
      await loadWorkspace(true);
    } catch (reason) { setError(String(reason)); } finally { setBusy(false); }
  };

  const toggleSkill = async (skill: OrgSkill) => {
    setBusy(true);
    try {
      await orgSetSkillPreference(skill.skillId, !skill.enabled);
      await loadWorkspace(true);
    } catch (reason) { setError(String(reason)); } finally { setBusy(false); }
  };

  const publishSkill = async (skill: OrgSkill) => {
    if (!publishScope) return;
    setBusy(true);
    try {
      const result = await orgPublishSkill(skill.skillId, publishScope);
      onToast?.(result.state === "pending" ? "技能副本已提交审核" : "技能副本已发布");
      setSkillDialog(null);
      await loadWorkspace(true);
    } catch (reason) { setError(String(reason)); } finally { setBusy(false); }
  };

  const syncSkills = async () => {
    setBusy(true);
    setError(null);
    try {
      const result = await orgSyncSkills();
      onToast?.(`已更新 ${result.installed.length} 个技能`);
      await loadWorkspace(true);
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(false);
    }
  };

  const view = resourceViews[tab] ?? "published";
  const activeResource: OrgLibraryResource = tab === "memories" ? (view === "mine" ? "memory-submissions" : "memories")
    : tab === "skills" ? (view === "mine" ? "skill-submissions" : "skills") : (view === "mine" ? "document-submissions" : "documents");
  const activeQuery = queries[activeResource];
  const activePage = pages[activeResource];
  const scrollKey = tab === "overview" ? "overview" : activeResource;
  const updateQuery = (patch: Partial<typeof activeQuery>) => {
    scrollPositions.current[scrollKey] = 0;
    library.update(activeResource, patch);
  };
  useLayoutEffect(() => {
    if (contentRef.current) contentRef.current.scrollTop = scrollPositions.current[scrollKey] ?? 0;
  }, [scrollKey, activeQuery.page, activeQuery.size, activeQuery.scopeId, activeQuery.query, activeQuery.filter, workspaceLoaded]);

  if (loading) {
    return (
      <CapabilityPage title="组织" headingLevel={1} className="organization-loading">
        <div className="organization-loading__status" role="status">
          <Loader2 className="org-memory__spin" size={20} aria-hidden="true" />
          <span>正在连接组织服务…</span>
        </div>
      </CapabilityPage>
    );
  }

  if (!session?.loggedIn) {
    return (
      <div className="org-memory org-memory--login">
        <PageToolbar className="org-login-toolbar"><span className="capability-page__title">组织</span></PageToolbar>
        <div className="org-login-card">
          <div className="org-login-card__mark"><Building2 size={28} /></div>
          <h1>连接组织</h1>
          <p>连接组织服务后，可在授权范围内共享文档、技能和工作经验，为任务执行提供参考。</p>
          <form onSubmit={handleLogin}>
            {session?.requiresReauthentication && (
              <div className="org-memory__reauth" role="status">
                <AlertTriangle size={15} />
                <span>登录状态已过期，服务器和账号已为你保留，请重新输入密码。</span>
              </div>
            )}
            <label>服务器地址<input value={serverUrl} onChange={(event) => setServerUrl(event.target.value)} placeholder="https://memory.company.com" required /></label>
            <label>账号<input value={username} onChange={(event) => setUsername(event.target.value)} autoComplete="username" required /></label>
            <label>密码<input value={password} onChange={(event) => setPassword(event.target.value)} type="password" autoComplete="current-password" autoFocus={session?.requiresReauthentication === true} required /></label>
            {error && <OrganizationError reason={error} />}
            <button className="org-memory__primary" disabled={busy}>{busy && <Loader2 className="org-memory__spin" size={15} />}安全登录</button>
          </form>
          <div className="org-login-card__security"><LockKeyhole size={14} />密码不会保存；自动续期凭据由系统安全存储保护，访问令牌只留在内存中。</div>
        </div>
      </div>
    );
  }

  const pendingMemoryCount = librarySummary?.memoryPending ?? 0;
  const pendingDocumentCount = (librarySummary?.documentPending ?? 0) + (librarySummary?.documentProcessing ?? 0);
  const pendingSkillCount = librarySummary?.skillPending ?? 0;
  const pendingCount = pendingMemoryCount + pendingDocumentCount + pendingSkillCount;
  const pageTitle = tab === "overview" ? "组织知识" : tab === "memories" ? "经验" : tab === "documents" ? "文档" : "组织 Skills";
  const pageDescription = tab === "overview" ? "让团队经验、文档和技能服务于每个任务。"
    : tab === "memories" ? "沉淀可复用的决策、规范和处理经验。"
      : tab === "documents" ? "查找、阅读和管理授权范围内的文档。" : "发现组织共享技能，安装到当前设备使用。";
  const orgName = session.bootstrap?.scopes.find(scope => scope.kind === "org")?.name ?? "组织";
  const identity = session.user?.displayName || session.user?.username || session.username || "组织账号";
  const capability = session.organizationMemoryEnabled ? "组织知识已启用"
    : session.bootstrap?.scopes.some(scope => scope.kind !== "personal") ? "组织知识暂不可用" : "暂无团队或组织共享权限";
  const navigatePending = (next: Tab) => {
    setTab(next);
    setResourceViews(previous => ({ ...previous, [next]: "mine" }));
  };

  return (
    <section className="org-memory org-workspace" aria-label="组织">
      <PageToolbar className="org-workspace__topbar">
        <div className="org-workspace__brand"><Building2 size={20} aria-hidden="true" /><h1>{orgName}</h1></div>
        <span className={`org-workspace__connection ${session.organizationMemoryEnabled ? "is-active" : ""}`} role="status"><i />{capability}</span>
        <ActionMenu label={`组织账号：${identity}`} menuLabel="组织账号" triggerClassName="org-account-trigger" resetKey={session.user?.id}
          trigger={<><UserRound size={16} aria-hidden="true" /><span>{identity}</span><ChevronDown size={14} aria-hidden="true" /></>}>
          {({ close }) => <><div className="org-account-details"><strong>{identity}</strong><span>{session.serverUrl}</span><small>退出登录不会退出组织成员关系</small></div>
            <button type="button" role="menuitem" className="topbar-actions__item" disabled={busy} onClick={() => { close(); void handleLogout(); }}><LogOut size={15} />退出登录</button></>}
        </ActionMenu>
      </PageToolbar>
      <div className="org-workspace__body">
        <nav className="org-memory__nav" aria-label="组织功能">
          {([["overview", "概览", LayoutDashboard], ["memories", "经验", BrainCircuit], ["documents", "文档", FileText], ["skills", "Skills", PackageOpen]] as const).map(([key, label, Icon]) =>
            <button type="button" key={key} className={tab === key ? "active" : ""} aria-current={tab === key ? "page" : undefined} onClick={() => setTab(key)}><Icon size={17} aria-hidden="true" /><span>{label}</span>{" "}{key !== "overview" && <em>{workspaceLoaded ? librarySummary?.[key] ?? "—" : "—"}</em>}</button>)}
        </nav>
        <main className="org-memory__content">
          <header className="org-workspace__page-header">
            <div><h2>{pageTitle}</h2><p>{pageDescription}</p></div>
            <div className="org-workspace__page-actions">
              {!workspaceUnavailable && <button type="button" className="org-icon-button" aria-label="刷新" title="更新组织资料" disabled={busy} onClick={() => void refreshWorkspace()}><RefreshCw size={16} className={busy ? "org-memory__spin" : undefined} /></button>}
              {tab === "memories" && <button className="org-memory__primary" disabled={busy || !writeScope} onClick={() => { setMemoryWorkspace(value => value || cwd || ""); setShowMemoryForm(true); }}><Plus size={15} />提交经验</button>}
              {tab === "documents" && <div className="org-upload-button"><button className="org-memory__primary" onClick={() => openDocumentUpload("files")} disabled={busy || availableDocumentScopes.length === 0}><Upload size={15} />上传文档</button>
                <ActionMenu label="更多上传方式" menuLabel="上传方式" busy={busy || availableDocumentScopes.length === 0} triggerClassName="org-memory__primary" trigger={<ChevronDown size={15} />}>
                  {({ close }) => <button type="button" role="menuitem" className="topbar-actions__item" onClick={() => { close(); openDocumentUpload("folder"); }}><FolderUp size={15} />上传文件夹</button>}
                </ActionMenu></div>}
              {tab === "skills" && <><button type="button" className="echo-button" onClick={() => void syncSkills()} disabled={busy}><RefreshCw size={15} />同步技能</button><button className="org-memory__primary" onClick={() => { setActionInvoker(null); setSkillDialog({ kind: "upload" }); }} disabled={busy || !uploadScope || !allowSkillSubmission}><Upload size={15} />提交 Skill</button></>}
            </div>
          </header>
          {tab !== "overview" && <div className="org-resource-toolbar">
            <div className="org-resource-views" role="group" aria-label="资源视图"><button type="button" aria-pressed={view === "published"} onClick={() => setResourceViews(previous => ({ ...previous, [tab]: "published" }))}>已发布</button><button type="button" aria-pressed={view === "mine"} onClick={() => setResourceViews(previous => ({ ...previous, [tab]: "mine" }))}>我的提交</button></div>
            <div className="org-resource-filters">
              <div className="org-resource-search"><Search size={16} aria-hidden="true" /><input type="search" aria-label={`搜索${pageTitle}`} placeholder={view === "mine" ? "搜索我的提交" : `搜索${tab === "skills" ? "技能" : pageTitle}`} maxLength={2000} value={activeQuery.query} onChange={event => updateQuery({ query: event.target.value })} />{activeQuery.query && <button type="button" aria-label="清空搜索" onClick={() => updateQuery({ query: "" })}><X size={14} /></button>}</div>
              <select aria-label="查看范围" value={activeQuery.scopeId} onChange={event => updateQuery({ scopeId: event.target.value })}><option value="">全部授权范围</option>{scopes.map(scope => <option key={scope.id} value={scope.id}>{scopeLabel(scope.kind)} · {scope.name}</option>)}</select>
              <select aria-label={view === "mine" ? "提交状态" : tab === "memories" ? "经验类型" : tab === "documents" ? "文档状态" : "安装状态"} value={activeQuery.filter} onChange={event => updateQuery({ filter: event.target.value })}>
                <option value="">{view === "mine" ? "全部状态" : tab === "memories" ? "全部类型" : tab === "documents" ? "全部状态" : "全部技能"}</option>
                {(view === "mine" ? [["pending", "待处理"], ["approved", "已通过"], ["rejected", "已拒绝"], ...(tab === "skills" ? [["revoked", "已撤销"]] : [["withdrawn", "已撤回"]])] : tab === "memories" ? Object.entries(memoryKindLabel) : tab === "documents" ? [["ready", "可检索"], ["processing", "处理中"], ["failed", "处理失败"]] : [["installed", "已安装"], ["available", "未安装"], ["mandatory", "组织必装"]]).map(([value, label]) => <option key={value} value={value}>{label}</option>)}
              </select>
              <span className="org-resource-count" role="status">{activePage.loading ? "正在更新…" : activePage.error ? "更新失败" : `共 ${activePage.total} 项`}</span>
            </div>
          </div>}
          <div className="org-resource-scroll" ref={contentRef} tabIndex={0} aria-label={`${pageTitle}资源区域`} onScroll={event => { scrollPositions.current[scrollKey] = event.currentTarget.scrollTop; }}>
            {error && !showMemoryForm && !skillDialog && !folderBatch && <OrganizationError reason={error} busy={busy} onRetry={workspaceUnavailable || !workspaceLoaded ? () => void refreshWorkspace() : undefined} onDismiss={() => setError(null)} />}
            {workspaceUnavailable && workspaceLoaded && <p className="org-workspace-status" role="status">组织资料更新失败，下方保留上次加载的内容。</p>}
            {uploadProgress && <div className="org-memory__upload-progress" role="status"><Loader2 className="org-memory__spin" size={15} /><span>{uploadProgress.label}</span><strong>{uploadProgress.completed}/{uploadProgress.total}</strong></div>}
            {tab !== "overview" && activePage.error && !workspaceUnavailable && <OrganizationError reason={activePage.error} onRetry={() => library.retry(activeResource)} />}
            {!workspaceLoaded && <p className="org-workspace-status" role="status">组织资料暂不可用，恢复连接后刷新查看。</p>}
            {tab === "overview" && <section className="org-overview">
              <div className="org-workspace__welcome"><div><h3>组织知识管理中心</h3><p>集中管理组织经验、文档和技能，让团队知识助力每个任务。</p><span>{capability}</span></div><img src={organizationHeroImg} width={2172} height={724} alt="组织协作与知识共享插画" draggable={false} /></div>
              <div className="org-overview__hero"><div className="org-overview__hero-icon"><MessageSquareText size={22} /></div><div><h3>在对话中使用组织知识</h3><p>发起任务时自动选择组织知识，可连续追问，也可以让 Agent 结合组织规则直接执行。</p></div><button className="org-memory__primary" onClick={onStartConversation} disabled={!session.organizationMemoryEnabled || !onStartConversation}>发起对话</button></div>
              {workspaceLoaded && librarySummary && <><div className="org-overview__metrics" aria-label="组织知识概况"><button type="button" onClick={() => setTab("memories")}><span>已发布经验</span><strong>{librarySummary.memories}</strong><small>{pendingMemoryCount} 条待审核</small></button><button type="button" onClick={() => setTab("documents")}><span>可检索文档</span><strong>{librarySummary.documentReady}</strong><small>{librarySummary.documentProcessing} 条处理中 · 共 {librarySummary.documents} 份文档</small></button><button type="button" onClick={() => setTab("skills")}><span>已安装 Skills</span><strong>{librarySummary.skillInstalled}/{librarySummary.skills}</strong><small>{librarySummary.skillMandatory} 个组织必装</small></button></div>
                <div className="org-overview__attention"><div><h3>需要关注</h3><p>查看提交审核和文档处理进度。</p></div>{pendingCount === 0 && <div className="org-library__empty">当前没有待处理事项</div>}{pendingMemoryCount > 0 && <div className="org-overview__attention-row"><AlertTriangle size={17} /><span>{pendingMemoryCount} 条经验待审核</span><button onClick={() => navigatePending("memories")}>查看经验</button></div>}{pendingDocumentCount > 0 && <div className="org-overview__attention-row"><AlertTriangle size={17} /><span>{pendingDocumentCount} 项文档正在扫描或建立索引</span><button onClick={() => { if (librarySummary.documentPending > 0) navigatePending("documents"); else { setTab("documents"); setResourceViews(previous => ({ ...previous, documents: "published" })); library.update("documents", { filter: "processing" }); } }}>查看文档</button></div>}{pendingSkillCount > 0 && <div className="org-overview__attention-row"><AlertTriangle size={17} /><span>{pendingSkillCount} 个 Skill 正在审核或扫描</span><button onClick={() => navigatePending("skills")}>查看 Skills</button></div>}</div></>}
            </section>}
            {workspaceLoaded && tab !== "overview" && <div className={`org-resource-results${activePage.loading ? " is-loading" : ""}`} aria-busy={activePage.loading}>
              {activePage.items.length === 0 && !activePage.loading && !activePage.error && <div className="org-resource-empty"><PackageOpen size={28} /><strong>{activeQuery.query || activeQuery.scopeId || activeQuery.filter ? "没有找到匹配的内容" : view === "mine" ? "还没有提交记录" : "当前范围暂无已发布内容"}</strong><p>{activeQuery.query || activeQuery.scopeId || activeQuery.filter ? "试试其他关键词，或调整筛选条件。" : view === "mine" ? "提交后的审核与处理进度会显示在这里。" : "上传或提交后，通过处理的内容会显示在这里。"}</p>{(activeQuery.query || activeQuery.scopeId || activeQuery.filter) && <button className="echo-button" onClick={() => updateQuery({ query: "", scopeId: "", filter: "" })}>清空筛选</button>}</div>}
              {view === "published" && tab === "memories" && <div className="org-library__list">{memories.map(memory => <article className="org-library__row org-memory-row" key={memory.id}><div className="org-library__icon"><BrainCircuit size={18} /></div><div className="org-library__main"><span className="org-memory-kind">{memoryKindLabel[memory.kind]}</span><button type="button" className="org-memory-summary" onClick={() => setMemoryDetail(memory)}>{memory.content}</button><span>{memory.scopeName} · {memory.trust === "verified" ? "已验证" : "已审核"}{memory.workspaceRef ? ` · ${memory.workspaceRef}` : ""}</span></div><span className={`org-state org-state--${memory.stale ? "failed" : "ready"}`}>{memory.stale ? "已过期" : "可用于任务"}</span></article>)}</div>}
              {view === "published" && tab === "documents" && <div className="org-library__list">{documents.map(document => {
                const mayManage = document.ownerId === session.user?.id || session.user?.role === "curator" || session.user?.role === "admin";
                const canPublishCopy = document.scopeKind === "personal" && scopes.some(scope => scope.kind !== "personal" && scope.canPublishDocuments !== false);
                const fileName = document.title.match(/\.[A-Za-z0-9]{1,8}$/) ? document.title : `${document.title}.${document.sourceType === "image" ? "png" : document.sourceType}`;
                return <article className="org-library__row org-document-library__row" key={document.id}><div className="org-library__icon"><FileText size={19} /></div><div className="org-library__main"><strong title={document.title}>{document.title}</strong><span>{document.scopeName} · {document.sourceType.toUpperCase()} · {readableBytes(document.byteSize)}</span>{document.failReason && <em>{document.failReason}</em>}</div><span className={`org-state org-state--${document.status}`}>{stateLabel(document.status)}</span><div className="org-library__actions">{document.status === "ready" && <><button onClick={() => void showDocumentPreview(document.id, fileName, document.sourceType, false)} disabled={previewLoading}><Eye size={14} />查看</button><button onClick={() => void downloadDocument(document.id, fileName, false)}><Download size={14} />下载</button></>}{(mayManage || canPublishCopy) && <ActionMenu label={`更多文档操作：${document.title}`} menuLabel="文档操作" resetKey={document.id} busy={busy} triggerClassName="org-icon-button" trigger={<MoreHorizontal size={18} />}>{({ close, triggerRef }) => <>{canPublishCopy && <button role="menuitem" disabled={document.status !== "ready"} className="topbar-actions__item" onClick={() => { setActionInvoker(triggerRef.current); close(); setPublishDocumentTarget(document); }}>发布副本</button>}{mayManage && <><button role="menuitem" className="topbar-actions__item" disabled={document.status !== "ready"} onClick={() => { close(true); void uploadNewVersion(document); }}><History size={15} />新版本</button><button role="menuitem" className="topbar-actions__item" disabled={document.status !== "ready"} onClick={() => { setActionInvoker(triggerRef.current); close(); setRemoveTarget({ id: document.id, name: document.title, submission: false }); }}><Archive size={15} />归档</button></>}</>}</ActionMenu>}</div></article>;
              })}</div>}
              {view === "published" && tab === "skills" && <div className="org-skill-grid">{skills.map(skill => <article className="org-skill-card" key={skill.skillId}><div><strong>{skill.name}</strong><span>v{skill.version}</span></div><p>{skill.description}</p><div className="org-skill-card__meta"><span className={`org-scope-badge org-scope-badge--${skill.scopeKind}`}><ScopeIcon kind={skill.scopeKind} />{skill.scopeName}</span><span className={`org-state org-state--${skill.enabled ? "ready" : "pending"}`}>{skill.mandatory ? "组织必装" : skill.enabled ? "已安装" : "未安装"}</span></div><footer>{skill.mandatory ? <span className="org-skill-card__managed"><LockKeyhole size={13} />由组织管理</span> : <button className={skill.enabled ? "echo-button" : "org-memory__primary"} disabled={busy} onClick={() => void toggleSkill(skill)}>{skill.enabled ? "卸载" : "安装"}</button>}{skill.scopeKind === "personal" && scopes.some(scope => scope.kind !== "personal") && <button className="org-skill-card__publish" disabled={busy} onClick={() => { setActionInvoker(null); setSkillDialog({ kind: "publish", skill }); }}>发布副本</button>}</footer></article>)}</div>}
              {view === "mine" && tab === "memories" && <MemoryPromotionList promotions={memoryPromotions} />}
              {view === "mine" && tab === "skills" && <SubmissionList title="我的 Skill 提交" submissions={skillSubmissions} />}
              {view === "mine" && tab === "documents" && <div className="org-document-submissions">{documentSubmissions.map(submission => {
                const id = submission.id ?? submission.submissionId ?? "";
                const name = submission.title ?? submission.name ?? "未命名文档";
                const fileName = name.match(/\.[A-Za-z0-9]{1,8}$/) ? name : `${name}.${submission.sourceType === "image" ? "png" : submission.sourceType ?? "txt"}`;
                return <article className="org-document-submissions__row" key={id}><div className="org-document-submissions__main"><strong>{name}</strong><small>{submission.scopeName} · {submission.byteSize ? readableBytes(submission.byteSize) : "文件"}</small>{submission.reviewNote && <em>{submission.reviewNote}</em>}</div><span className={`org-state org-state--${submission.state === "approved" ? "ready" : submission.scanStatus ?? submission.state}`}>{documentSubmissionLabel(submission)}</span><div className="org-document-submissions__actions">{submission.scanStatus === "passed" && <><button onClick={() => void showDocumentPreview(id, fileName, submission.sourceType ?? "txt", true, submission.resultDocumentId)} disabled={previewLoading}><Eye size={14} />查看</button><button onClick={() => void downloadDocument(id, fileName, true)}><Download size={14} />下载</button></>}<button onClick={() => { setActionInvoker(null); setRemoveTarget({ id, name, submission: true }); }} disabled={busy || !id}><Trash2 size={14} />删除</button></div></article>;
              })}</div>}
            </div>}
          </div>
          {tab !== "overview" && workspaceLoaded && !(activePage.error && activePage.items.length === 0) && <footer className="org-resource-pagination"><span>{activePage.total ? `${(activePage.page - 1) * activePage.size + 1}–${Math.min(activePage.page * activePage.size, activePage.total)} / ${activePage.total} 项` : "0 项"}</span><label>每页<select aria-label="每页数量" value={activeQuery.size} onChange={event => updateQuery({ size: Number(event.target.value) })}><option value={20}>20</option><option value={50}>50</option></select></label><div><button type="button" disabled={activePage.loading || activePage.page <= 1} onClick={() => updateQuery({ page: activePage.page - 1 })}>上一页</button><span>{activePage.page} / {Math.max(1, Math.ceil(activePage.total / activePage.size))}</span><button type="button" disabled={activePage.loading || activePage.page * activePage.size >= activePage.total} onClick={() => updateQuery({ page: activePage.page + 1 })}>下一页</button></div></footer>}
          {createPortal(<div className="org-memory org-dialog-layer">
          {showMemoryForm && <div ref={memoryFormRef} tabIndex={-1} className="org-document-modal" role="dialog" aria-modal="true" aria-label="提交经验">
            <div className="org-editor-dialog"><header><h3>提交经验</h3><button type="button" aria-label="关闭经验表单" disabled={busy} onClick={() => setShowMemoryForm(false)}><X size={18} /></button></header>
              <form onSubmit={submitMemoryCandidate}><div className="org-memory-form">
                <label>发布范围<select data-modal-initial-focus value={writeScope} onChange={event => setWriteScope(event.target.value)} required>{scopes.map(scope => <option key={scope.id} value={scope.id}>{scopeLabel(scope.kind)} · {scope.name}</option>)}</select></label>
                <label>类型<select value={memoryKind} onChange={event => setMemoryKind(event.target.value as OrgMemoryKind)}>{Object.entries(memoryKindLabel).map(([value,label]) => <option key={value} value={value}>{label}</option>)}</select></label>
                <label className="org-memory-form__wide">经验内容<textarea aria-label="经验内容" required maxLength={2000} rows={4} value={memoryContent} onChange={event => setMemoryContent(event.target.value)} placeholder="写成可直接指导下一次任务的明确结论或步骤" /></label>
                <label className="org-memory-form__wide">为什么有效（可选）<textarea maxLength={2000} rows={2} value={memoryRationale} onChange={event => setMemoryRationale(event.target.value)} placeholder="背景、适用条件或决策依据" /></label>
                <label>结果（可选）<input maxLength={2000} value={memoryOutcome} onChange={event => setMemoryOutcome(event.target.value)} placeholder="例如：构建时间下降 30%" /></label>
                <label>项目/工作空间（可选）<input maxLength={1000} value={memoryWorkspace} onChange={event => setMemoryWorkspace(event.target.value)} placeholder="用于项目定向召回" /></label>
                <label>有效期（可选）<input type="date" value={memoryValidUntil} onChange={event => setMemoryValidUntil(event.target.value)} /></label>
                {error && <div className="org-memory-form__wide"><OrganizationError reason={error} /></div>}
              </div><footer><button type="button" disabled={busy} onClick={() => setShowMemoryForm(false)}>取消</button><button className="org-memory__primary" disabled={busy || !memoryContent.trim() || !writeScope}>{busy ? "提交中…" : "提交审核"}</button></footer></form>
            </div>
          </div>}
          {memoryDetail && <div ref={memoryDetailRef} tabIndex={-1} className="org-document-modal" role="dialog" aria-modal="true" aria-label="经验详情" onClick={event => { if(event.target === event.currentTarget) setMemoryDetail(null); }}>
            <div className="org-editor-dialog"><header><div><h3>{memoryKindLabel[memoryDetail.kind]}</h3><span>{memoryDetail.scopeName} · {memoryDetail.trust === "verified" ? "已验证" : "已审核"}</span></div><button data-modal-initial-focus aria-label="关闭经验详情" onClick={() => setMemoryDetail(null)}><X size={18} /></button></header><div className="org-memory-detail"><p>{memoryDetail.content}</p>{memoryDetail.stale && <span className="org-state org-state--failed">已过期，仅供参考</span>}{memoryDetail.rationale && <><h4>适用背景与依据</h4><p>{memoryDetail.rationale}</p></>}{memoryDetail.outcome && <><h4>结果</h4><p>{memoryDetail.outcome}</p></>}{memoryDetail.workspaceRef && <><h4>项目/工作空间</h4><p>{memoryDetail.workspaceRef}</p></>}{memoryDetail.validUntil && <><h4>有效期至</h4><p>{new Date(memoryDetail.validUntil).toLocaleDateString()}</p></>}</div></div>
          </div>}
          {skillDialog && <div ref={skillDialogRef} tabIndex={-1} className="org-document-modal" role="dialog" aria-modal="true" aria-label={skillDialog.kind === "upload" ? "提交 Skill" : "发布 Skill 副本"}>
            <div className="org-document-modal__confirm"><h3>{skillDialog.kind === "upload" ? "提交 Skill" : "发布 Skill 副本"}</h3><p>{skillDialog.kind === "upload" ? "选择共享范围后，选择本地 Skill 文件或技能包。提交内容会按组织规则审核。" : `将“${skillDialog.skill.name}”发布到共享范围，个人版本保持不变。`}</p><label className="org-document-publish__scope"><span>{skillDialog.kind === "upload" ? "提交到" : "发布到"}</span><select data-modal-initial-focus aria-label={skillDialog.kind === "upload" ? "Skill 提交范围" : "Skill 副本发布目标"} value={skillDialog.kind === "upload" ? writeScope : publishScope} onChange={event => skillDialog.kind === "upload" ? setWriteScope(event.target.value) : setPublishScope(event.target.value)}><option value="">选择范围</option>{scopes.filter(scope => skillDialog.kind === "upload" || scope.kind !== "personal").map(scope => <option key={scope.id} value={scope.id}>{scopeLabel(scope.kind)} · {scope.name}</option>)}</select></label>{error && <OrganizationError reason={error} />}<div><button disabled={busy} onClick={() => setSkillDialog(null)}>取消</button><button className="org-memory__primary" disabled={busy || (skillDialog.kind === "upload" ? !writeScope || !allowSkillSubmission : !publishScope)} onClick={() => { if(skillDialog.kind === "publish") void publishSkill(skillDialog.skill); else { setSkillDialog(null); void pickAndUploadSkill(); } }}>{busy ? "处理中…" : skillDialog.kind === "upload" ? "选择文件" : "确认发布"}</button></div></div>
          </div>}
          {folderBatch && <div ref={folderBatchRef} tabIndex={-1} className="org-document-modal" role="dialog" aria-modal="true" aria-label="文件夹上传确认"><div className="org-editor-dialog"><header><h3>准备上传 {folderBatch.files.length} 个文档</h3></header><div className="org-document-batch"><p>目标：{folderBatch.scopeName} · 共 {readableBytes(folderBatch.totalBytes)} · 已跳过 {folderBatch.skipped} 项不支持或不符合限制的内容</p><small>{folderBatch.folder}</small><ul>{folderBatch.files.map(file => <li key={file.path}>{file.relativePath}<span>{readableBytes(file.size)}</span></li>)}</ul>{error && <OrganizationError reason={error} />}</div><footer><button disabled={busy} onClick={() => setFolderBatch(null)}>取消</button><button className="org-memory__primary" disabled={busy} onClick={() => void uploadDocumentsInBatch(folderBatch.files.map(file => ({ path:file.path,title:file.relativePath })),folderBatch.scopeId)}>开始上传</button></footer></div></div>}
          {preview && <div ref={previewDialogRef} tabIndex={-1} onClick={(event) => { if (event.target === event.currentTarget) closePreview(); }} className="org-document-modal" role="dialog" aria-modal="true" aria-label={`查看文档 ${preview.name}`}>
            <div className="org-document-modal__panel">
              <header><div><strong>{preview.name}</strong>{preview.parsed && <small>内容来自知识索引，原件排版请下载查看</small>}</div><div className="org-document-modal__header-actions">{!preview.unavailableReason && <button onClick={() => void downloadDocument(preview.resourceId, preview.name, preview.submission)}><Download size={14} />下载原件</button>}<button data-modal-initial-focus aria-label="关闭预览" onClick={closePreview}><X size={18} /></button></div></header>
              <div className="org-document-modal__body">{preview.unavailableReason
                ? <div className="org-parsed-preview__unavailable"><FileText size={28} /><strong>暂时无法在线预览</strong><p>{preview.unavailableReason}</p><button className="org-memory__primary" onClick={() => void downloadDocument(preview.resourceId, preview.name, preview.submission)}><Download size={15} />下载原件</button></div>
                : preview.parsed
                ? <ParsedDocumentPreview name={preview.name} sourceType={preview.sourceType} content={preview.content} />
                : <FilePreview filename={preview.name} content={preview.content} />}</div>
              {preview.more && <footer><button onClick={() => void loadMorePreview()} disabled={previewLoading}>{previewLoading ? "加载中…" : "加载后续内容"}</button></footer>}
            </div>
          </div>}
          {publishDocumentTarget && <div ref={publishDialogRef} tabIndex={-1} onClick={(event) => { if (event.target === event.currentTarget) closePublishDialog(); }} aria-busy={busy} className="org-document-modal" role="dialog" aria-modal="true" aria-label="发布文档副本">
            <div className="org-document-modal__confirm"><h3>发布文档副本</h3><p>将“{publishDocumentTarget.title}”发布到指定共享范围。原文档仍留在我的空间。</p><label className="org-document-publish__scope"><span>发布到</span><select aria-label="文档副本发布目标" value={publishScope} onChange={(event) => setPublishScope(event.target.value)}><option value="">选择共享范围</option>{scopes.filter((scope) => scope.kind !== "personal").map((scope) => <option key={scope.id} value={scope.id} disabled={scope.canPublishDocuments === false}>{scopeLabel(scope.kind)} · {scope.name}{scope.canPublishDocuments === false ? "（仅管理员可发布）" : ""}</option>)}</select></label><div><button data-modal-initial-focus onClick={closePublishDialog} disabled={busy}>取消</button><button className="org-memory__primary" onClick={() => void publishDocument(publishDocumentTarget)} disabled={busy || !publishScope || scopes.find((scope) => scope.id === publishScope)?.canPublishDocuments === false}>{busy ? "发布中…" : "确认发布"}</button></div></div>
          </div>}
          {uploadIntent && <div ref={uploadDialogRef} className="org-document-modal" role="dialog" aria-modal="true" aria-label="选择上传位置" tabIndex={-1}>
            <div className="org-document-modal__confirm"><h3>{uploadIntent === "folder" ? "上传文件夹" : "上传文档"}</h3><p>选择文档的可访问范围。上传后会自动安全扫描并建立索引。</p><label className="org-document-publish__scope"><span>上传到</span><select aria-label="文档上传范围" data-modal-initial-focus value={writeScope} onChange={(event) => setWriteScope(event.target.value)}><option value="">选择上传范围</option>{availableDocumentScopes.map((scope) => <option key={scope.id} value={scope.id}>{scopeLabel(scope.kind)} · {scope.name}</option>)}</select></label><div><button onClick={() => setUploadIntent(null)}>取消</button><button className="org-memory__primary" onClick={() => void startDocumentUpload()} disabled={!allowDocumentUpload}>{uploadIntent === "folder" ? "选择文件夹" : "选择文件"}</button></div></div>
          </div>}
          {removeTarget && <div ref={removeDialogRef} tabIndex={-1} onClick={(event) => { if (event.target === event.currentTarget) closeRemoveDialog(); }} aria-busy={busy} className="org-document-modal" role="alertdialog" aria-modal="true" aria-label="确认删除文档">
            <div className="org-document-modal__confirm"><h3>移除文档？</h3><p>“{removeTarget.name}”会从共享列表和 Agent 检索中移除。服务器会保留已上传的原始文件内容。</p><div><button data-modal-initial-focus onClick={closeRemoveDialog} disabled={busy}>取消</button><button className="org-memory__primary" onClick={() => void removeOwnDocument()} disabled={busy}>{busy ? "处理中…" : "确认移除"}</button></div></div>
          </div>}
          </div>, document.body)}
        </main>
      </div>
    </section>
  );
}

function ParsedDocumentPreview({ name, sourceType, content }: { name: string; sourceType: string; content: string }) {
  const spreadsheet = sourceType.toLowerCase() === "xlsx";
  return <div className="org-parsed-preview">
    <div className="org-parsed-preview__intro"><FileText size={19} /><div><strong>{name}</strong><span>{spreadsheet ? "工作表内容" : "文档内容"} · 来自知识索引</span></div></div>
    <pre className={spreadsheet ? "org-parsed-preview__content org-parsed-preview__content--sheet" : "org-parsed-preview__content"}>{content}</pre>
  </div>;
}

function SubmissionList({ title, submissions }: { title: string; submissions: Submission[] }) {
  if (submissions.length === 0) return null;
  return <div className="org-submissions"><h3>{title}</h3>{submissions.map(submission => {
    const label = submission.state !== "pending" ? stateLabel(submission.state)
      : submission.scanStatus === "queued" ? "等待安全检查"
        : submission.scanStatus === "scanning" ? "安全检查中"
          : submission.scanStatus === "failed" ? "安全检查未通过" : "待审核";
    return <div key={submission.id ?? submission.submissionId}>
      <span>{submission.title ?? submission.name}{submission.version ? ` · v${submission.version}` : ""}</span>
      <span className={`org-state org-state--${submission.state}`}>{label}</span>
      <small>{submission.scopeName}</small>
      {submission.reviewNote && <em>{submission.reviewNote}</em>}
      {submission.scanReport?.findings.filter(item => item.severity !== "info").map(item => <em key={`${item.code}-${item.path ?? ""}`}>{item.message}{item.path ? ` (${item.path})` : ""}</em>)}
    </div>;
  })}</div>;
}

function MemoryPromotionList({ promotions }: { promotions: MemoryPromotion[] }) {
  if (promotions.length === 0) return null;
  return <div className="org-submissions"><h3>我的经验提交</h3>{promotions.map(item => <div key={item.id}>
    <details className="org-submission-detail"><summary><span>{item.payload.content ?? "经验候选"}</span><small>查看内容</small></summary>
      {item.payload.rationale && <p>{item.payload.rationale}</p>}{item.payload.outcome && <p>结果：{item.payload.outcome}</p>}
    </details>
    <span className={`org-state org-state--${item.state}`}>{stateLabel(item.state)}</span>
    <small>{item.scopeName} · {item.payload.kind ? memoryKindLabel[item.payload.kind] : "经验"}</small>
    {item.reviewNote && <em>{item.reviewNote}</em>}
  </div>)}</div>;
}
