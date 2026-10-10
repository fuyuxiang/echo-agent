import { useEffect, useMemo, useState } from "react";
import { Upload } from "lucide-react";
import { XCloseIcon } from "@/foundation/components/Icon/icons";
import {
  orgListScopes,
  orgSession,
  orgSubmitSkill,
  type OrgScope,
} from "@/lib/org-client";
import type { SkillInfo } from "@/lib/types";
import { useModalFocus } from "@/lib/use-modal-focus";

function scopeLabel(scope: OrgScope): string {
  return scope.kind === "team" ? `团队 · ${scope.name}` : `全组织 · ${scope.name}`;
}

/** Upload one discovered local Skill to an organization sharing scope. */
export function UploadSkillModal({
  skill,
  onClose,
  onToast,
}: {
  skill: SkillInfo;
  onClose: () => void;
  onToast?: (message: string) => void;
}) {
  const [scopes, setScopes] = useState<OrgScope[]>([]);
  const [scopeId, setScopeId] = useState("");
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const name = skill.displayName || skill.name;
  const dialogRef = useModalFocus<HTMLDivElement>(true, () => {
    if (!busy) onClose();
  });

  useEffect(() => {
    let alive = true;
    (async () => {
      try {
        const session = await orgSession();
        if (!session.loggedIn) throw new Error("请先在「组织」中登录组织账号");
        if (session.bootstrap?.policy.allowSkillSubmission === false) {
          throw new Error("当前组织策略不允许上传 Skill");
        }
        const available = (await orgListScopes()).filter((scope) => scope.kind !== "personal");
        if (!alive) return;
        setScopes(available);
        setScopeId(available.find((scope) => scope.kind === "team")?.id ?? available[0]?.id ?? "");
        if (available.length === 0) setError("当前账号没有可用的团队或组织共享范围");
      } catch (reason) {
        if (alive) setError(String(reason).replace(/^Error:\s*/, ""));
      } finally {
        if (alive) setLoading(false);
      }
    })();
    return () => { alive = false; };
  }, []);

  const selectedScope = useMemo(
    () => scopes.find((scope) => scope.id === scopeId),
    [scopeId, scopes],
  );

  const submit = async () => {
    if (!skill.path || !scopeId) return;
    setBusy(true);
    setError("");
    try {
      const result = await orgSubmitSkill(skill.path, scopeId);
      onToast?.(result.state === "pending"
        ? `Skill「${name}」已提交，等待组织审核`
        : `Skill「${name}」已上传到${selectedScope?.name ?? "组织"}`);
      onClose();
    } catch (reason) {
      setError(String(reason).replace(/^Error:\s*/, ""));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="modal-overlay sk-import-overlay" onClick={(event) => {
      if (event.target === event.currentTarget && !busy) onClose();
    }}>
      <div ref={dialogRef} className="sk-import sk-upload" onClick={(event) => event.stopPropagation()} role="dialog" aria-modal="true" aria-label={`上传技能「${name}」`} tabIndex={-1}>
        <div className="sk-import-head">
          <div>
            <h3>上传到组织</h3>
            <p>将本地 Skill「{name}」打包后提交给服务器，审核通过后其他用户可安装。</p>
          </div>
          <button type="button" className="sk-import-close" onClick={onClose} disabled={busy} aria-label="关闭" data-modal-initial-focus>
            <XCloseIcon size="md" />
          </button>
        </div>
        <div className="sk-import-body">
          <div className="sk-upload-source">
            <strong>{name}</strong>
            <span title={skill.path}>{skill.path}</span>
          </div>

          {loading ? (
            <div className="ec-loading">正在读取组织共享范围…</div>
          ) : scopes.length > 0 ? (
            <label className="sk-upload-field">
              <span>共享范围</span>
              <select value={scopeId} onChange={(event) => setScopeId(event.target.value)} disabled={busy}>
                {scopes.map((scope) => <option key={scope.id} value={scope.id}>{scopeLabel(scope)}</option>)}
              </select>
            </label>
          ) : null}

          {error && <div className="sk-install-error" role="alert">{error}</div>}

          <div className="sk-upload-note">
            上传内容会先经过服务器安全扫描。普通成员提交到团队或组织后，需要管理员审核才会对其他用户可见。
          </div>
        </div>
        <div className="sk-upload-footer">
          <button
            type="button"
            className="um-btn um-btn--primary sk-install-submit"
            disabled={loading || busy || !skill.path || !scopeId}
            onClick={() => void submit()}
          >
            <Upload size={15} />{busy ? "正在上传…" : "提交分享"}
          </button>
        </div>
      </div>
    </div>
  );
}
