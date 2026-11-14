import { BookOpen, ChartNoAxesCombined, CloudSun, FilePenLine, FileSearch, Image,
  Mail, MessageSquare, Search, Terminal, Users, Wrench, type LucideIcon } from "lucide-react";
import type { ToolRenderer } from "@/lib/tool-renderers";

const icons: Partial<Record<ToolRenderer, LucideIcon>> = {
  command: Terminal, read: FileSearch, edit: FilePenLine, search: Search, weather: CloudSun,
  task: Users, "team-create": Users, "team-delete": Users, "team-status": Users,
  "image-gen": Image, visualizer: ChartNoAxesCombined, "send-message": MessageSquare,
  "agent-mail": Mail, knowledge: BookOpen, "personal-knowledge": BookOpen,
};

export function ToolActivityIcon({ renderer }: { renderer: ToolRenderer }) {
  const Icon = icons[renderer] ?? Wrench;
  return <Icon size={15} aria-hidden="true" />;
}
