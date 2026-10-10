import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  AlertTriangle,
  Archive,
  BookOpenCheck,
  BrainCircuit,
  Building2,
  CheckCircle2,
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
  orgDocumentSubmissionsMinePage,
  orgArchiveDocument,
  orgDownloadDocument,
  orgFetchDocument,
  orgListDocuments,
  orgListMemories,
  orgListScopes,
  orgListSkills,
  orgLogin,
  orgLogout,
  orgMemoryPromotionsMine,
  orgPreviewDocument,
  orgRemoveOwnSubmission,
  orgScanDocumentFolder,
  orgSession,
  orgSkillSubmissionsMine,
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
  type MemoryPromotion,
  type Submission,
} from "@/lib/org-client";
import { useOrgSessionStore } from "@/stores/org-session-store";
import { filesystemPickDirectory, filesystemPickFiles } from "@/lib/agent-client";
import { FilePreview } from "./FilePreview";
import { useModalFocus } from "@/lib/use-modal-focus";

type Tab = "overview" | "memories" | "documents" | "skills";

const memoryKindLabel: Record<OrgMemoryKind, string> = {
  fact: "事实",
  decision: "决策",
  convention: "规范",
  pitfall: "踩坑",
  howto: "操作手册",
};

const scopeLabel = (kind: OrgScope["kind"]) =>
  kind === "personal" ? "仅自己" : kind === "team" ? "团队" : "全组织";

