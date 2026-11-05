import { groupPublicSeq } from "./group-presentation";
import { create } from "zustand";
import { useEffect } from "react";
import { groupCleanupAck, groupCleanupPending, groupDelete, groupList, groupSnapshot, onGroupChanged, type Group, type TeamTemplate } from "./group-client";
import { readDurable, writeDurable, isStringMap, useStorageHealth } from "@/lib/durable-ui-state";
import { invalidateAgentKnowledgeSourceSync, type RuntimeTeamInfo } from "@/lib/agent-client";
import { touchDraft } from "@/lib/draft-lifecycle";
import { saveDraftAttachments } from "@/lib/draft-attachments";
import { useKnowledgeStore } from "@/stores/knowledge-store";
import { useSessionStore } from "@/stores/session-store";
import { usePermissionStore } from "@/stores/permission-store";
import { useQuestionStore } from "@/stores/question-store";
import { usePermissionModeStore } from "@/stores/permission-mode-store";
import { useMessageQueueStore } from "@/stores/message-queue-store";
import { useSubagentStore } from "@/stores/subagent-store";
import { useSessionsStore } from "@/stores/sessions-store";

export interface GroupDraftOptions { targets: string[]; discussion: boolean; replyTo: string | null; newTopic?: boolean; requestId?: string | null; newRound?: boolean; sourceAttachments?: { requestId: string; paths: string[] } }
export interface GroupCreationSource { sessionId: string; team: Pick<RuntimeTeamInfo, "teamId" | "description" | "members" | "roles"> & Partial<RuntimeTeamInfo>; referenceText?: string }
interface GroupState {
  groups: Group[]; currentId: string | null; current: Group | null; error: string | null; loading: boolean;
  createFrom: GroupCreationSource | null;
  createTemplate: TeamTemplate | null;
  drafts: Record<string, string>;
  outbox: Record<string, string>;
  requestId: (id: string, fingerprint: string) => string;
  acknowledgeSend: (id: string) => void;
  readRevisions: Record<string, number>;
  markRead: (id: string, revision: number) => void;
  cleanupError: string | null;
  retryCleanup: () => Promise<void>;
  forget: (id: string, sessionIds?: string[]) => boolean;
  remove: (id: string, stopWork: boolean) => Promise<void>;
  refresh: () => Promise<void>; select: (id: string | null) => Promise<void>;
  fromTeam: (sessionId: string, team: RuntimeTeamInfo, referenceText?: string) => void;
  setDraft: (id: string, text: string) => void;
  clearCreationDraft: () => void;
  draftOptions: (id: string) => GroupDraftOptions;
  setDraftOptions: (id: string, options: GroupDraftOptions) => void;
}
let listGeneration = 0, selectionGeneration = 0;
const deletedGroups = new Set<string>();
const pendingCleanup = new Map<string, string[]>();
let cleanupRunning: Promise<void> | null = null;
function submissionFingerprint(value: string): string {
  try {
    const input = JSON.parse(value);
    if (input && typeof input === "object" && !Array.isArray(input)) {
      return JSON.stringify({...input,newTopic:input.newTopic ?? false,selectedRequest:input.selectedRequest ?? null});
    }
  } catch { /* Older callers may use an opaque fingerprint. */ }
  return value;
}
export const useGroupStore = create<GroupState>((set, get) => ({
  groups: [], currentId: null, current: null, error: null, loading: false, createFrom: null, createTemplate: null, cleanupError: null,
  drafts: readDurable("echoagent.group-drafts.v1", {}, isStringMap),
  outbox: readDurable("echoagent.group-outbox.v1", {}, isStringMap),
  requestId: (id, fingerprint) => {
    if (deletedGroups.has(id)) throw new Error("专家团已删除，无法继续发送");
    try {
      const pending = JSON.parse(get().outbox[id] ?? "null");
      if (typeof pending?.fingerprint === "string" && submissionFingerprint(pending.fingerprint) === submissionFingerprint(fingerprint) && typeof pending.id === "string") return pending.id;
    } catch { /* Replace an invalid pending record before sending. */ }
    const requestId = crypto.randomUUID();
    const outbox = { ...get().outbox, [id]: JSON.stringify({ id: requestId, fingerprint }) };
    if (!writeDurable("echoagent.group-outbox.v1", outbox)) throw new Error("暂时无法保存待发送消息，请释放存储空间后重试");
    set({ outbox });
    return requestId;
  },
  acknowledgeSend: (id) => {
    const outbox = { ...get().outbox }; delete outbox[id];
    writeDurable("echoagent.group-outbox.v1", outbox); set({ outbox });
  },
  readRevisions: readDurable<Record<string, number>>("echoagent.group-read.v3", {}, (value): value is Record<string, number> => !!value && typeof value === "object" && !Array.isArray(value) && Object.values(value).every(value => typeof value === "number" && Number.isFinite(value))),
  markRead: (id, revision) => { if (deletedGroups.has(id)) return; if ((get().readRevisions[id] ?? 0) >= revision) return; const readRevisions = { ...get().readRevisions, [id]: revision }; if (writeDurable("echoagent.group-read.v3", readRevisions)) set({ readRevisions }); },
  forget: (id, sessionIds = []) => {
    deletedGroups.add(id);
    ++listGeneration;
    const state = get();
    const local = state.current?.id === id ? state.current : state.groups.find(g => g.id === id);
    const sessions = new Set([...sessionIds, ...(local?.members.flatMap(m => [m.sessionId, m.discussionSessionId].filter((sid): sid is string => !!sid)) ?? [])]);
    const drafts = { ...state.drafts }, outbox = { ...state.outbox }, readRevisions = { ...state.readRevisions };
    delete drafts[id]; delete drafts[`options:${id}`]; delete outbox[id]; delete readRevisions[id];
    const receipt = [...sessions];
    pendingCleanup.set(id, receipt);
    const writes = [writeDurable("echoagent.group-drafts.v1", drafts), writeDurable("echoagent.group-outbox.v1", outbox), writeDurable("echoagent.group-read.v3", readRevisions)];
    const selected = state.currentId === id;
    if (selected) ++selectionGeneration;
    set({ groups: state.groups.filter(g => g.id !== id), drafts, outbox, readRevisions, ...(selected ? { currentId: null, current: null, loading: false, error: null } : {}) });
    for (const sid of [`group:${id}`, ...sessions]) {
      writes.push(saveDraftAttachments(sid, []));
      touchDraft(sid);
      invalidateAgentKnowledgeSourceSync(sid);
      writes.push(useKnowledgeStore.getState().forgetSession(sid));
      useSessionStore.getState().dropSessionCache(sid);
      usePermissionStore.getState().clearSession(sid);
      useQuestionStore.getState().clearSession(sid);
      usePermissionModeStore.getState().clearSession(sid);
      useMessageQueueStore.getState().clear(sid);
      useSubagentStore.getState().clearSession(sid);
    }
    for (const sid of sessions) useSessionsStore.getState().remove(sid);
    const errors = useStorageHealth.getState().errors;
    const saved = writes.every(Boolean) && !errors["echoagent.knowledge-preferences.v3"];
    if (!saved) set({ cleanupError: "专家团已删除，本机草稿清理尚未完成。存储恢复后可重试，重启也会继续清理。" });
    else void groupCleanupAck(id).then(() => {
      // A newer failed cleanup must not be acknowledged by an older attempt.
      if (pendingCleanup.get(id) === receipt) pendingCleanup.delete(id);
      if (!pendingCleanup.size) set({ cleanupError: null });
    }).catch(() => set({ cleanupError: "本机草稿已清理，清理进度尚未保存，请重试。" }));
    return saved;
  },
  retryCleanup: () => {
    if (cleanupRunning) return cleanupRunning;
    cleanupRunning = (async () => {
      try {
        const receipts = await groupCleanupPending();
        if (!Array.isArray(receipts)) throw new Error("清理记录暂时不可读");
        for (const receipt of receipts) pendingCleanup.set(receipt.groupId, receipt.sessionIds);
        for (const [id,sessions] of [...pendingCleanup]) get().forget(id,sessions);
      } catch (error) { set({ cleanupError: `本机清理尚未完成：${String(error).replace(/^Error:\s*/, "")}` }); }
    })().finally(() => { cleanupRunning = null; });
    return cleanupRunning;
  },
  remove: async (id, stopWork) => {
    const result = await groupDelete(id, stopWork);
    get().forget(id, result.sessionIds);
  },
  refresh: async () => {
    const generation = ++listGeneration;
    try {
      const groups = await groupList();
      if (!Array.isArray(groups)) throw new Error("专家团列表未返回有效数据，请稍后重试");
      if (generation !== listGeneration) return;
      set({ groups: groups.filter(group => !deletedGroups.has(group.id)), error: null });
    } catch (error) { if (generation === listGeneration) set({ error: String(error).replace(/^Error:\s*/, "") }); }
  },
  select: async (id) => {
    const generation = ++selectionGeneration;
    if (id && deletedGroups.has(id)) return;
    set({ currentId: id, ...(get().current?.id === id ? {} : { current: null }), loading: !!id, createFrom: null, createTemplate: null });
    if (!id) return;
    try {
      const current = await groupSnapshot(id);
      if (!current || current.id !== id || !Array.isArray(current.messages) || !Array.isArray(current.members)) throw new Error("专家团记录未返回有效数据，请稍后重试");
      if (generation === selectionGeneration) {
        const previous = get().current;
        current.publicSeq ??= groupPublicSeq(current);
        if (previous?.id === current.id) {
          const messages = new Map(previous.messages.map(message => [message.id, message]));
          current.messages = current.messages.map(message => {
            const old = messages.get(message.id);
            return old && JSON.stringify(old) === JSON.stringify(message) ? old : message;
          });
        }
        set({ current, loading: false, error: null });
      }
    } catch (error) { if (generation === selectionGeneration) set({ loading: false, error: String(error).replace(/^Error:\s*/, "") }); }
  },
  fromTeam: (sessionId, team, referenceText) => {
    const existing = get().groups.find(group => group.sourceSessionId === sessionId && group.sourceTeamId === team.teamId);
    if (existing) { void get().select(existing.id); return; }
    ++selectionGeneration;
    set({ currentId: null, current: null, createTemplate: null, createFrom: { sessionId, team, referenceText } });
  },
  setDraft: (id, text) => {
    if (deletedGroups.has(id) || (id.startsWith("options:") && deletedGroups.has(id.slice(8)))) return;
    const drafts = { ...get().drafts, [id]: text };
    if (writeDurable("echoagent.group-drafts.v1", drafts)) set({ drafts });
  },
  clearCreationDraft: () => {
    const drafts = { ...get().drafts };
    delete drafts["create:pending"];
    if (writeDurable("echoagent.group-drafts.v1", drafts)) set({ drafts });
  },
  draftOptions: (id) => {
    const raw = get().drafts[`options:${id}`];
    if (raw) try {
      const value = JSON.parse(raw);
      if (Array.isArray(value.targets) && value.targets.every((target: unknown) => typeof target === "string") && typeof value.discussion === "boolean" && (value.replyTo === null || typeof value.replyTo === "string") && (value.newTopic === undefined || typeof value.newTopic === "boolean") && (value.requestId == null || typeof value.requestId === "string")
        && (value.newRound === undefined || typeof value.newRound === "boolean")
        && (value.sourceAttachments === undefined || (value.sourceAttachments && typeof value.sourceAttachments.requestId === "string" && Array.isArray(value.sourceAttachments.paths) && value.sourceAttachments.paths.every((path: unknown) => typeof path === "string")))) return value;
    } catch { /* Preserve the draft text even if its optional selection is invalid. */ }
    return { targets: [], discussion: false, replyTo: null };
  },
  setDraftOptions: (id, options) => {
    const key = `options:${id}`, raw = JSON.stringify(options);
    if (get().drafts[key] !== raw) get().setDraft(key, raw);
  },
}));

