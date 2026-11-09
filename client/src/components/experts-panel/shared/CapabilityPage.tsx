import { useRef, type ReactNode } from "react";
import { SearchIcon, XCloseIcon } from "@/foundation/components/Icon/icons";
import { PageToolbar } from "@/components/PageToolbar";
import { Button, IconButton } from "@/components/Button";
import { PageHeading } from "@/components/PageHeading";

interface SearchProps {
  value: string;
  onChange: (value: string) => void;
  label: string;
  placeholder?: string;
}

interface Props {
  title: string;
  headingLevel?: 1 | 2;
  description?: string;
  metadata?: ReactNode;
  className?: string;
  search?: SearchProps;
  actions?: ReactNode;
  illustration?: ReactNode;
  summary?: ReactNode;
  children: ReactNode;
}

/** The extension pages share one toolbar, content gutter and scroll owner. */
export function CapabilityPage({ title, headingLevel = 2, description, metadata, className = "", search, actions, illustration, summary, children }: Props) {
  return (
    <section className={`capability-page ${className}`} aria-label={title}>
      <div className="capability-page__inner">
        <PageToolbar className={`capability-page__header${illustration ? " capability-page__header--illustrated" : ""}`}>
          <PageHeading title={title} description={description} level={headingLevel}
            className="capability-page__heading" titleClassName="capability-page__title"
            descriptionClassName="capability-page__description">
            {metadata && <div className="capability-page__metadata">{metadata}</div>}
          </PageHeading>
          <div className="capability-page__controls">
            {search && <CapabilitySearch {...search} />}
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

export function CapabilitySearch({ value, onChange, label, placeholder = label }: SearchProps) {
  const input = useRef<HTMLInputElement>(null);
  return <div className="capability-search">
    <SearchIcon size="sm" className="capability-search__icon" />
    <input ref={input} type="text" aria-label={label} placeholder={placeholder}
      value={value} onChange={event => onChange(event.target.value)} />
    {value && <IconButton className="capability-search__clear"
      label={`清空${label}`} onClick={() => { onChange(""); input.current?.focus(); }}>
      <XCloseIcon size="sm" />
    </IconButton>}
  </div>;
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
