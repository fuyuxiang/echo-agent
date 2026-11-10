import { useLayoutEffect, useRef, useState, type ReactNode } from "react";

export type CatalogSection = "tasks" | "projects" | "groups";

const listIds: Record<CatalogSection, string> = {
  tasks: "sidebar-task-list",
  projects: "sidebar-project-list",
  groups: "sidebar-group-list",
};
const sectionNames: Record<CatalogSection, string> = {
  tasks: "任务",
  projects: "项目",
  groups: "专家团",
};

type Preview = { height: number; remaining: number; visible: number };

const pixels = (value: string) => parseFloat(value) || 0;
const margins = (element: Element) => {
  const style = getComputedStyle(element);
  return pixels(style.marginBlockStart) + pixels(style.marginBlockEnd);
};
const outerHeight = (element: Element) => element.getBoundingClientRect().height + margins(element);

export function useSidebarCatalog() {
  const ref = useRef<HTMLDivElement>(null);
  const [active, setActive] = useState<CatalogSection | null>(null);
  const [previews, setPreviews] = useState<Partial<Record<CatalogSection, Preview>>>({});
  const [compactDisclosures, setCompactDisclosures] = useState(false);
  const disclosureFocus = useRef<string>();
  const reveal = useRef<{ section: CatalogSection; index: number } | null>(null);

  useLayoutEffect(() => {
    const catalog = ref.current;
    if (!catalog || typeof ResizeObserver === "undefined") return;

    let timer: number | undefined;
    let observed = new Set<Element>();
    const schedule = () => {
      if (timer === undefined) timer = window.setTimeout(measure, 0);
    };
    const resize = new ResizeObserver(schedule);
    const mutations = new MutationObserver(schedule);

    const measure = () => {
      timer = undefined;
      if (!catalog.clientHeight || !catalog.clientWidth) return;
      const targets = new Set<Element>([catalog]);
      const style = getComputedStyle(catalog);
      const moreHeight = pixels(style.getPropertyValue("--sidebar-catalog-more-height"));
      let space = catalog.clientHeight - pixels(style.paddingBlockStart) - pixels(style.paddingBlockEnd);
      const lists = Array.from(catalog.children).flatMap(section => {
        const list = section.querySelector<HTMLElement>(":scope > .sidebar__group[data-catalog-list]");
        const rows = list?.querySelector<HTMLElement>(":scope > .sidebar__catalog-rows");
        if (!list || !rows) {
          targets.add(section);
          space -= outerHeight(section);
          return [];
        }

        space -= margins(section) + margins(list);
        const heading = section.querySelector(":scope > .sidebar__section-head");
        if (heading) {
          targets.add(heading);
          space -= outerHeight(heading);
        }
        targets.add(rows);
        const id = list.dataset.catalogList as CatalogSection;
        const sizes = [0];
        let projectContextHeight = 0;
        for (const row of rows.children) {
          const item = id === "projects" ? row.firstElementChild ?? row : row;
          const height = outerHeight(item);
          sizes.push(sizes[sizes.length - 1] + height);
          const conversations = id === "projects" ? row.querySelector(":scope > .sidebar__children") : null;
          if (conversations?.firstElementChild) {
            projectContextHeight = Math.max(projectContextHeight,
              height + margins(conversations) + outerHeight(conversations.firstElementChild));
          }
        }
        return [{ id, rows, sizes, projectContextHeight, hasHeading: Boolean(heading),
          count: rows.children.length, visible: Math.min(1, rows.children.length) }];
      });
      for (const element of observed) if (!targets.has(element)) resize.unobserve(element);
      for (const element of targets) if (!observed.has(element)) resize.observe(element);
      observed = targets;

      const rowHeight = (list: typeof lists[number], visible: number) =>
        Math.ceil(Math.max(list.sizes[visible], list.projectContextHeight));
      const minimumWithFooters = lists.reduce((total, list) =>
        total + rowHeight(list, list.visible) + (list.visible < list.count ? moreHeight : 0), 0);
      const compact = minimumWithFooters > space;
      if (compact !== compactDisclosures) setCompactDisclosures(compact);
      const cost = (list: typeof lists[number], visible: number) =>
        rowHeight(list, visible) + (visible < list.count && !(compact && list.hasHeading) ? moreHeight : 0);

      // Reserve a whole row per section and room for an open project's heading
      // and first conversation before giving any section additional preview rows.
      for (const list of lists) space -= cost(list, list.visible);
      let added = true;
      while (added && space >= 0) {
        added = false;
        for (const list of lists) {
          if (list.visible === list.count) continue;
          const extra = cost(list, list.visible + 1) - cost(list, list.visible);
          if (extra > space) continue;
          space -= extra;
          list.visible++;
          added = true;
        }
      }

      const next: Partial<Record<CatalogSection, Preview>> = {};
      for (const list of lists) {
        const height = rowHeight(list, list.visible);
        const extra = active === "projects" && list.projectContextHeight
          ? Math.max(0, Math.min(space, list.rows.getBoundingClientRect().height - height)) : 0;
        next[list.id] = { height: Math.ceil(height + extra), remaining: list.count - list.visible, visible: list.visible };
      }
      const focused = document.activeElement;
      if (focused?.classList.contains("sidebar__catalog-more") && catalog.contains(focused)) {
        const section = lists.find(list => listIds[list.id] === focused.getAttribute("aria-controls"))?.id;
        if (section && (compact !== compactDisclosures || !next[section]?.remaining)) {
          disclosureFocus.current = listIds[section];
        }
      }
      setPreviews(previous => (Object.keys(listIds) as CatalogSection[]).every(id =>
        previous[id]?.height === next[id]?.height && previous[id]?.remaining === next[id]?.remaining
        && previous[id]?.visible === next[id]?.visible
      ) ? previous : next);
    };

    mutations.observe(catalog, { childList: true, subtree: true, characterData: true });
    measure();
    return () => {
      window.clearTimeout(timer);
      resize.disconnect();
      mutations.disconnect();
    };
  }, [active, compactDisclosures]);

  useLayoutEffect(() => {
    const listId = disclosureFocus.current;
    if (!listId) return;
    disclosureFocus.current = undefined;
    const list = ref.current?.querySelector<HTMLElement>(`#${listId}`);
    if (list && document.activeElement === document.body) focusCatalogDisclosure(list);
  }, [compactDisclosures, previews]);

  return { ref, active, setActive, previews, reveal, compactDisclosures };
}