/** Mounted by App, so navigating away does not disconnect group updates. */
export function useGroupEvents(ready: boolean) {
  useEffect(() => {
    if (!ready) return;
    let closed = false, timer: ReturnType<typeof setTimeout> | undefined;
    let unlisten: (() => void) | undefined;
    const dirty = new Set<string>();
    void useGroupStore.getState().refresh();
    void useGroupStore.getState().retryCleanup();
    const retry = () => { void useGroupStore.getState().retryCleanup(); };
    window.addEventListener("focus",retry);
    void onGroupChanged((id, deleted, sessionIds) => {
      if (deleted) { useGroupStore.getState().forget(id, sessionIds); dirty.delete(id); return; }
      if (deletedGroups.has(id)) return;
      dirty.add(id);
      timer ??= setTimeout(() => {
        timer = undefined;
        if (closed) return;
        const state = useGroupStore.getState();
        void state.refresh();
        if (state.currentId && dirty.has(state.currentId)) void state.select(state.currentId);
        dirty.clear();
      }, 100);
    }).then(stop => { if (closed) stop(); else unlisten = stop; }).catch(error => useGroupStore.setState({ error: String(error) }));
    return () => { closed = true; unlisten?.(); window.removeEventListener("focus",retry); if (timer) clearTimeout(timer); };
  }, [ready]);
}
