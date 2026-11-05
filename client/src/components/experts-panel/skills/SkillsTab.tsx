import { ResourceIcon, ResourceList, ResourceRow } from "../../ResourceList";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Upload } from "lucide-react";
import {
  AddCircleIcon, RefreshCwIcon, PuzzlePieceIcon, DeleteIcon,
} from "@/foundation/components/Icon/icons";
import {
  skillsList, skillsRemove, skillsToggle, skillsUninstallPackage, internalReload,
} from "@/lib/agent-client";
import type { SkillInfo } from "@/lib/types";
import { ImportSkillModal } from "./ImportSkillModal";
import { UploadSkillModal } from "./UploadSkillModal";
import { listenOrgSkillsChanged, orgSetSkillPreference } from "@/lib/org-client";
import { useAppDialog } from "../../AppDialog";
import { SkillCapabilityStatus } from "./SkillCapabilityStatus";
import { useOrgSessionStore } from "@/stores/org-session-store";
import { CapabilityEmptyState, CapabilityError, CapabilityPage } from "../shared/CapabilityPage";

interface Props {
  cwd?: string;
  pills: React.ReactNode;
  onToast?: (m: string) => void;
}

function organizationScopeLabel(scope: SkillInfo["orgScopeKind"]): string {
  if (scope === "personal") return "个人云 Skill";
  if (scope === "team") return "团队共享";
  if (scope === "org") return "全组织共享";
  return "服务器同步";
}

function skillSourceLabel(skill: SkillInfo): string {
  if (skill.orgManaged || skill.scope === "server") {
    return organizationScopeLabel(skill.orgScopeKind);
  }
  if (skill.managed) return "本地用户";
  const labels: Record<string, string> = {
    local: "当前项目",
    repo: "代码仓库",
    user: "本地用户",
    bundled: "内置技能",
    plugin: "插件技能",
  };
  return skill.scope ? labels[skill.scope] ?? skill.scope : "本地来源";
}

function RuntimeSkillRow({
  skill,
  onToggle,
  onRemove,
  onUpload,
}: {
  skill: SkillInfo;
  onToggle: (skill: SkillInfo, enabled: boolean) => void;
  onRemove?: (skill: SkillInfo) => void;
  onUpload?: (skill: SkillInfo) => void;
}) {
  const name = skill.displayName || skill.name;
  const organizationManaged = skill.orgManaged || skill.scope === "server";
  const mandatory = organizationManaged && skill.orgMandatory === true;
  const removablePath = skill.managed ? skill.path : skill.configuredPath;
  const toggleAction = organizationManaged
    ? skill.enabled ? "卸载" : "安装"
    : skill.enabled ? "停用" : "启用";
  return (
    <ResourceRow className={`sk-inst-row${organizationManaged ? " sk-inst-row--organization" : ""}`}>
      <div className="sk-card-head resource-row__main">
        <ResourceIcon><PuzzlePieceIcon size="md" /></ResourceIcon>
        <div className="sk-inst-info">
          <div className="sk-card-name resource-row__title">{name}</div>
          <p className="sk-card-desc resource-row__description resource-clamp">{skill.description || "（无描述）"}</p>
          <div className="sk-inst-meta resource-row__meta">
            <span>{skillSourceLabel(skill)}</span>
            {organizationManaged && <span>组织托管</span>}
            {mandatory && <span className="sk-inst-meta--mandatory">组织强制</span>}
            {skill.version && <span>v{skill.version}</span>}
            {skill.compatibility && <span title={skill.compatibility}>{skill.compatibility}</span>}
            {skill.capability && <SkillCapabilityStatus report={skill.capability} compact />}
          </div>
        </div>
      </div>
      <div className="resource-row__actions">
      <label className={`sk-toggle${mandatory ? " sk-toggle--locked" : ""}`}
        title={mandatory ? "组织强制 Skill 不能卸载" : organizationManaged
          ? skill.enabled ? "已安装" : "未安装"
          : skill.enabled ? "已启用" : "已禁用"}>
        <input
          type="checkbox"
          aria-label={`${toggleAction}${name}`}
          checked={skill.enabled}
          disabled={mandatory}
          onChange={() => onToggle(skill, !skill.enabled)}
        />
        <span className="sk-toggle-track"><span className="sk-toggle-thumb" /></span>
      </label>
      {onUpload && skill.path && !organizationManaged && (
        <button type="button" className="sk-inst-upload" title="上传到组织"
          aria-label={`上传${name}到组织`}
          onClick={() => onUpload(skill)}><Upload size={16} /></button>
      )}
      {onRemove && removablePath && !organizationManaged && (
        <button type="button" className="sk-inst-del" title="移除"
          aria-label={`移除技能 ${name}`}
          onClick={() => onRemove(skill)}><DeleteIcon size="sm" /></button>
      )}
      </div>
    </ResourceRow>
  );
}

