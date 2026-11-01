// Production renderers and status classes, with deterministic review-only data.
import { Markdown } from "../../src/components/markdown";
import { ToolCallDetailBody } from "../../src/components/ToolCallCard";
import { useTheme } from "../../src/components/ThemeProvider";
import { ProjectSwitcher } from "../../src/features/coding/shell/ProjectSwitcher";
import { Mic, MonitorPlay } from "lucide-react";

const code = '```typescript\n// 阅读代码与执行结果\nexport async function inspect(value: number = 42) {\n  console.log("ready", value);\n  return true;\n}\n```\n\n```diff\n- const ready = false;\n+ const ready = true;\n```';
export function ColorSystemReview() {
  const { theme } = useTheme();
  return <main className="color-system-review" style={{height:"100%",overflow:"auto",padding:24,background:"var(--echo-bg-primary)",color:"var(--echo-text-strong)"}}>
    <h1 style={{fontSize:"1.25rem",margin:"0 0 16px"}}>代码与语义状态</h1>
    <div style={{display:"flex",alignItems:"center",gap:16,marginBottom:16}}>
      <ProjectSwitcher projects={[]} activeCwd="/review/EchoAgent" dirtyCount={3} onSelect={() => {}} onRemove={() => {}} onOpenFolder={() => {}} />
      <span className="meeting-recorder__pulse" role="img" aria-label="麦克风录音"><Mic size={20} /></span>
      <span className="meeting-global-recorder__dot" role="img" aria-label="系统声音录音"><MonitorPlay size={14} /></span>
    </div>
    {new URLSearchParams(location.search).has("profileDangerReview") && <button className="btn btn--ghost colleague-profile-btn colleague-profile-btn--danger">删除专家</button>}
    <div style={{display:"flex",flexWrap:"wrap",gap:12,marginBottom:16}}>
      <span className="sk-risk sk-risk--medium">需要确认</span>
      <span className="sk-risk sk-risk--high">高风险</span>
      <span className="mcp-state mcp-state--ready">连接可用</span>
      <span className="mcp-state mcp-state--warn">等待配置</span>
      <span className="mcp-state mcp-state--error">连接失败</span>
      <span className="org-scope-badge org-scope-badge--personal">个人空间</span>
      <span className="org-scope-badge org-scope-badge--team">团队空间</span>
      <span className="model-tag-badges__chip model-tag-badges__chip--multimodal">多模态</span>
      <span className="model-tag-badges__chip model-tag-badges__chip--reasoning">推理</span>
    </div>
    <div className="mcp-load-error">连接失败，保留配置后重试。</div>
    <div className="runtime-panel-empty runtime-panel-empty--error">运行信息加载失败，可以重试。</div>
    <Markdown complete theme={theme}>{code}</Markdown>
    <ToolCallDetailBody tc={{toolCallId:"color-review",title:"更新配置",kind:"edit",status:"completed",content:[{type:"diff",diff:{path:"review.ts",old:"const ready = false;\n",new:"const ready = true;\n"}}]}} />
    <div style={{display:"flex",gap:8,marginTop:12}}>
      <span className="file-tree__git-badge file-tree__git-badge--added">A</span>
      <span className="file-tree__git-badge file-tree__git-badge--modified">M</span>
      <span className="file-tree__git-badge file-tree__git-badge--deleted">D</span>
      <span className="file-tree__git-badge file-tree__git-badge--renamed">R</span>
      <span className="file-tree__git-badge file-tree__git-badge--untracked">?</span>
    </div>
    <div style={{display:"grid",gridTemplateColumns:"repeat(2,minmax(0,1fr))",gap:12,marginTop:16}}>
      <label>交付说明<textarea className="form-control" aria-label="交付说明" defaultValue="初始交付说明" style={{display:"block",width:"100%"}} /></label>
      <label>交付工作区<select className="form-control" aria-label="交付工作区" defaultValue="personal" style={{display:"block",width:"100%"}}><option value="personal">个人工作区</option><option value="team">团队交付工作区</option></select></label>
    </div>
  </main>;
}