export type SidebarCatalogState = ReturnType<typeof useSidebarCatalog>;

function focusCatalogDisclosure(list: HTMLElement) {
  const section = list.parentElement;
  (section?.querySelector<HTMLElement>(".sidebar__catalog-more") ??
    section?.querySelector<HTMLElement>(".sidebar__section-label"))?.focus();
}

export function SidebarCatalogMore({ section, catalog, inHeading = false }: {
  section: CatalogSection;
  catalog?: SidebarCatalogState;
  inHeading?: boolean;
}) {
  const preview = catalog?.previews[section];
  if (!catalog || !preview?.remaining || inHeading !== catalog.compactDisclosures) return null;
  const expanded = catalog.active === section;
  const label = expanded ? `收起更多${sectionNames[section]}`
    : `查看更多${sectionNames[section]}（${preview.remaining}）`;
  return <button type="button" className={`sidebar__catalog-more${inHeading ? " sidebar__catalog-more--compact" : ""}`}
    aria-controls={listIds[section]} aria-expanded={expanded} aria-label={label} title={label}
    onClick={() => {
      catalog.reveal.current = expanded ? null : { section, index: preview.visible };
      catalog.setActive(expanded ? null : section);
    }}>
    {expanded ? "收起" : inHeading ? `更多 ${preview.remaining}` : `查看更多（${preview.remaining}）`}
  </button>;
}

export function SidebarCatalogList({ section, catalog, label, className = "", children }: {
  section: CatalogSection;
  catalog?: SidebarCatalogState;
  label: string;
  className?: string;
  children: ReactNode;
}) {
  const listRef = useRef<HTMLDivElement>(null);
  const rowsRef = useRef<HTMLDivElement>(null);
  const preview = catalog?.previews[section];
  const expanded = catalog?.active === section;
  const id = listIds[section];

  useLayoutEffect(() => {
    const list = listRef.current;
    const rows = rowsRef.current;
    if (!list || !rows) return;
    const items = Array.from(rows.children) as HTMLElement[];
    if (!expanded && preview && items.slice(preview.visible).some(row => row.contains(document.activeElement))) {
      focusCatalogDisclosure(list);
    }
    if (!expanded) list.scrollTop = 0;
    items.forEach((row, index) => { row.inert = !!preview && !expanded && index >= preview.visible; });

    const reveal = catalog?.reveal.current;
    if (expanded && catalog && reveal?.section === section) {
      catalog.reveal.current = null;
      const firstNew = items[reveal.index];
      const target = firstNew?.querySelector<HTMLElement>("button:not(:disabled)") ?? list;
      target.focus();
      if (firstNew) {
        list.scrollTop += firstNew.getBoundingClientRect().top - list.getBoundingClientRect().top;
      }
    }
  }, [catalog?.reveal, children, expanded, preview, section]);

  return <>
    <div id={id} ref={listRef} className={`sidebar__group ${className}`} data-catalog-list={section}
      data-catalog-preview={preview && !expanded ? "" : undefined} style={preview ? { height: preview.height } : undefined}
      role="region" aria-label={label} tabIndex={expanded ? 0 : undefined}
      onKeyDown={event => {
        if (event.key !== "Escape" || event.defaultPrevented || !expanded || !catalog) return;
        event.preventDefault();
        event.stopPropagation();
        focusCatalogDisclosure(event.currentTarget);
        catalog.setActive(null);
      }}>
      <div ref={rowsRef} className="sidebar__catalog-rows">{children}</div>
    </div>
    <SidebarCatalogMore section={section} catalog={catalog} />
  </>;
}
