import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { X } from "lucide-react";
import { useModalFocus } from "@/lib/use-modal-focus";
import { orgFetchDocument } from "@/lib/org-client";
import { captureTeamProjectOperation, teamProjectRequest, type ProjectAnswer, type ProjectCitation, type TeamProjectSnapshot } from "@/lib/team-project-client";
import type { ProjectWorkspace } from "@/lib/use-project-workspace";
import { useOrgSessionStore } from "@/stores/org-session-store";
import { useSessionsStore } from "@/stores/sessions-store";
import { Markdown } from "./markdown/Markdown";

const BRIEFING = "我是新加入的项目成员。请说明项目目标、当前进展、有效方案和理由、待验收与阻塞事项、我应该先接手什么，并引用依据。";

/** Answers and original evidence remain memory-only, bound to account and revision. */
export function ProjectQuestionsDialog({ open, projectId, draftKey, workspace, onClose }: {
  open: boolean; projectId: string; draftKey: string; workspace: ProjectWorkspace; onClose: () => void;
}) {
  const boundary = useOrgSessionStore(s => s.boundaryRevision);
  return <ProjectQuestions key={`${boundary}:${projectId}`} open={open} projectId={projectId} draftKey={draftKey} workspace={workspace} onClose={onClose} />;
}

function ProjectQuestions({ open, projectId, draftKey, workspace, onClose }: {
  open: boolean; projectId: string; draftKey: string; workspace: ProjectWorkspace; onClose: () => void;
}) {
  const question = useSessionsStore(s => s.drafts[draftKey] ?? "");
  const [answer, setAnswer] = useState<ProjectAnswer | null>(null);
  const [preview, setPreview] = useState<{ title: string; text: string } | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const operation = useRef(false);
  const mounted = useRef(true);
  const snapshot = useRef(workspace.snapshot); snapshot.current = workspace.snapshot;
  const focus = useModalFocus<HTMLDivElement>(open, onClose);
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, []);
  const visibleAnswer = workspace.snapshot && answer?.projectRevision === workspace.snapshot.revision ? answer : null;
  useEffect(() => {
    if (answer && !visibleAnswer) {
      setAnswer(null); setPreview(null);
      setNotice(workspace.snapshot ? "项目内容已更新，请重新提问以获取最新依据。" : "项目连接不可用，恢复连接后可重新查询。");
    }
  }, [answer, visibleAnswer, workspace.snapshot]);

  const run = async (work: (identity: ReturnType<typeof captureTeamProjectOperation>) => Promise<void>) => {
    if (operation.current) return;
    operation.current = true; setBusy(true); setError(""); setNotice("");
    try {
      if (!snapshot.current) throw new Error("项目连接不可用，请刷新项目后重试。");
      const identity = captureTeamProjectOperation();
      await work(identity);
    } catch (failure) {
      if (mounted.current) { setError(String(failure).replace(/^Error:\s*/, "")); setAnswer(null); setPreview(null); void workspace.refresh(); }
    } finally {
      operation.current = false; if (mounted.current) setBusy(false);
    }
  };
  const checkRevision = async (revision: number, identity: ReturnType<typeof captureTeamProjectOperation>) => {
    const latest = await teamProjectRequest<TeamProjectSnapshot>("snapshot", projectId, undefined, undefined, identity);
    identity.assertCurrent();
    if (!mounted.current) return false;
    if (!snapshot.current || latest.id !== projectId || latest.revision !== revision || snapshot.current.revision !== revision) {
      void workspace.refresh();
      throw new Error("项目依据已更新或访问已变化，请刷新并重新提问。");
    }
    return true;
  };
  const ask = (text: string) => run(async identity => {
    setAnswer(null); setPreview(null);
    const value = await teamProjectRequest<ProjectAnswer>("ask", projectId, { question: text }, undefined, identity);
    identity.assertCurrent();
    if (!value || typeof value.answer !== "string" || !Number.isInteger(value.projectRevision) || !Array.isArray(value.citations) || !Array.isArray(value.gaps)) throw new Error("项目问答结果格式不兼容，请联系管理员更新服务。");
    if (await checkRevision(value.projectRevision, identity)) setAnswer(value);
  });
  const openCitation = (citation: ProjectCitation) => run(async identity => {
    if (!visibleAnswer || !await checkRevision(visibleAnswer.projectRevision, identity)) return;
    const text = citation.kind.startsWith("project") ? citation.quote
      : (await orgFetchDocument(citation.resourceId, undefined, undefined, identity.expectedGeneration)).text;
    identity.assertCurrent();
    if (await checkRevision(visibleAnswer.projectRevision, identity)) setPreview({ title: citation.title, text });
  });

  if (!open) return null;
  return createPortal(<div className="modal-overlay" onClick={onClose}>
    <div ref={focus} className="create-colleague-dialog project-questions-dialog" role="dialog" aria-modal="true" aria-label="问项目" tabIndex={-1} onClick={e => e.stopPropagation()}>
      <div className="create-colleague-header"><h3>问项目</h3><button type="button" className="create-colleague-close" aria-label="关闭项目问答" onClick={onClose}><X size={18} /></button></div>
      <div className="create-colleague-body project-questions-body">
        <p className="project-workspace-hint">查询共享资料、任务进展与已确认的决定。查看者也可提问，查询不会创建执行任务。</p>
        <button type="button" className="pd-btn" disabled={busy || !workspace.snapshot} onClick={() => void ask(BRIEFING)}>新成员快速了解项目</button>
        <form onSubmit={e => { e.preventDefault(); if (question.trim()) void ask(question.trim()); }}>
          <label className="project-settings-field">项目问题<textarea rows={3} maxLength={8000} value={question} disabled={busy} placeholder="项目做到哪里了？为什么采用当前方案？" onChange={e => useSessionsStore.getState().setDraft(draftKey, e.target.value)} data-modal-initial-focus /></label>
          <button type="submit" className="pd-btn pd-btn--primary" disabled={busy || !workspace.snapshot || !question.trim()}>查询项目</button>
        </form>
        {busy && <p role="status">正在核对项目依据…</p>}
        {notice && <p role="status">{notice}</p>}
        {error && <p role="alert" className="project-inline-error">{error}</p>}
        {!workspace.snapshot && <p role="alert" className="project-inline-error">{workspace.error || "项目连接不可用，请刷新项目后重试。"}<button type="button" className="pd-btn" disabled={workspace.loading} onClick={() => void workspace.refresh()}>刷新项目</button></p>}
        {visibleAnswer && <article aria-label="项目问答结果" className="project-question-answer">
          <p className="project-workspace-hint">依据共享项目版本 {visibleAnswer.projectRevision} · {visibleAnswer.verification === "supported" ? "经来源核验" : "来源摘录，尚未形成完整回答"}</p>
          <Markdown complete>{visibleAnswer.answer}</Markdown>
          {(visibleAnswer.insufficient || visibleAnswer.gaps.length > 0) && <div className="project-question-gaps"><strong>当前依据仍有不足</strong>{visibleAnswer.gaps.length > 0 && <ul>{visibleAnswer.gaps.map((gap, i) => <li key={i}>{gap}</li>)}</ul>}</div>}
          {!!visibleAnswer.citations.length && <div className="project-question-sources" aria-label="回答来源">{visibleAnswer.citations.map(c => <button type="button" className="pd-btn" key={c.id} disabled={busy} onClick={() => void openCitation(c)}>{c.id} · {c.title}</button>)}</div>}
          {preview && <section aria-label="项目来源原文" className="project-question-source"><h4>{preview.title}</h4><Markdown complete>{preview.text}</Markdown></section>}
        </article>}
      </div>
    </div>
  </div>, document.body);
}