/**
 * 技能页直接展示 EchoAgent Runtime 的技能扫描结果。
 *
 * Runtime 会自动扫描用户全局 `~/.echo-agent/skills`，并合并当前工作区、
 * 内置和插件技能。组织 Skill 也安装在该全局目录下，因此这里不再维护
 * 另一套“来源目录”或市场目录选择状态。
 */
export function SkillsTab({ pills, onToast, cwd }: Props) {
  const [search, setSearch] = useState("");
  const [skills, setSkills] = useState<SkillInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [importOpen, setImportOpen] = useState(false);
  const [uploadSkill, setUploadSkill] = useState<SkillInfo | null>(null);
  const reloadGeneration = useRef(0);
  const currentCwd = useRef(cwd);
  currentCwd.current = cwd;
  const { requestConfirmation, dialog } = useAppDialog();

  const reloadSkills = useCallback(async (silent = false) => {
    if (currentCwd.current !== cwd) return;
    const generation = ++reloadGeneration.current;
    setLoading(true);
    setError("");
    try {
      const next = await skillsList(cwd);
      if (reloadGeneration.current === generation && currentCwd.current === cwd) setSkills(next);
    } catch (e) {
      if (reloadGeneration.current !== generation) return;
      const message = String(e).replace(/^Error:\s*/, "");
      setError(message);
      if (!silent) onToast?.(`加载技能失败：${message}`);
    } finally {
      if (reloadGeneration.current === generation) setLoading(false);
    }
  }, [onToast, cwd]);

  useEffect(() => {
    void reloadSkills(true);
    return () => { reloadGeneration.current += 1; };
  }, [reloadSkills]);
  useEffect(() => useOrgSessionStore.subscribe((next, previous) => {
    if (next.boundaryRevision === previous.boundaryRevision) return;
    setSkills((current) => current.filter((skill) => !skill.orgManaged && skill.scope !== "server"));
    setUploadSkill(null);
    void reloadSkills(true);
  }), [reloadSkills]);
  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void listenOrgSkillsChanged(() => { void reloadSkills(true); }).then((stop) => {
      if (disposed) stop();
      else unlisten = stop;
    }).catch(() => {});
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [reloadSkills]);

  const visible = useMemo(() => {
    const query = search.trim().toLowerCase();
    if (!query) return skills;
    return skills.filter((skill) => (
      (skill.displayName || skill.name).toLowerCase().includes(query)
      || skill.name.toLowerCase().includes(query)
      || (skill.description ?? "").toLowerCase().includes(query)
      || skillSourceLabel(skill).toLowerCase().includes(query)
      || skill.capability?.capabilities.some((capability) => capability.includes(query))
      || skill.capability?.manifest?.requirements.connectors.some((connector) => (
        connector.id.includes(query) || (connector.label ?? "").toLowerCase().includes(query)
      ))
    ));
  }, [search, skills]);

  const refreshAndReload = useCallback(async () => {
    if (currentCwd.current !== cwd) return;
    setLoading(true);
    try {
      await internalReload("skills");
      await reloadSkills();
      if (currentCwd.current === cwd) onToast?.("技能已更新");
    } catch (error) {
      if (currentCwd.current === cwd) {
        setError(String(error).replace(/^Error:\s*/, ""));
        setLoading(false);
      }
    }
  }, [cwd, onToast, reloadSkills]);

  const handleToggle = useCallback(async (skill: SkillInfo, enabled: boolean) => {
    if (skill.orgMandatory) return;
    try {
      if ((skill.orgManaged || skill.scope === "server") && skill.orgSkillId) {
        await orgSetSkillPreference(skill.orgSkillId, enabled);
      } else {
        await skillsToggle(skill.name, enabled, cwd);
      }
      await reloadSkills();
    } catch (e) {
      const message = String(e).replace(/^Error:\s*/, "");
      if (currentCwd.current === cwd) setError(message);
      onToast?.(`切换失败：${message}`);
    }
  }, [onToast, reloadSkills, cwd]);

  const handleRemove = useCallback((skill: SkillInfo) => {
    const removablePath = skill.managed ? skill.path : skill.configuredPath;
    if (!removablePath) {
      onToast?.("该技能由项目、内置包或插件管理，请在对应来源中修改");
      return;
    }
    const action = skill.managed ? "卸载" : "从配置中移除";
    const shared = !skill.managed
      ? skills.filter((candidate) => candidate.configuredPath === removablePath).length
      : 1;
    requestConfirmation({
      title: `${action}技能“${skill.displayName || skill.name}”？`,
      description: shared > 1
        ? `该路径共提供 ${shared} 个技能，移除后它们都将不再加载。`
        : skill.managed
          ? "技能包将从本机卸载，之后可以重新安装。"
          : "该路径将从技能配置中移除，原文件不会被删除。",
      confirmLabel: action,
      danger: true,
      action: async () => {
        if (skill.managed) await skillsUninstallPackage(removablePath);
        else await skillsRemove(removablePath, cwd);
        await reloadSkills();
      },
    });
  }, [skills, onToast, reloadSkills, requestConfirmation, cwd]);

  return (
    <div className="capability-tab">
      {pills && <header className="um-topbar"><div className="um-topbar-left">{pills}</div></header>}
      <CapabilityPage title="已安装技能" className="capability-page--skills"
        search={{ value: search, onChange: setSearch, label: "搜索已安装技能", placeholder: "搜索技能…" }}
        actions={<>
          <button type="button" className="capability-button" onClick={() => void refreshAndReload()}
            disabled={loading} title="重新扫描全局和当前工作区技能">
            <RefreshCwIcon size="sm" /><span>刷新</span>
          </button>
          <button type="button" className="capability-button capability-button--primary" onClick={() => setImportOpen(true)}>
            <AddCircleIcon size="sm" /><span>添加技能</span>
          </button>
        </>}
        summary={loading && skills.length === 0 ? "正在读取技能…" : error && skills.length === 0 ? "技能列表未加载" : `${skills.length} 个技能 · ${skills.filter(skill => skill.enabled).length} 个已启用`}>

        {error && (
          <CapabilityError onRetry={() => void refreshAndReload()} retrying={loading}>
            技能列表加载失败：{error}{skills.length > 0 ? "。下方保留上次加载的结果。" : ""}
          </CapabilityError>
        )}

        {loading && skills.length === 0 ? (
          <div className="capability-loading" role="status">正在读取已安装技能…</div>
        ) : error && skills.length === 0 ? null : skills.length === 0 ? (
          <CapabilityEmptyState icon={<PuzzlePieceIcon size="lg" />} title="还没有已安装的技能"
            description="点击「添加技能」安装本地技能，或在组织页面安装共享技能" />
        ) : visible.length === 0 ? (
          <CapabilityEmptyState icon={<PuzzlePieceIcon size="lg" />} title={`没有找到与「${search}」匹配的技能`} onClear={() => setSearch("")} />
        ) : (
          <ResourceList className="sk-inst-list" aria-label="已安装技能">
            {visible.map((skill) => (
              <RuntimeSkillRow key={skill.name + (skill.path ?? "")} skill={skill}
                onToggle={handleToggle} onRemove={handleRemove} onUpload={setUploadSkill} />
            ))}
          </ResourceList>
        )}
      </CapabilityPage>

      {importOpen && (
        <ImportSkillModal onClose={() => setImportOpen(false)} onToast={onToast}
          onInstalled={reloadSkills} />
      )}
      {uploadSkill && (
        <UploadSkillModal skill={uploadSkill} onClose={() => setUploadSkill(null)} onToast={onToast} />
      )}
      {dialog}
    </div>
  );
}
