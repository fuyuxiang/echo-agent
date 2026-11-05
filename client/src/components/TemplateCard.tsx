import { useId, type ReactNode } from "react";

/** Shared template choice: consistent presentation, feature-owned creation. */
export function TemplateCard({ icon, title, description, category, metadata, onSelect }: {
  icon: ReactNode;
  title: string;
  description: string;
  category?: string;
  metadata?: ReactNode;
  onSelect: () => void;
}) {
  const id = useId();
  return (
    <button type="button" className="template-card resource-template" onClick={onSelect}
      aria-labelledby={`${id}-title`} aria-describedby={`${category ? `${id}-category ` : ""}${id}-description${metadata ? ` ${id}-metadata` : ""}`}>
      <span className="resource-icon" aria-hidden="true">{icon}</span>
      <span className="resource-template__body">
        <span className="resource-template__heading">
          <span id={`${id}-title`} className="resource-template__title" title={title}>{title}</span>
          {category && <span id={`${id}-category`} className="resource-tag">{category}</span>}
        </span>
        <span id={`${id}-description`} className="resource-template__description" title={description}>{description}</span>
        {metadata && <span id={`${id}-metadata`} className="resource-template__metadata">{metadata}</span>}
      </span>
    </button>
  );
}
