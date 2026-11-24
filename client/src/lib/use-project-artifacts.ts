import { useEffect, useMemo, useState } from "react";
import { useSessionStore } from "@/stores/session-store";
import { useSessionsStore } from "@/stores/sessions-store";
import { projectOwnsSession, type ProjectMeta } from "@/stores/projects-store";
import { ARTIFACT_CATALOG_EVENT, artifactsFromTranscripts, loadDocumentArtifactCatalog, loadTaskArtifacts, mergeTaskArtifacts, type TaskArtifact } from "./artifact-catalog";

/** The native export catalog survives renderer-cache loss; live tools supplement it. */
export function useProjectArtifacts(project: ProjectMeta) {
  const [cached, setCached] = useState(loadTaskArtifacts);
  const [native, setNative] = useState<TaskArtifact[]>([]);
  const [error, setError] = useState("");
  const [attempt, setAttempt] = useState(0);
  const transcripts = useSessionStore(s => s.transcripts);
  const summaries = useSessionsStore(s => s.independent);
  useEffect(() => {
    let active = true;
    let latestRequest = 0;
    const refresh = async () => {
      const request = ++latestRequest;
      setCached(loadTaskArtifacts());
      try {
        const items = await loadDocumentArtifactCatalog();
        if (!active || request !== latestRequest) return;
        setNative(items);
        setError("");
      } catch (error) {
        if (active && request === latestRequest) setError(String(error).replace(/^Error:\s*/, ""));
      }
    };
    void refresh();
    window.addEventListener(ARTIFACT_CATALOG_EVENT, refresh);
    window.addEventListener("focus", refresh);
    return () => { active = false; window.removeEventListener(ARTIFACT_CATALOG_EVENT, refresh); window.removeEventListener("focus", refresh); };
  }, [project.id, attempt]);
  const artifacts = useMemo(() => {
    const owned = Object.fromEntries(Object.entries(transcripts).filter(([id]) => projectOwnsSession(project, id)));
    const meta = Object.fromEntries(summaries.map(s => [s.sessionId, { title: s.title, cwd: s.cwd, updatedAt: s.updatedAt ? Date.parse(s.updatedAt) : undefined }]));
    return mergeTaskArtifacts(cached, native, artifactsFromTranscripts(owned, meta)).filter(a => projectOwnsSession(project, a.sessionId));
  }, [project, cached, native, transcripts, summaries]);
  return { artifacts, error, refresh: () => setAttempt(n => n + 1) };
}
