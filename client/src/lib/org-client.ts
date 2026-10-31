import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type OrgScopeKind = "personal" | "team" | "org";

export interface OrgScope {
  id: string;
  kind: OrgScopeKind;
  name: string;
  groupId?: string | null;
  ownerUserId?: string | null;
  canPublishDocuments?: boolean;
}

export interface OrgUser {
  id: string;
  username: string;
  displayName: string;
  role: "member" | "curator" | "admin";
  clearance: number;
}

export interface OrgBootstrap {
  apiVersion: number;
  user: OrgUser;
  scopes: OrgScope[];
  policy: Record<string, unknown>;
  serverTime: number;
}

export interface OrgSession {
  loggedIn: boolean;
  /** Whether Agent sessions currently receive organization-memory tools. */
  organizationMemoryEnabled?: boolean;
  serverUrl?: string;
  /** Non-secret account hint available even when authentication must be renewed. */
  username?: string;
  /** The saved refresh credential is missing, invalid, or was rejected. */
  requiresReauthentication?: boolean;
  user?: OrgUser;
  bootstrap?: OrgBootstrap;
}

export interface OrgDocument {
  id: string;
  title: string;
  sourceType: string;
  status: "pending" | "parsing" | "chunking" | "embedding" | "ready" | "failed" | "archived";
  failReason?: string | null;
  byteSize: number;
  scopeId: string;
  scopeKind: OrgScopeKind;
  scopeName: string;
  ownerName?: string | null;
  ownerId?: string | null;
  chunkCount: number;
  tags: string[];
  updatedAt: number;
}

export interface DocumentPage {
  items: OrgDocument[];
  total: number;
  page: number;
  size: number;
}

export type OrgLibraryResource = "memories" | "documents" | "skills" | "memory-submissions" | "document-submissions" | "skill-submissions";
export interface OrgLibraryQuery { scopeId: string; query: string; filter: string; page: number; size: number }
export interface OrgLibraryPage<T> { items: T[]; total: number; page: number; size: number }
export interface OrgLibrarySummary {
  memories: number; documents: number; documentReady: number; documentProcessing: number;
  skills: number; skillInstalled: number; skillMandatory: number;
  memoryPending: number; documentPending: number; skillPending: number;
}
export const orgLibraryQuery = <T>(resource: OrgLibraryResource, query: OrgLibraryQuery) =>
  invoke<OrgLibraryPage<T>>("org_library_query", { resource, ...query });
export const orgLibrarySummary = () => invoke<OrgLibrarySummary>("org_library_query", { resource: "summary" });

export interface Submission {
  id?: string;
  submissionId?: string;
  title?: string;
  name?: string;
  version?: string;
  state: "pending" | "approved" | "rejected" | "withdrawn" | "revoked";
  scopeId: string;
  scopeName: string;
  scopeKind: OrgScopeKind;
  reviewNote?: string | null;
  scanStatus?: "queued" | "scanning" | "passed" | "failed";
  scanReport?: {
    status: "passed" | "failed";
    findings: Array<{ code: string; severity: string; message: string; path?: string }>;
  } | null;
  resultDocumentId?: string | null;
  sourceType?: string;
  byteSize?: number;
  canDeletePublished?: number | boolean;
  createdAt: number;
}

export interface OrgSkill {
  skillId: string;
  versionId: string;
  name: string;
  description: string;
  version: string;
  scopeId: string;
  scopeKind: OrgScopeKind;
  scopeName: string;
  mandatory: boolean;
  allowPersonalOverride: boolean;
  enabled: boolean;
  updatedAt: number;
}

export type OrgMemoryKind = "fact" | "decision" | "convention" | "pitfall" | "howto";

export interface OrgMemory {
  id: string;
  kind: OrgMemoryKind;
  content: string;
  rationale?: string | null;
  evidence: Array<{ type?: string; id?: string; loc?: string }>;
  confidence: number;
  observedAt?: number | null;
  validFrom?: number | null;
  validUntil?: number | null;
  workspaceRef?: string | null;
  outcome?: string | null;
  trust: "reported" | "reviewed" | "verified";
  stale: boolean;
  status: "active" | "superseded" | "retired";
  scopeId: string;
  scopeKind: OrgScopeKind;
  scopeName: string;
  authorName?: string | null;
  updatedAt: number;
}

export interface MemoryPromotion {
  id: string;
  payloadType: "memory" | "document";
  payload: Partial<OrgMemory> & { kind?: OrgMemoryKind; content?: string };
  source: string;
  state: "pending" | "approved" | "rejected" | "withdrawn";
  scopeName: string;
  scopeKind: OrgScopeKind;
  reviewNote?: string | null;
  createdAt: number;
}

export interface SkillSyncResult {
  cursor: string;
  leaseUntil: number;
  installed: Array<{
    skillId: string;
    versionId: string;
    name: string;
    path: string;
    mandatory: boolean;
    allowPersonalOverride: boolean;
  }>;
}

export const orgSession = () => invoke<OrgSession>("org_session");
export interface OrgModelSyncResult {
  configured: boolean;
  modelId?: string | null;
  syncedAt: number;
}
export const orgSyncModelConfig = () =>
  invoke<OrgModelSyncResult>("org_sync_model_config");
export const orgLogin = (serverUrl: string, username: string, password: string) =>
  invoke<OrgSession>("org_login", { serverUrl, username, password });
