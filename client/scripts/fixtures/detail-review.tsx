import { useState } from "react";
import { ExpertDetailModal } from "../../src/components/experts-panel/experts/ExpertDetailModal";
import { ConnectorDetailModal } from "../../src/components/experts-panel/connectors/ConnectorDetailModal";
import { ConnectorTokenForm } from "../../src/components/experts-panel/connectors/ConnectorTokenForm";
import { UploadSkillModal } from "../../src/components/experts-panel/skills/UploadSkillModal";
import { KnowledgePicker } from "../../src/components/KnowledgePicker";

const description = "帮助梳理工作目标、检查执行过程并整理可继续使用的交付结果。支持长内容阅读、明确的操作反馈和相关配置查看。".repeat(15);
const expert = { id: "review", cat: "办公与研发", type: "agent", name: "产品评审专家", title: "产品设计与交互评审专家", desc: description, tags: ["界面评审", "交互设计", "前端工程"], quickPrompts: ["检查所有弹窗在长内容和小窗口下的布局与交互状态。", "整理重要问题并给出完整的验证结果。"] };
const connector = { id: "review", name: "数据分析连接器", cat: "效率工具", kind: "mcp", source: "review", desc: description, examplesZh: ["把最新项目的交付结果整理成一份报告。"], tokenSchema: { title: "连接数据分析服务", description: "填写服务连接配置。凭据只用于当前连接。", fields: Array.from({ length: 8 }, (_, index) => ({ key: `field-${index}`, label: `配置字段 ${index + 1}`, required: true, description: "使用隔离的界面测试内容，无需真实凭据。" })) } };
export default function DetailReview() {
  const variant = new URLSearchParams(location.search).get("detailVariant") ?? "expert";
  const [open, setOpen] = useState(true);
  if (!open) return <button onClick={() => setOpen(true)}>重新打开</button>;
  if (variant === "knowledge") return <div style={{ padding: 30, height: "100%", overflow: "hidden" }}><KnowledgePicker onManage={() => {}} onOpenOrganization={() => {}} /></div>;
  if (variant === "connector") return <ConnectorDetailModal connector={connector} root="/review" onClose={() => setOpen(false)} onConfigure={() => setOpen(false)} />;
  if (variant === "token") return <ConnectorTokenForm connector={connector} onClose={() => setOpen(false)} onSubmit={() => { throw new Error("隔离测试：连接服务暂不可用"); }} />;
  if (variant === "upload") return <UploadSkillModal skill={{ name: "界面与交互质量评审技能", path: "/review/skills/ui-review/SKILL.md", source: "user", description: description }} onClose={() => setOpen(false)} />;
  return <ExpertDetailModal expert={expert} onClose={() => setOpen(false)} onSummon={() => setOpen(false)} />;
}
