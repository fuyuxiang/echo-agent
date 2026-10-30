import { useEffect, useState, type ReactNode } from "react";
import { ChevronDown } from "lucide-react";

/** Secondary configuration stays discoverable without competing with daily preferences. */
export function SettingsDisclosure({ title, description, children, defaultOpen = false, settingId }: {
  title: string;
  description?: string;
  children: ReactNode;
  defaultOpen?: boolean;
  settingId?: string;
}) {
  const [expanded, setExpanded] = useState(defaultOpen);
  useEffect(() => {
    // Bring failures or an opened file into view, without collapsing the
    // user's section again when the error clears or the file is closed.
    if (defaultOpen) setExpanded(true);
  }, [defaultOpen]);
  return (
    <details className="settings-disclosure" open={expanded} data-setting-id={settingId}>
      <summary onClick={(event) => { event.preventDefault(); setExpanded(value => !value); }}>
        <span className="settings-disclosure__heading">
          <strong>{title}</strong>
          {description && <span>{description}</span>}
        </span>
        <ChevronDown size={16} aria-hidden="true" />
      </summary>
      <div className="settings-disclosure__body">{children}</div>
    </details>
  );
}