export const orgLogout = () => invoke<void>("org_logout");
export const orgListScopes = () => invoke<OrgScope[]>("org_list_scopes");
export const orgListDocuments = (scopeId?: string, query?: string, page = 1, size = 20) =>
  invoke<DocumentPage>("org_list_documents", { scopeId: scopeId ?? null, query: query ?? null, page, size });
export const orgListMemories = (scopeId?: string, kind?: OrgMemoryKind, query?: string) =>
  invoke<OrgMemory[]>("org_list_memories", {
    scopeId: scopeId ?? null,
    kind: kind ?? null,
    query: query ?? null,
  });
export const orgMemoryPromotionsMine = () =>
  invoke<MemoryPromotion[]>("org_memory_promotions_mine");
export const orgSubmitMemoryCandidate = (input: {
  targetScopeId: string;
  kind: OrgMemoryKind;
  content: string;
  rationale?: string;
  outcome?: string;
  workspaceRef?: string;
  validUntil?: number;
  sensitivity?: number;
}) => invoke<{ promotionId: string; state: string }>("org_submit_memory_candidate", {
  ...input,
  rationale: input.rationale || null,
  outcome: input.outcome || null,
  workspaceRef: input.workspaceRef || null,
  validUntil: input.validUntil ?? null,
  sensitivity: input.sensitivity ?? 0,
});
export const orgSubmitDocument = (filePath: string, scopeId: string, title?: string, tags?: string[]) =>
  invoke<{ submissionId?: string | null; docId?: string | null; state: string; documentStatus?: string; dedup?: boolean }>(
    "org_submit_document",
    { filePath, scopeId, title: title ?? null, tags: tags ?? null },
  );
export const orgDocumentSubmissionsMine = () =>
  invoke<Submission[]>("org_document_submissions_mine");
export const orgDocumentSubmissionsMinePage = (page = 1, size = 20) =>
  invoke<{ items: Submission[]; total: number; page: number; size: number }>("org_document_submissions_mine_page", { page, size });
export const orgRemoveOwnSubmission = (submissionId: string) =>
  invoke<{ removed: boolean }>("org_remove_own_submission", { submissionId });
export const orgPreviewDocument = (resourceId: string, submission: boolean, sourceType: string, fileName: string) =>
  invoke<{ kind: "text" | "binary"; content: string }>("org_preview_document", { resourceId, submission, sourceType, fileName });
export const orgDownloadDocument = (resourceId: string, submission: boolean, suggestedName: string) =>
  invoke<string | null>("org_download_document", { resourceId, submission, suggestedName });
export interface OrgFolderFile { path: string; relativePath: string; size: number }
export const orgScanDocumentFolder = (folderPath: string) =>
  invoke<{ items: OrgFolderFile[]; skipped: number; totalBytes: number }>("org_scan_document_folder", { folderPath });
export const orgFetchDocument = (docId: string, page?: number | null, range?: string) =>
  invoke<{ docId: string; text: string; chunks: Array<{ seq: number; text: string; heading?: string; locPage?: number }>; hasMore?: boolean; nextSeq?: number | null }>(
    "org_fetch_document",
    { docId, page: page ?? null, range: range ?? null },
  );
export const orgArchiveDocument = (docId: string) =>
  invoke<{ archived: boolean }>("org_archive_document", { docId });
export const orgNewDocumentVersion = (docId: string, filePath: string) =>
  invoke<{ docId: string; version: number; status: string }>("org_new_document_version", { docId, filePath });
export const orgPublishDocument = (docId: string, targetScopeId: string) =>
  invoke<{ submissionId?: string | null; docId?: string | null; state: string }>("org_publish_document", { docId, targetScopeId });
export const orgListSkills = () => invoke<OrgSkill[]>("org_list_skills");
export const orgSkillDetail = (skillId: string) => invoke<Record<string, unknown>>("org_skill_detail", { skillId });
export const orgSetSkillPreference = (skillId: string, enabled: boolean) =>
  invoke<{ skillId: string; enabled: boolean }>("org_set_skill_preference", { skillId, enabled });
export const orgPublishSkill = (skillId: string, targetScopeId: string) =>
  invoke<{ submissionId: string; skillId: string; version: string; state: string }>("org_publish_skill", { skillId, targetScopeId });
export const orgSubmitSkill = (filePath: string, scopeId: string, version?: string) =>
  invoke<{ submissionId: string; skillId: string; version: string; state: string }>(
    "org_submit_skill",
    { filePath, scopeId, version: version ?? null },
  );
export const orgSkillSubmissionsMine = () => invoke<Submission[]>("org_skill_submissions_mine");
export const orgSyncSkills = () => invoke<SkillSyncResult>("org_sync_skills");
export const orgQaFeedback = (qaEventId: string, feedback: "helpful" | "not_helpful" | "wrong") =>
  invoke<unknown>("org_qa_feedback", { qaEventId, feedback });

/** Fired after native organization Skill activation/deactivation is persisted. */
export const listenOrgSkillsChanged = (handler: () => void): Promise<UnlistenFn> =>
  listen("org://skills-changed", handler);

export const listenOrgModelsChanged = (handler: () => void): Promise<UnlistenFn> =>
  listen("org://models-changed", handler);

export interface OrgSessionChangedEvent {
  reason: string;
  session: OrgSession;
}

/** Native auth/health changes, including silent Agent-context degradation. */
export const listenOrgSessionChanged = (
  handler: (event: OrgSessionChangedEvent) => void,
): Promise<UnlistenFn> =>
  listen<OrgSessionChangedEvent>("org://session-changed", ({ payload }) => handler(payload));
