import { NavLink } from "react-router";
import { useTranslation } from "react-i18next";
import {
  LayoutDashboard, MessageSquare, Brain, Zap, BookOpen,
  Radio, Clock, Kanban, ScrollText, Settings, BarChart3
} from "lucide-react";

const NAV = [
  { to: "/", icon: LayoutDashboard, key: "overview" },
  { to: "/sessions", icon: MessageSquare, key: "sessions" },
  { to: "/memory", icon: Brain, key: "memory" },
  { to: "/skills", icon: Zap, key: "skills" },
  { to: "/knowledge", icon: BookOpen, key: "knowledge" },
  { to: "/channels", icon: Radio, key: "channels" },
  { to: "/cron", icon: Clock, key: "cron" },
  { to: "/kanban", icon: Kanban, key: "kanban" },
  { to: "/logs", icon: ScrollText, key: "logs" },
  { to: "/config", icon: Settings, key: "config" },
  { to: "/analytics", icon: BarChart3, key: "analytics" },
] as const;

export function Sidebar() {
  const { t, i18n } = useTranslation("nav");
  return (
    // 窄屏收成图标条(w-14),lg 以上展开为带文字的 w-56。此前固定 w-56 无折叠,
    // 在笔记本分屏/平板上会把主内容区压到不可读。
    <aside className="w-14 lg:w-56 shrink-0 bg-white border-r border-gray-200 flex flex-col">
      <div className="p-4 font-bold text-lg border-b truncate">
        <span className="hidden lg:inline">{t("appName")}</span>
        <span className="lg:hidden" aria-hidden="true">E</span>
      </div>
      <nav className="flex-1 p-2 space-y-1 overflow-y-auto">
        {NAV.map(({ to, icon: Icon, key }) => (
          <NavLink
            key={to}
            to={to}
            end={to === "/"}
            title={t(key)}
            className={({ isActive }) =>
              `flex items-center gap-2 px-3 py-2 rounded-md text-sm ${
                isActive ? "bg-blue-50 text-blue-700" : "text-gray-700 hover:bg-gray-100"
              }`
            }
          >
            <Icon size={18} className="shrink-0" />
            <span className="hidden lg:inline">{t(key)}</span>
          </NavLink>
        ))}
      </nav>
      <div className="p-2 border-t flex flex-col lg:flex-row gap-1">
        {(["zh", "en"] as const).map((lng) => (
          <button
            key={lng}
            onClick={() => i18n.changeLanguage(lng)}
            className={`flex-1 text-xs py-1 rounded ${
              i18n.resolvedLanguage === lng ? "bg-blue-600 text-white" : "bg-gray-100 text-gray-600"
            }`}
          >
            {lng === "zh" ? t("langZh") : t("langEn")}
          </button>
        ))}
      </div>
    </aside>
  );
}
