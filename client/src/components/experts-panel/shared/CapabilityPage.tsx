import { useRef, type ReactNode } from "react";
import { SearchIcon, XCloseIcon } from "@/foundation/components/Icon/icons";
import { PageToolbar } from "@/components/PageToolbar";
import { Button, IconButton } from "@/components/Button";

interface Props {
  title: string;
  headingLevel?: 1 | 2;
  description?: string;
  metadata?: ReactNode;
  className?: string;
  search?: {
    value: string;
    onChange: (value: string) => void;
    label: string;
    placeholder?: string;
  };
  actions?: ReactNode;
  illustration?: ReactNode;
  summary?: ReactNode;
  children: ReactNode;
}

/** The extension pages share one toolbar, content gutter and scroll owner. */
export function CapabilityPage({ title, headingLevel = 2, description, metadata, className = "", search, actions, illustration, summary, children }: Props) {
  const searchRef = useRef<HTMLInputElement>(null);
  const Heading = headingLevel === 1 ? "h1" : "h2";
  return (
    <section className={`capability-page ${className}`} aria-label={title}>
      <div className="capability-page__inner">
        <PageToolbar className={`capability-page__header${illustration ? " capability-page__header--illustrated" : ""}`}>
          <div className="capability-page__heading">
            <Heading className="capability-page__title">{title}</Heading>
            {description && <p className="capability-page__description">{description}</p>}
            {(metadata || illustration) && <div className="capability-page__metadata" aria-hidden={metadata ? undefined : true}>{metadata}</div>}
          </div>
          <div className="capability-page__controls">
            {search && (
              <div className="capability-search">
                <SearchIcon size="sm" className="capability-search__icon" />
                <input ref={searchRef} type="text" aria-label={search.label}
                  placeholder={search.placeholder ?? search.label}
                  value={search.value} onChange={(event) => search.onChange(event.target.value)} />
                {search.value && <IconButton className="capability-search__clear"
                  label={`清空${search.label}`} onClick={() => { search.onChange(""); searchRef.current?.focus(); }}>
                  <XCloseIcon size="sm" />
                </IconButton>}
              </div>
            )}
            {actions && <div className="capability-page__actions">{actions}</div>}
          </div>
          {illustration && <div className="capability-page__illustration">{illustration}</div>}
        </PageToolbar>
        {summary && <div className="capability-page__summary">{summary}</div>}
        <div className="capability-page__content">{children}</div>
      </div>
    </section>
  );
}

export function CapabilityEmptyState({ icon, title, description, onClear }: {
  icon?: ReactNode;
  title: string;
  description?: ReactNode;
  onClear?: () => void;
}) {
  return <div className="capability-empty" role="status">
    {icon && <span className="capability-empty__icon" aria-hidden="true">{icon}</span>}
    <div className="capability-empty__copy">
      <p>{title}</p>
      {description && <div className="capability-empty__description">{description}</div>}
    </div>
    {onClear && <Button onClick={onClear}>清空搜索</Button>}
  </div>;
}

export function CapabilityError({ children, onRetry, retrying = false }: {
  children: ReactNode;
  onRetry: () => void;
  retrying?: boolean;
}) {
  return <div className="capability-error" role="alert">
    <span>{children}</span>
    <Button onClick={onRetry} disabled={retrying} aria-busy={retrying}>
      {retrying ? "重试中…" : "重试"}
    </Button>
  </div>;
}