const stateLabel = (state: string) => ({
  pending: "待审核",
  approved: "已通过",
  rejected: "已拒绝",
  withdrawn: "已撤回",
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
  const [session, setSession] = useState<OrgSession | null>(null);
  const mirroredSession = useOrgSessionStore((state) => state.session);
  const mirrorOrgSession = useOrgSessionStore((state) => state.setSession);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [uploadProgress, setUploadProgress] = useState<{
    label: string;
    completed: number;
    total: number;
  } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [serverUrl, setServerUrl] = useState("https://");
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [tab, setTab] = useState<Tab>("overview");
  const [scopes, setScopes] = useState<OrgScope[]>([]);
  const [selectedScope, setSelectedScope] = useState("");
  const selectedScopeRef = useRef("");
  const [writeScope, setWriteScope] = useState("");
  const [publishScope, setPublishScope] = useState("");
  const [documents, setDocuments] = useState<OrgDocument[]>([]);
  const [memories, setMemories] = useState<OrgMemory[]>([]);
  const [memoryPromotions, setMemoryPromotions] = useState<MemoryPromotion[]>([]);
  const [documentSubmissions, setDocumentSubmissions] = useState<Submission[]>([]);
  const [documentTotal, setDocumentTotal] = useState(0);
  const [documentPage, setDocumentPage] = useState(1);
  const [submissionTotal, setSubmissionTotal] = useState(0);
  const [submissionPage, setSubmissionPage] = useState(1);
  const [folderBatch, setFolderBatch] = useState<{ folder: string; files: OrgFolderFile[]; skipped: number; totalBytes: number; scopeId: string; scopeName: string } | null>(null);
  const [uploadIntent, setUploadIntent] = useState<"files" | "folder" | null>(null);
  const uploadDialogRef = useModalFocus<HTMLDivElement>(Boolean(uploadIntent), () => setUploadIntent(null));
  const [preview, setPreview] = useState<{ name: string; content: string; parsed: boolean; docId?: string; nextChunk?: number; more?: boolean; resourceId: string; submission: boolean; sourceType: string; unavailableReason?: string } | null>(null);
  const [previewLoading, setPreviewLoading] = useState(false);
  const [removeTarget, setRemoveTarget] = useState<{ id: string; name: string; submission: boolean } | null>(null);
  const [publishDocumentTarget, setPublishDocumentTarget] = useState<OrgDocument | null>(null);
  const previewDialogRef = useModalFocus<HTMLDivElement>(Boolean(preview), () => setPreview(null));
  const closePublishDialog = () => { if (!busy) setPublishDocumentTarget(null); };
  const closeRemoveDialog = () => { if (!busy) setRemoveTarget(null); };
  const publishDialogRef = useModalFocus<HTMLDivElement>(Boolean(publishDocumentTarget), closePublishDialog);
  const removeDialogRef = useModalFocus<HTMLDivElement>(Boolean(removeTarget), closeRemoveDialog);
  const [skills, setSkills] = useState<OrgSkill[]>([]);
  const [skillSubmissions, setSkillSubmissions] = useState<Submission[]>([]);
  const [showMemoryForm, setShowMemoryForm] = useState(false);
  const [memoryKind, setMemoryKind] = useState<OrgMemoryKind>("howto");
  const [memoryContent, setMemoryContent] = useState("");
  const [memoryRationale, setMemoryRationale] = useState("");
  const [memoryOutcome, setMemoryOutcome] = useState("");
  const [memoryWorkspace, setMemoryWorkspace] = useState("");
  const [memoryValidUntil, setMemoryValidUntil] = useState("");

  useEffect(() => {
    if (mirroredSession) setSession(mirroredSession);
  }, [mirroredSession]);

  const loadWorkspace = useCallback(async () => {
    const nextScopes = await orgListScopes();
    const activeScope = nextScopes.some((scope) => scope.id === selectedScopeRef.current)
      ? selectedScopeRef.current : "";
    selectedScopeRef.current = activeScope;
    const [docs, nextSkills, docSubs, skillSubs, nextMemories, promotions] = await Promise.all([
      orgListDocuments(activeScope || undefined),
      orgListSkills(),
      orgDocumentSubmissionsMinePage(),
      orgSkillSubmissionsMine(),
      orgListMemories(),
      orgMemoryPromotionsMine(),
    ]);
    setScopes(nextScopes);
    if (selectedScopeRef.current === activeScope) {
      setDocuments(docs.items);
      setDocumentTotal(docs.total);
      setDocumentPage(1);
      setSelectedScope(activeScope);
    }
    setSkills(nextSkills);
    setDocumentSubmissions(docSubs.items);
    setSubmissionTotal(docSubs.total);
    setSubmissionPage(1);
    setSkillSubmissions(skillSubs);
    setMemories(nextMemories);
    setMemoryPromotions(promotions.filter((item) => item.payloadType === "memory"));
    setWriteScope((current) => nextScopes.some((scope) => scope.id === current && scope.canPublishDocuments !== false)
      ? current
      : nextScopes.find((scope) => scope.kind === "personal")?.id || nextScopes[0]?.id || "");
    setPublishScope((current) => nextScopes.some((scope) => scope.id === current && scope.kind !== "personal")
      ? current
      : nextScopes.find((scope) => scope.kind === "team")?.id || nextScopes.find((scope) => scope.kind === "org")?.id || "");
  }, []);

  useEffect(() => {
    let alive = true;
    orgSession()
      .then(async (nextSession) => {
        if (!alive) return;
        setSession(nextSession);
        mirrorOrgSession(nextSession);
        if (nextSession.serverUrl) setServerUrl(nextSession.serverUrl);
        if (nextSession.username || nextSession.user?.username) {
          setUsername(nextSession.username ?? nextSession.user?.username ?? "");
        }
        if (nextSession.loggedIn) await loadWorkspace();
      })
      .catch((reason) => alive && setError(String(reason)))
      .finally(() => alive && setLoading(false));
    return () => { alive = false; };
  }, [loadWorkspace, mirrorOrgSession]);

  useEffect(() => {
    if (!session?.loggedIn) return;
    const active = documentSubmissions.some(submissionIsActive)
      || skillSubmissions.some(submissionIsActive)
      || memoryPromotions.some((item) => item.state === "pending")
      || documents.some(documentIsProcessing);
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
  ]);

  const uploadScope = useMemo(
    () => scopes.find((scope) => scope.id === writeScope),
    [scopes, writeScope],
  );
  const allowSkillSubmission = session?.bootstrap?.policy.allowSkillSubmission !== false;
  const availableDocumentScopes = scopes.filter((scope) => scope.canPublishDocuments !== false
    && (scope.kind !== "personal" || session?.bootstrap?.policy.allowPersonalCloud !== false));
  const allowDocumentUpload = !!uploadScope && availableDocumentScopes.some((scope) => scope.id === uploadScope.id);
  const visibleDocuments = documents;
  const visibleMemories = useMemo(
    () => selectedScope ? memories.filter((memory) => memory.scopeId === selectedScope) : memories,
    [memories, selectedScope],
  );
  const visibleSkills = useMemo(
    () => selectedScope ? skills.filter((skill) => skill.scopeId === selectedScope) : skills,
    [skills, selectedScope],
  );
  const visibleDocumentSubmissions = documentSubmissions;
  const visibleSkillSubmissions = useMemo(
    () => selectedScope ? skillSubmissions.filter((submission) => submission.scopeId === selectedScope) : skillSubmissions,
    [skillSubmissions, selectedScope],
  );

  const handleLogin = async (event: React.FormEvent) => {
    event.preventDefault();
    setBusy(true);
    setError(null);
    try {
      const next = await orgLogin(serverUrl, username, password);
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
    setBusy(true);
    setError(null);
    try {
      const nextSession = await orgSession();
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
      await loadWorkspace();
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(false);
    }
  };

  const resetWorkspace = (signedOutSession: OrgSession = { loggedIn: false }) => {
    setSession(signedOutSession);
    mirrorOrgSession(signedOutSession);
    setScopes([]);
    setSelectedScope("");
    selectedScopeRef.current = "";
    setWriteScope("");
    setPublishScope("");
    setDocuments([]);
    setDocumentTotal(0);
    setDocumentPage(1);
    setMemories([]);
    setMemoryPromotions([]);
    setDocumentSubmissions([]);
    setSubmissionTotal(0);
    setSubmissionPage(1);
    setFolderBatch(null);
    setUploadIntent(null);
    setPreview(null);
    setSkills([]);
    setSkillSubmissions([]);
    setTab("overview");
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
    setError(null);
    try {
      const paths = await filesystemPickFiles({
        multiple: true,
        maxFiles: 50,
        title: "选择要提交的可检索文档（可多选）",
        extensions: ["md", "txt", "pdf", "docx", "xlsx", "pptx", "png", "jpg", "jpeg"],
      });
      if (!paths || paths.length === 0) return;
      await uploadDocumentsInBatch(paths.map((path) => ({ path })), uploadScope.id);
    } catch (reason) {
      setError(`选择文档失败：${String(reason).replace(/^Error:\s*/, "")}`);
    }
  };

  const pickDocumentFolder = async () => {
    if (!uploadScope) return;
    setError(null);
    try {
      const folder = await filesystemPickDirectory();
      if (!folder) return;
      const result = await orgScanDocumentFolder(folder);
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
    setBusy(true);
    setError(null);
    setUploadProgress({ label: "正在上传文档", completed: 0, total: files.length });
    let batchError: string | null = null;
    try {
      const settled = await settleWithConcurrency(
        files,
        3,
        (file) => orgSubmitDocument(file.path, scopeId, file.title),
        (completed) => setUploadProgress({
          label: "正在上传文档",
          completed,
          total: files.length,
        }),
      );
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
        onToast?.(`新增 ${done} 个文档，跳过重复 ${skipped} 个${failures.length ? `，失败 ${failures.length} 个` : ""}${done ? "；正在建立索引" : ""}`);
      } else {
        onToast?.(`上传失败：${failures[0] ?? "未知原因"}`);
      }
      setUploadProgress({ label: "正在刷新列表", completed: files.length, total: files.length });
      await loadWorkspace();
      setFolderBatch(null);
    } catch (reason) {
      const refreshError = `刷新列表失败：${String(reason).replace(/^Error:\s*/, "")}`;
      setError(batchError ? `${batchError}\n${refreshError}` : refreshError);
    } finally {
      setUploadProgress(null);
      setBusy(false);
    }
  };

  const changeDocumentScope = async (scopeId: string) => {
    selectedScopeRef.current = scopeId;
    setSelectedScope(scopeId);
    setError(null);
    try {
      const page = await orgListDocuments(scopeId || undefined);
      if (selectedScopeRef.current !== scopeId) return;
      setDocuments(page.items);
      setDocumentTotal(page.total);
      setDocumentPage(1);
    } catch (reason) { setError(String(reason)); }
  };

  const loadMoreDocuments = async () => {
    const next = documentPage + 1;
    setBusy(true);
    try {
      const page = await orgListDocuments(selectedScopeRef.current || undefined, undefined, next);
      setDocuments((current) => [...current, ...page.items.filter((item) => !current.some((old) => old.id === item.id))]);
      setDocumentTotal(page.total);
      setDocumentPage(next);
    } catch (reason) { setError(String(reason)); } finally { setBusy(false); }
  };

  const loadMoreSubmissions = async () => {
    const next = submissionPage + 1;
    setBusy(true);
    try {
      const page = await orgDocumentSubmissionsMinePage(next);
      setDocumentSubmissions((current) => [...current, ...page.items.filter((item) => !current.some((old) => old.id === item.id))]);
      setSubmissionTotal(page.total);
      setSubmissionPage(next);
    } catch (reason) { setError(String(reason)); } finally { setBusy(false); }
  };

  const showDocumentPreview = async (id: string, name: string, sourceType: string, submission: boolean, publishedDocId?: string | null) => {
    setPreviewLoading(true);
    setPreview(null);
    setError(null);
    const parsedDocId = submission ? publishedDocId : id;
    const officeDocument = ["docx", "xlsx", "pptx"].includes(sourceType.toLowerCase());
    try {
      if (officeDocument && parsedDocId) {
        const parsed = await orgFetchDocument(parsedDocId, null, "0:199");
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
      setPreview({ name, content: result.content, parsed: false, resourceId: id, submission, sourceType });
    } catch (reason) {
      if (officeDocument) {
        setPreview({ name, content: "", parsed: false, resourceId: id, submission, sourceType, unavailableReason: "暂时无法读取文档内容。可下载原件在本地查看，稍后重试在线预览。" });
        return;
      }
      if (parsedDocId && !officeDocument) {
        try {
          const parsed = await orgFetchDocument(parsedDocId, null, "0:199");
          if (parsed.chunks.length === 0) throw new Error("尚无解析内容");
          setPreview({ name, content: parsed.text, parsed: true, docId: parsedDocId, nextChunk: (parsed.nextSeq ?? 199) + 1, more: parsed.hasMore ?? parsed.chunks.length === 200, resourceId: id, submission, sourceType });
          return;
        } catch { /* Show the original preview error. */ }
      }
      setError(`无法在线预览文档：${String(reason).replace(/^Error:\s*/, "")}。可下载原件在本地查看。`);
    } finally { setPreviewLoading(false); }
  };

  const loadMorePreview = async () => {
    if (!preview?.docId || preview.nextChunk === undefined) return;
    setPreviewLoading(true);
    try {
      const start = preview.nextChunk;
      const parsed = await orgFetchDocument(preview.docId, null, `${start}:${start + 199}`);
      setPreview((current) => current ? {
        ...current,
        content: `${current.content}\n\n${parsed.text}`,
        nextChunk: (parsed.nextSeq ?? start + 199) + 1,
        more: parsed.hasMore ?? parsed.chunks.length === 200,
      } : current);
    } catch (reason) { setError(String(reason)); } finally { setPreviewLoading(false); }
  };

  const downloadDocument = async (id: string, name: string, submission: boolean) => {
    setError(null);
    try {
      const saved = await orgDownloadDocument(id, submission, name);
      if (saved) onToast?.(`已保存到 ${saved}`);
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
      onToast?.("文档已移出共享与检索；已上传原件仍安全保留在服务器");
      await loadWorkspace();
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
      onToast?.("经验候选已提交，审核通过后会进入 Agent 工作上下文");
      await loadWorkspace();
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(false);
    }
  };

  const pickAndUploadSkill = async () => {
    if (!uploadScope) return;
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
    if (!paths || paths.length === 0) return;
    setBusy(true);
    setUploadProgress({ label: "正在上传 Skill", completed: 0, total: paths.length });
    let batchError: string | null = null;
    try {
      const settled = await settleWithConcurrency(
        paths,
        3,
        (path) => orgSubmitSkill(path, uploadScope.id),
        (completed) => setUploadProgress({
          label: "正在上传 Skill",
          completed,
          total: paths.length,
        }),
      );
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
        onToast?.(`Skill 上传失败：${failures[0] ?? "未知原因"}`);
      }
      setUploadProgress({ label: "正在刷新列表", completed: paths.length, total: paths.length });
      await loadWorkspace();
    } catch (reason) {
      const refreshError = `刷新列表失败：${String(reason).replace(/^Error:\s*/, "")}`;
      setError(batchError ? `${batchError}\n${refreshError}` : refreshError);
    } finally {
      setUploadProgress(null);
      setBusy(false);
    }
  };

  const uploadNewVersion = async (document: OrgDocument) => {
    const extensions = document.sourceType === "image" ? ["png", "jpg", "jpeg"] : [document.sourceType];
    const [path] = await filesystemPickFiles({
      multiple: false,
      title: "选择同类型新版本",
      extensions,
    });
    if (!path) return;
    setBusy(true);
    try {
      await orgNewDocumentVersion(document.id, path);
      onToast?.("新版本已上传；索引完成前旧版本继续生效");
      await loadWorkspace();
    } catch (reason) { setError(String(reason)); } finally { setBusy(false); }
  };

  const publishDocument = async (document: OrgDocument) => {
    if (!publishScope) return;
    setBusy(true);
    try {
      const result = await orgPublishDocument(document.id, publishScope);
      onToast?.(result.state === "pending" ? "已生成副本，等待安全扫描" : "文档副本已发布");
      setPublishDocumentTarget(null);
      await loadWorkspace();
    } catch (reason) { setError(String(reason)); } finally { setBusy(false); }
  };

  const toggleSkill = async (skill: OrgSkill) => {
    setBusy(true);
    try {
      await orgSetSkillPreference(skill.skillId, !skill.enabled);
      onToast?.(skill.enabled
        ? "Skill 已从用户全局技能目录卸载"
        : "Skill 已安装到用户全局技能目录");
      await loadWorkspace();
    } catch (reason) { setError(String(reason)); } finally { setBusy(false); }
  };

  const publishSkill = async (skill: OrgSkill) => {
    if (!publishScope) return;
    setBusy(true);
    try {
      const result = await orgPublishSkill(skill.skillId, publishScope);
      onToast?.(result.state === "pending" ? "Skill 副本已提交目标范围审核" : "Skill 副本已发布");
      await loadWorkspace();
    } catch (reason) { setError(String(reason)); } finally { setBusy(false); }
  };

  const syncSkills = async () => {
    setBusy(true);
    setError(null);
    try {
      const result = await orgSyncSkills();
      onToast?.(`已同步 ${result.installed.length} 个已安装 Skills`);
      await loadWorkspace();
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(false);
    }
  };

  if (loading) {
    return <div className="org-memory org-memory--center"><Loader2 className="org-memory__spin" />正在连接组织服务…</div>;
  }

  if (!session?.loggedIn) {
    return (
      <div className="org-memory org-memory--login">
        <div className="org-login-card">
          <div className="org-login-card__mark"><Building2 size={28} /></div>
          <h1>连接组织</h1>
          <p>登录企业服务器后，可在授权范围内共享文档、Skills 和经验；Agent 会在执行前自动召回规则、手册与踩坑记录。</p>
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
            {error && <div className="org-memory__error"><XCircle size={15} />{error}</div>}
            <button className="org-memory__primary" disabled={busy}>{busy && <Loader2 className="org-memory__spin" size={15} />}安全登录</button>
          </form>
          <div className="org-login-card__security"><LockKeyhole size={14} />密码不会保存；自动续期凭据由系统安全存储保护，访问令牌只留在内存中。</div>
        </div>
      </div>
    );
  }

  const pendingMemoryCount = memoryPromotions.filter((item) => item.state === "pending").length;
  const pendingDocumentCount = documentSubmissions.filter(submissionIsActive).length
    + documents.filter(documentIsProcessing).length;
  const pendingSkillCount = skillSubmissions.filter(submissionIsActive).length;
  const pendingCount = pendingMemoryCount + pendingDocumentCount + pendingSkillCount;
  const pageTitle = tab === "overview" ? "组织知识"
    : tab === "memories" ? "经验"
      : tab === "documents" ? "文档" : "组织 Skills";
  const pageDescription = tab === "overview" ? "管理可供 Agent 使用的组织经验、文档与 Skills。"
    : tab === "memories" ? "沉淀决策、规范、操作手册和踩坑记录，供 Agent 在任务前召回。"
      : tab === "documents" ? "查看、上传和管理组织文档。"
        : "管理组织 Skill 的提交、安装与同步。";

  return (
    <div className="org-memory">
      <header className="org-memory__header">
        <div>
          <div className="org-memory__eyebrow">组织工作台</div>
          <h1>{session.bootstrap?.scopes.find((scope) => scope.kind === "org")?.name ?? "组织知识管理中心"}</h1>
          <p>{session.user?.displayName ?? session.user?.username} · {session.serverUrl}</p>
          <span className={`org-memory__capability ${session.organizationMemoryEnabled ? "is-active" : ""}`}>
            <i />{session.organizationMemoryEnabled
              ? "Agent 组织上下文已启用"
              : session.bootstrap?.scopes?.some((scope) => scope.kind === "team" || scope.kind === "org")
                ? "Agent 组织上下文暂不可用"
                : "未加入共享组织范围"}
          </span>
        </div>
        <div className="org-memory__header-actions">
          <button onClick={() => void refreshWorkspace()} disabled={busy}><RefreshCw size={15} />刷新</button>
          <button onClick={() => void handleLogout()} disabled={busy}><LogOut size={15} />退出组织</button>
        </div>
      </header>

      <div className="org-memory__workspace">
        <nav className="org-memory__nav" aria-label="组织功能">
          <div className="org-memory__nav-label">工作台</div>
          <button className={tab === "overview" ? "active" : ""} aria-current={tab === "overview" ? "page" : undefined} onClick={() => setTab("overview")}><LayoutDashboard size={16} /><span>概览</span></button>
          <div className="org-memory__nav-label">知识资产</div>
          <button className={tab === "memories" ? "active" : ""} aria-current={tab === "memories" ? "page" : undefined} onClick={() => setTab("memories")}><BrainCircuit size={16} /><span>经验</span><em>{memories.length}</em></button>
          <button className={tab === "documents" ? "active" : ""} aria-current={tab === "documents" ? "page" : undefined} onClick={() => setTab("documents")}><FileText size={16} /><span>文档</span><em>{documents.length}</em></button>
          <div className="org-memory__nav-label">能力分发</div>
          <button className={tab === "skills" ? "active" : ""} aria-current={tab === "skills" ? "page" : undefined} onClick={() => setTab("skills")}><CheckCircle2 size={16} /><span>Skills</span><em>{skills.length}</em></button>
        </nav>

        <main className="org-memory__content">
          <div className="org-memory__page-header">
            <div><h2>{pageTitle}</h2><p>{pageDescription}</p></div>
            {tab !== "overview" && tab !== "documents" && <label className="org-memory__filter"><span>查看范围</span><select value={selectedScope} onChange={(event) => void changeDocumentScope(event.target.value)}><option value="">全部授权范围</option>{scopes.map((scope) => <option key={scope.id} value={scope.id}>{scopeLabel(scope.kind)} · {scope.name}</option>)}</select></label>}
            {tab === "documents" && <div className="org-document-library__upload-actions">
              <button onClick={() => openDocumentUpload("folder")} disabled={busy || availableDocumentScopes.length === 0}><FolderUp size={15} />上传文件夹</button>
              <button className="org-memory__primary" onClick={() => openDocumentUpload("files")} disabled={busy || availableDocumentScopes.length === 0}><Upload size={15} />上传文档</button>
            </div>}
          </div>

          {error && <div className="org-memory__error"><XCircle size={15} />{error}<button onClick={() => setError(null)}>关闭</button></div>}
          {uploadProgress && <div className="org-memory__upload-progress" role="status" aria-live="polite"><Loader2 className="org-memory__spin" size={15} /><span>{uploadProgress.label}</span><strong>{uploadProgress.completed}/{uploadProgress.total}</strong></div>}

          {tab === "overview" && <section className="org-overview">
            <div className="org-overview__hero">
              <div className="org-overview__hero-icon"><MessageSquareText size={22} /></div>
              <div><h3>在对话中使用组织知识</h3><p>发起任务时自动选择组织知识，可连续追问，也可以让 Agent 结合组织规则直接执行。</p></div>
              <button className="org-memory__primary" onClick={onStartConversation} disabled={!session.organizationMemoryEnabled || !onStartConversation}>发起对话</button>
            </div>
            <div className="org-overview__metrics" aria-label="组织知识概况">
              <button type="button" onClick={() => setTab("memories")}><span>已发布经验</span><strong>{memories.length}</strong><small>{memoryPromotions.filter((item) => item.state === "pending").length} 条待审核</small></button>
              <button type="button" onClick={() => setTab("documents")}><span>可检索文档</span><strong>{documents.filter((item) => item.status === "ready").length}</strong><small>{documents.filter((item) => item.status !== "ready" && item.status !== "archived").length} 条处理中</small></button>
              <button type="button" onClick={() => setTab("skills")}><span>已安装 Skills</span><strong>{skills.filter((item) => item.enabled).length}/{skills.length}</strong><small>{skills.filter((item) => item.mandatory).length} 个组织强制</small></button>
            </div>
            <div className="org-overview__attention">
              <div><h3>需要关注</h3><p>聚合当前账号需要处理的提交、同步与索引状态。</p></div>
              {pendingCount === 0 && <div className="org-library__empty">当前没有待处理事项</div>}
              {pendingMemoryCount > 0 && <div className="org-overview__attention-row"><AlertTriangle size={17} /><span>{pendingMemoryCount} 条经验待审核</span><button type="button" onClick={() => setTab("memories")}>查看经验</button></div>}
              {pendingDocumentCount > 0 && <div className="org-overview__attention-row"><AlertTriangle size={17} /><span>{pendingDocumentCount} 项文档正在扫描或建立索引</span><button type="button" onClick={() => setTab("documents")}>查看文档</button></div>}
              {pendingSkillCount > 0 && <div className="org-overview__attention-row"><AlertTriangle size={17} /><span>{pendingSkillCount} 个 Skill 正在审核或扫描</span><button type="button" onClick={() => setTab("skills")}>查看 Skills</button></div>}
            </div>
          </section>}

      {tab === "memories" && (
        <section className="org-library org-memory-library">
          <div className="org-library__toolbar org-memory-library__toolbar">
            <div className="org-memory-library__summary"><strong>已发布</strong><span>{visibleMemories.length}</span></div>
            <button className="org-memory__primary" onClick={() => {
              setMemoryWorkspace((value) => value || cwd || "");
              setShowMemoryForm((value) => !value);
            }} disabled={!writeScope}><Plus size={15} />提交经验</button>
          </div>
          {showMemoryForm && (
            <form className="org-memory-form" onSubmit={submitMemoryCandidate}>
              <label>发布范围<select value={writeScope} onChange={(event) => setWriteScope(event.target.value)} required><option value="">选择范围</option>{scopes.map((scope) => <option key={scope.id} value={scope.id}>{scopeLabel(scope.kind)} · {scope.name}</option>)}</select></label>
              <label>类型<select value={memoryKind} onChange={(event) => setMemoryKind(event.target.value as OrgMemoryKind)}>{Object.entries(memoryKindLabel).map(([value, label]) => <option key={value} value={value}>{label}</option>)}</select></label>
              <label className="org-memory-form__wide">经验内容<textarea required maxLength={2000} rows={4} value={memoryContent} onChange={(event) => setMemoryContent(event.target.value)} placeholder="写成可直接指导下一次任务的明确结论或步骤" /></label>
              <label className="org-memory-form__wide">为什么有效<textarea maxLength={2000} rows={2} value={memoryRationale} onChange={(event) => setMemoryRationale(event.target.value)} placeholder="背景、适用条件或决策依据（建议填写）" /></label>
              <label>结果<input maxLength={2000} value={memoryOutcome} onChange={(event) => setMemoryOutcome(event.target.value)} placeholder="例如：构建时间下降 30%" /></label>
              <label>项目/工作空间<input maxLength={1000} value={memoryWorkspace} onChange={(event) => setMemoryWorkspace(event.target.value)} placeholder="可选，用于项目定向召回" /></label>
              <label>有效期<input type="date" value={memoryValidUntil} onChange={(event) => setMemoryValidUntil(event.target.value)} /></label>
              <div className="org-memory-form__actions"><button type="button" onClick={() => setShowMemoryForm(false)}>取消</button><button className="org-memory__primary" disabled={busy || !memoryContent.trim()}>{busy && <Loader2 className="org-memory__spin" size={14} />}提交审核</button></div>
            </form>
          )}
          <div className="org-library__list">
            {visibleMemories.length === 0 && <div className="org-library__empty">当前范围还没有可用于任务的组织经验</div>}
            {visibleMemories.map((memory) => <div className="org-library__row org-memory-row" key={memory.id}>
              <div className="org-library__icon">{memory.kind === "pitfall" ? <AlertTriangle size={18} /> : memory.kind === "howto" ? <BookOpenCheck size={18} /> : <BrainCircuit size={18} />}</div>
              <div className="org-library__main"><strong>{memory.content}</strong><span>{memory.scopeName} · {memoryKindLabel[memory.kind]} · 置信度 {Math.round(memory.confidence * 100)}% · {memory.trust === "verified" ? "已验证" : "已审核"}{memory.workspaceRef ? ` · ${memory.workspaceRef}` : ""}</span>{memory.rationale && <p>{memory.rationale}</p>}{memory.outcome && <em className="org-memory-row__outcome">结果：{memory.outcome}</em>}</div>
              <div className="org-library__actions">{memory.stale ? <span className="org-state org-state--failed">已过期，仅供参考</span> : <span className="org-state org-state--ready">可召回</span>}</div>
            </div>)}
          </div>
          <MemoryPromotionList promotions={memoryPromotions} />
        </section>
      )}

      {tab === "documents" && (
        <section className="org-library org-document-library">
          <div className="org-document-library__heading">
            <h2>共享文档 <span>{documentTotal}</span></h2>
            <label className="org-document-library__filter"><span>查看范围</span><select aria-label="查看范围" value={selectedScope} onChange={(event) => void changeDocumentScope(event.target.value)}><option value="">全部授权范围</option>{scopes.map((scope) => <option key={scope.id} value={scope.id}>{scopeLabel(scope.kind)} · {scope.name}</option>)}</select></label>
          </div>
          {folderBatch && <div className="org-document-batch" role="region" aria-label="文件夹上传确认">
            <strong>准备上传 {folderBatch.files.length} 个文档</strong>
            <p>目标：{folderBatch.scopeName} · 共 {readableBytes(folderBatch.totalBytes)} · 已跳过 {folderBatch.skipped} 项不支持或不符合限制的内容</p>
            <small>{folderBatch.folder}</small>
            <ul>{folderBatch.files.slice(0, 8).map((file) => <li key={file.path}>{file.relativePath} <span>{readableBytes(file.size)}</span></li>)}</ul>
            {folderBatch.files.length > 8 && <small>还有 {folderBatch.files.length - 8} 个文件</small>}
            <div className="org-document-batch__actions"><button onClick={() => setFolderBatch(null)}>取消</button><button className="org-memory__primary" disabled={busy} onClick={() => void uploadDocumentsInBatch(folderBatch.files.map((file) => ({ path: file.path, title: file.relativePath })), folderBatch.scopeId)}>开始上传</button></div>
          </div>}
          <div className="org-library__list">
            {visibleDocuments.length === 0 && <div className="org-library__empty">当前授权范围还没有已发布文档</div>}
            {visibleDocuments.map((document) => {
              const mayManage = document.ownerId === session.user?.id || session.user?.role !== "member";
              const fileName = document.title.match(/\.[A-Za-z0-9]{1,8}$/) ? document.title : `${document.title}.${document.sourceType === "image" ? "png" : document.sourceType}`;
              return <div className="org-library__row org-document-library__row" key={document.id}>
                <div className="org-library__icon"><FileText size={18} /></div>
                <div className="org-library__main"><strong>{document.title}</strong><span>{document.scopeName} · {document.sourceType.toUpperCase()} · {readableBytes(document.byteSize)} · {document.chunkCount} 个知识片段</span>{document.failReason && <em>{document.failReason}</em>}</div>
                <div className="org-library__actions">{document.status === "ready" && <><button onClick={() => void showDocumentPreview(document.id, fileName, document.sourceType, false)} disabled={previewLoading}><Eye size={13} />查看</button><button onClick={() => void downloadDocument(document.id, fileName, false)}><Download size={13} />下载</button></>}{document.scopeKind === "personal" && scopes.some((scope) => scope.kind !== "personal" && scope.canPublishDocuments !== false) && <button onClick={() => setPublishDocumentTarget(document)} disabled={busy}>发布副本</button>}{mayManage && <><button onClick={() => void uploadNewVersion(document)} disabled={busy}><History size={13} />新版本</button><button onClick={() => setRemoveTarget({ id: document.id, name: document.title, submission: false })} disabled={busy || document.status !== "ready"}><Archive size={13} />归档</button></>}<span className={`org-state org-state--${document.status}`}>{stateLabel(document.status)}</span></div>
              </div>
            })}
          </div>
          {documents.length < documentTotal && <div className="org-document-more"><button onClick={() => void loadMoreDocuments()} disabled={busy}>加载更多共享文档（{documents.length}/{documentTotal}）</button></div>}
          <div className="org-submissions org-document-submissions"><h3>我的文档提交 <small>全部范围 · {submissionTotal} 项</small></h3>
            {visibleDocumentSubmissions.length === 0 && <p className="org-document-submissions__empty">还没有文档提交</p>}
            {visibleDocumentSubmissions.map((submission) => {
              const id = submission.id ?? submission.submissionId ?? "";
              const name = submission.title ?? submission.name ?? "未命名文档";
              const fileName = name.match(/\.[A-Za-z0-9]{1,8}$/) ? name : `${name}.${submission.sourceType === "image" ? "png" : submission.sourceType ?? "txt"}`;
              return <div className="org-document-submissions__row" key={id}>
                <div className="org-document-submissions__main"><strong>{name}</strong><small>{submission.scopeName} · {submission.byteSize ? readableBytes(submission.byteSize) : "文件"}</small>{submission.reviewNote && <em>{submission.reviewNote}</em>}</div>
                <div className="org-document-submissions__actions"><span className={`org-state org-state--${submission.state === "approved" ? "ready" : submission.scanStatus ?? submission.state}`}>{documentSubmissionLabel(submission)}</span>{submission.scanStatus === "passed" && <><button onClick={() => void showDocumentPreview(id, fileName, submission.sourceType ?? "txt", true, submission.resultDocumentId)} disabled={previewLoading}><Eye size={13} />查看</button><button onClick={() => void downloadDocument(id, fileName, true)}><Download size={13} />下载</button></>}<button onClick={() => setRemoveTarget({ id, name, submission: true })} disabled={busy || !id}><Trash2 size={13} />删除</button></div>
              </div>;
            })}
            {documentSubmissions.length < submissionTotal && <div className="org-document-more"><button onClick={() => void loadMoreSubmissions()} disabled={busy}>加载更多提交（{documentSubmissions.length}/{submissionTotal}）</button></div>}
          </div>
        </section>
      )}

      {tab === "skills" && (
        <section className="org-library org-skill-library">
          <div className="org-library__toolbar org-skill-library__toolbar">
            <div className="org-skill-library__summary"><strong>已发布</strong><span>{visibleSkills.length}</span></div>
            <div className="org-library__actions org-skill-library__actions">
              <label className="org-library__scope-field org-skill-library__scope-field"><span>提交到</span><select aria-label="Skill 提交范围" value={writeScope} onChange={(event) => setWriteScope(event.target.value)}><option value="">选择提交范围</option>{scopes.map((scope) => <option key={scope.id} value={scope.id}>{scopeLabel(scope.kind)} · {scope.name}</option>)}</select></label>
              {skills.some((skill) => skill.scopeKind === "personal") && scopes.some((scope) => scope.kind !== "personal") && <label className="org-library__scope-field org-skill-library__scope-field"><span>副本发布到</span><select aria-label="Skill 副本发布目标" value={publishScope} onChange={(event) => setPublishScope(event.target.value)}><option value="">选择副本发布目标</option>{scopes.filter((scope) => scope.kind !== "personal").map((scope) => <option key={scope.id} value={scope.id}>{scopeLabel(scope.kind)} · {scope.name}</option>)}</select></label>}
              <button onClick={() => void syncSkills()} disabled={busy}><RefreshCw size={15} />安全同步</button><button className="org-memory__primary" onClick={() => void pickAndUploadSkill()} disabled={busy || !uploadScope || !allowSkillSubmission}><Upload size={15} />提交 Skill</button>
            </div>
          </div>
          <div className="org-skill-grid">
            {visibleSkills.length === 0 && <div className="org-skill-library__empty"><span className="org-skill-library__empty-icon"><PackageOpen size={20} /></span><strong>暂无已发布的 Skill</strong><span>提交并通过审核后会显示在这里</span></div>}
            {visibleSkills.map((skill) => (
              <article className="org-skill-card" key={skill.skillId}>
                <div><strong>{skill.name}</strong><span>v{skill.version}</span></div>
                <p>{skill.description}</p>
                <footer><span className={`org-scope-badge org-scope-badge--${skill.scopeKind}`}><ScopeIcon kind={skill.scopeKind} />{skill.scopeName}</span>{skill.scopeKind === "personal" && <button onClick={() => void publishSkill(skill)} disabled={busy || !publishScope}>发布副本</button>}{skill.mandatory ? <span className="org-state org-state--ready">组织强制 · 已安装</span> : <button onClick={() => void toggleSkill(skill)} disabled={busy}>{skill.enabled ? "卸载" : "安装"}</button>}</footer>
              </article>
            ))}
          </div>
          <SubmissionList title="我的 Skill 提交" submissions={visibleSkillSubmissions} />
        </section>
      )}
          {preview && <div ref={previewDialogRef} tabIndex={-1} onClick={(event) => { if (event.target === event.currentTarget) setPreview(null); }} className="org-document-modal" role="dialog" aria-modal="true" aria-label={`查看文档 ${preview.name}`}>
            <div className="org-document-modal__panel">
              <header><div><strong>{preview.name}</strong>{preview.parsed && <small>内容来自知识索引，原件排版请下载查看</small>}</div><div className="org-document-modal__header-actions">{!preview.unavailableReason && <button onClick={() => void downloadDocument(preview.resourceId, preview.name, preview.submission)}><Download size={14} />下载原件</button>}<button data-modal-initial-focus aria-label="关闭预览" onClick={() => setPreview(null)}><X size={18} /></button></div></header>
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
        </main>
      </div>
    </div>
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
  return <div className="org-submissions"><h3>{title}</h3>{submissions.slice(0, 8).map((submission) => <div key={submission.id ?? submission.submissionId}><span>{submission.title ?? submission.name} {submission.version ? `v${submission.version}` : ""}</span><small>{submission.scopeName}</small>{submission.scanStatus && <span className={`org-state org-state--${submission.scanStatus}`}>扫描：{stateLabel(submission.scanStatus)}</span>}<span className={`org-state org-state--${submission.state}`}>{stateLabel(submission.state)}</span>{submission.reviewNote && <em>{submission.reviewNote}</em>}{submission.scanReport?.findings.filter((item) => item.severity !== "info").map((item) => <em key={`${item.code}-${item.path ?? ""}`}>{item.code}：{item.message}{item.path ? ` (${item.path})` : ""}</em>)}</div>)}</div>;
}

function MemoryPromotionList({ promotions }: { promotions: MemoryPromotion[] }) {
  if (promotions.length === 0) return null;
  return <div className="org-submissions"><h3>我的经验提交</h3>{promotions.slice(0, 8).map((item) => <div key={item.id}><span>{item.payload.content ?? "经验候选"}</span><small>{item.scopeName} · {item.payload.kind ? memoryKindLabel[item.payload.kind] : "经验"}</small><span className={`org-state org-state--${item.state}`}>{stateLabel(item.state)}</span>{item.reviewNote && <em>{item.reviewNote}</em>}</div>)}</div>;
}
