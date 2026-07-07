import { NavLink } from "react-router-dom";
import {
  LayoutDashboard, MessageSquare, Brain, Zap, BookOpen,
  Radio, Clock, Kanban, ScrollText, Settings, BarChart3
} from "lucide-react";

const NAV = [
  { to: "/", icon: LayoutDashboard, label: "概览" },
  { to: "/sessions", icon: MessageSquare, label: "会话" },
  { to: "/memory", icon: Brain, label: "记忆" },
  { to: "/skills", icon: Zap, label: "技能" },
  { to: "/knowledge", icon: BookOpen, label: "知识库" },
  { to: "/channels", icon: Radio, label: "通道" },
  { to: "/cron", icon: Clock, label: "定时任务" },
  { to: "/kanban", icon: Kanban, label: "看板" },
  { to: "/logs", icon: ScrollText, label: "日志" },
  { to: "/config", icon: Settings, label: "配置" },
  { to: "/analytics", icon: BarChart3, label: "统计" },
];

export function Sidebar() {
  return (
    <aside className="w-56 bg-white border-r border-gray-200 flex flex-col">
      <div className="p-4 font-bold text-lg border-b">Echo Agent</div>
      <nav className="flex-1 p-2 space-y-1">
        {NAV.map(({ to, icon: Icon, label }) => (
          <NavLink
            key={to}
            to={to}
            className={({ isActive }) =>
              `flex items-center gap-2 px-3 py-2 rounded-md text-sm ${
                isActive ? "bg-blue-50 text-blue-700" : "text-gray-700 hover:bg-gray-100"
              }`
            }
          >
            <Icon size={18} />
            {label}
          </NavLink>
        ))}
      </nav>
    </aside>
  );
}
