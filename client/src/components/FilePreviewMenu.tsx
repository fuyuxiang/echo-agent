import { MoreHorizontal, type LucideIcon } from "lucide-react";
import { ActionMenu } from "./ActionMenu";

export interface FilePreviewAction {
  label: string;
  icon: LucideIcon;
  disabled?: boolean;
  onSelect: () => void;
}

export function FilePreviewMenu({ fileKey, actions, layer }: {
  fileKey: string;
  actions: readonly FilePreviewAction[];
  layer?: string;
}) {
  if (actions.length === 0) return null;
  return <ActionMenu label="更多文件操作" menuLabel="文件操作" resetKey={fileKey}
    menuZIndex={layer} triggerClassName="file-preview__more" trigger={<MoreHorizontal size={18} aria-hidden="true" />}>
    {({ close }) => actions.map(({ label, icon: Icon, disabled, onSelect }) =>
      <button key={label} type="button" role="menuitem" className="topbar-actions__item" disabled={disabled}
        onClick={() => { close(true); onSelect(); }}>
        <Icon size={15} aria-hidden="true" />{label}
      </button>)}
  </ActionMenu>;
}
