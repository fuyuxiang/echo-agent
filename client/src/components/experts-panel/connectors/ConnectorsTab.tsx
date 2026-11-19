import { McpModal } from "./McpModal";

interface Props {
  pills: React.ReactNode;
  onToast?: (message: string) => void;
  initialEditing?: boolean;
  initialServerName?: string;
}


/**
 * 连接器页直接展示 EchoAgent Runtime 的用户全局 MCP 配置。
 *
 * 配置统一持久化在 `~/.echo-agent/mcp.json`，支持添加服务、授权与工具管理。
 */
export function ConnectorsTab({ pills, onToast, initialEditing, initialServerName }: Props) {
  return (
    <div className="capability-tab">
      {pills && <header className="um-topbar">
        <div className="um-topbar-left">{pills}</div>
      </header>}
        <McpModal embedded onClose={() => {}} onToast={onToast} initialEditing={initialEditing} initialServerName={initialServerName} />
    </div>
  );
}
