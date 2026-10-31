import { useCallback, useEffect, useRef, useState } from "react";
import {
  orgLibraryQuery, orgLibrarySummary, type OrgLibraryPage, type OrgLibraryQuery,
  type OrgLibraryResource, type OrgLibrarySummary, type OrgMemory, type OrgDocument,
  type OrgSkill, type MemoryPromotion, type Submission,
} from "@/lib/org-client";
import { useOrgSessionStore } from "@/stores/org-session-store";

export interface LibraryItems {
  memories: OrgMemory; documents: OrgDocument; skills: OrgSkill;
  "memory-submissions": MemoryPromotion; "document-submissions": Submission; "skill-submissions": Submission;
}
export const libraryResources: OrgLibraryResource[] = ["memories", "documents", "skills", "memory-submissions", "document-submissions", "skill-submissions"];
const defaultQuery = (): OrgLibraryQuery => ({ scopeId: "", query: "", filter: "", page: 1, size: 20 });
type Pages = { [K in OrgLibraryResource]: OrgLibraryPage<LibraryItems[K]> & { loading: boolean; error: string | null } };
type Queries = Record<OrgLibraryResource, OrgLibraryQuery>;
const freshQueries = () => Object.fromEntries(libraryResources.map(key => [key, defaultQuery()])) as Queries;
const freshPages = () => Object.fromEntries(libraryResources.map(key => [key, { items: [], total: 0, page: 1, size: 20, loading: false, error: null }])) as unknown as Pages;

/** Each resource and submission view owns its query, page and request generation.
 * Background refresh never resets pagination or overwrites a newer search. */
export function useOrganizationLibrary() {
  const [queries, setQueries] = useState(freshQueries);
  const queryRef = useRef(queries);
  const [pages, setPages] = useState(freshPages);
  const [summary, setSummary] = useState<OrgLibrarySummary | null>(null);
  const appliedQueries = useRef<Partial<Queries>>({});
  const generations = useRef<Record<string, number>>({});
  const timers = useRef<Record<string, ReturnType<typeof setTimeout>>>({});
  const lifecycle = useRef(0);

  const clear = useCallback(() => {
    lifecycle.current++;
    for (const timer of Object.values(timers.current)) clearTimeout(timer);
    timers.current = {};
    appliedQueries.current = {};
    queryRef.current = freshQueries();
    setQueries(queryRef.current);
    setPages(freshPages());
    setSummary(null);
  }, []);
  useEffect(() => () => {
    lifecycle.current++;
    for (const timer of Object.values(timers.current)) clearTimeout(timer);
  }, []);

  const request = useCallback(async (resource: OrgLibraryResource, query: OrgLibraryQuery, foreground = true) => {
    const revision = useOrgSessionStore.getState().boundaryRevision;
    const life = lifecycle.current;
    const generation = (generations.current[resource] ?? 0) + 1;
    generations.current[resource] = generation;
    const current = () => revision === useOrgSessionStore.getState().boundaryRevision
      && lifecycle.current === life && generations.current[resource] === generation;
    if (foreground) setPages(previous => ({ ...previous, [resource]: { ...previous[resource], loading: true, error: null } }));
    try {
      let page = await orgLibraryQuery(resource, query);
      if (!current()) return;
      const lastPage = Math.max(1, Math.ceil(page.total / query.size));
      if (query.page > lastPage) {
        query = { ...query, page: lastPage };
        queryRef.current = { ...queryRef.current, [resource]: query };
        setQueries(queryRef.current);
        page = await orgLibraryQuery(resource, query);
        if (!current()) return;
      }
      appliedQueries.current[resource] = query;
      setPages(previous => ({ ...previous, [resource]: { ...page, loading: false, error: null } }) as Pages);
    } catch (reason) {
      if (!current()) return;
      const applied = appliedQueries.current[resource];
      const sameCriteria = applied && applied.scopeId === query.scopeId && applied.query === query.query
        && applied.filter === query.filter && applied.size === query.size;
      setPages(previous => ({ ...previous, [resource]: {
        ...(sameCriteria ? previous[resource] : { items: [], total: 0, page: query.page, size: query.size }),
        loading: false, error: String(reason),
      } }) as Pages);
      throw reason;
    }
  }, []);

  const update = useCallback((resource: OrgLibraryResource, patch: Partial<OrgLibraryQuery>) => {
    const query = { ...queryRef.current[resource], ...patch, page: patch.page ?? 1 };
    queryRef.current = { ...queryRef.current, [resource]: query };
    setQueries(queryRef.current);
    clearTimeout(timers.current[resource]);
    generations.current[resource] = (generations.current[resource] ?? 0) + 1;
    setPages(previous => ({ ...previous, [resource]: { ...previous[resource], loading: true, error: null } }));
    const run = () => { delete timers.current[resource]; void request(resource, query).catch(() => {}); };
    if (patch.query !== undefined) timers.current[resource] = setTimeout(run, 300);
    else run();
  }, [request]);

  const refresh = useCallback(async () => {
    const revision = useOrgSessionStore.getState().boundaryRevision;
    const life = lifecycle.current;
    const generation = (generations.current.summary ?? 0) + 1;
    generations.current.summary = generation;
    for (const timer of Object.values(timers.current)) clearTimeout(timer);
    timers.current = {};
    const [nextSummary] = await Promise.all([
      orgLibrarySummary(),
      ...libraryResources.map(resource => request(resource, queryRef.current[resource], false)),
    ]);
    if (revision === useOrgSessionStore.getState().boundaryRevision && lifecycle.current === life && generations.current.summary === generation) setSummary(nextSummary);
  }, [request]);
  const retry = useCallback((resource: OrgLibraryResource) => {
    void request(resource, queryRef.current[resource]).catch(() => {});
  }, [request]);

  return { pages, queries, summary, update, refresh, clear, retry };
}
