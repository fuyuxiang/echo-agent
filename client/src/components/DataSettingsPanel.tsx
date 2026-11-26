import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { ArchiveRestore, Download, Folder, FolderOpen, ListChecks } from "lucide-react";
import { echoAgentDataDir, openEchoAgentDataDir } from "@/lib/agent-client";
import {
  backupLastError,
  exportBackup,
  exportDeferredUiArchive,
  hasDeferredUiArchive,
  inspectBackup,
  restoreBackup,
} from "@/lib/backup-client";
import { confirmExternalOperationStopped, exportLegacyWorkspaceArchive, hasLegacyWorkspaceArchive, pendingExternalOperations } from "@/lib/legacy-workspace";
import { useSettingsDraft } from "@/lib/settings-drafts";
import { useAppDialog } from "./AppDialog";
import { SettingsGroup, SettingsSection } from "./SettingsSections";

type DataAction = "export" | "restore" | "directory" | "recovery" | "legacy-export" | "legacy-review";
type Feedback = { message: string; error?: boolean };

function errorMessage(error: unknown): string {
  return String(error).replace(/^Error:\s*/, "");
}

function DataActionRow({ title, icon, description, action, feedback }: {
  title: string;
  icon: ReactNode;
  description: ReactNode;
  action: ReactNode;
  feedback?: Feedback;
}) {
  return (
    <div className="settings-action-row data-settings__row">
      <div className="settings-action-row__content">
        <strong className="settings-row__name">{icon}{title}</strong>
        <div className="settings-row__description">{description}</div>
        {feedback && (
          <p className={feedback.error ? "settings-msg settings-msg--warn" : "settings-msg"}
            role={feedback.error ? "alert" : "status"}>{feedback.message}</p>
        )}
      </div>
      {action}
    </div>
  );
}

export function DataSettingsPanel({ navigation, recoveryRequested = false, legacyRequested = false }: {
  navigation: ReactNode;
  recoveryRequested?: boolean;
  legacyRequested?: boolean;
}) {
  const [directory, setDirectory] = useState<{ path?: string; error?: string }>({});
  const [lastRestoreError, setLastRestoreError] = useState<string>();
  const [feedback, setFeedback] = useState<Partial<Record<DataAction, Feedback>>>({});
  const [busy, setBusy] = useState<DataAction | null>(null);
  const actionInFlight = useRef(false);
  const [hasRecoveryArchive, setHasRecoveryArchive] = useState(false);
  const [legacyArchive, setLegacyArchive] = useState<"available" | "empty" | "error">();
  const [operations, setOperations] = useState<Awaited<ReturnType<typeof pendingExternalOperations>>>();
  const [verificationNotes, setVerificationNotes] = useState<Record<string, string>>({});
  const { requestConfirmation, dialog } = useAppDialog();
  useSettingsDraft({ dirty: false, busy: busy !== null, label: "本机数据" });
  const reviewFeedback = feedback["legacy-review"];
  const showLegacyExport = legacyRequested || legacyArchive === "available" || legacyArchive === "error";
  const showLegacyReview = Boolean(operations?.length || reviewFeedback) || busy === "legacy-review";
  const checkingOperations = !operations && !reviewFeedback && "__TAURI_INTERNALS__" in window;

  const loadDirectory = useCallback(async () => {
    setDirectory({});
    try {
      setDirectory({ path: await echoAgentDataDir() });
    } catch (error) {
      setDirectory({ error: `读取数据目录失败：${errorMessage(error)}` });
    }
  }, []);

  useEffect(() => {
    void loadDirectory();
    void backupLastError().then(error => {
      if (error) setLastRestoreError(`上次恢复失败，已保留原有数据：${error}`);
    }).catch(() => {});
    void hasDeferredUiArchive().then(setHasRecoveryArchive).catch(() => {});
  }, [loadDirectory]);

  useEffect(() => {
    if (!("__TAURI_INTERNALS__" in window)) return;
    let active = true;
    void hasLegacyWorkspaceArchive().then(available => {
      if (active) setLegacyArchive(available ? "available" : "empty");
    }).catch(() => { if (active) setLegacyArchive("error"); });
    void pendingExternalOperations().then(pending => {
      if (active) setOperations(pending);
    }).catch(error => {
      if (active) setFeedback(current => ({
        ...current,
        "legacy-review": { message: `读取旧版外部操作失败：${errorMessage(error)}`, error: true },
      }));
    });
    return () => { active = false; };
  }, []);

  async function loadOperations() {
    setOperations(await pendingExternalOperations());
  }

  async function runAction(id: DataAction, failure: string, action: () => Promise<string | void>) {
    if (actionInFlight.current) return;
    actionInFlight.current = true;
    setBusy(id);
    setFeedback(current => ({ ...current, [id]: undefined }));
    try {
      const message = await action();
      if (message) setFeedback(current => ({ ...current, [id]: { message } }));
    } catch (error) {
      setFeedback(current => ({ ...current, [id]: { message: `${failure}${errorMessage(error)}`, error: true } }));
    } finally {
      actionInFlight.current = false;
      setBusy(null);
    }
  }

  async function chooseBackup(returnFocus: HTMLButtonElement) {
    await runAction("restore", "备份校验失败：", async () => {
      const preview = await inspectBackup();
      if (!preview) return;
      const size = (preview.totalBytes / 1024 / 1024).toFixed(1);
      const createdAt = new Date(preview.createdAt).toLocaleString();
      requestConfirmation({
        title: "恢复备份并重启？",
        description: (
          <>
            <p>备份创建于 {createdAt}，已校验 {preview.fileCount} 个文件（约 {size} MB）。</p>
            <p>恢复会停止当前任务、替换同名记录并重启应用。原文件保留在本地数据目录中，文件夹名称以 restore-previous- 开头。</p>
            <p>备份中的项目目录关联也会恢复，请只恢复可信来源的备份。定时任务和待发送消息会暂停，检查后可手动恢复。</p>
          </>
        ),
        confirmLabel: "恢复并重启",
        danger: true,
        returnFocus,
        action: async () => {
          setLastRestoreError(undefined);
          setBusy("restore");
          try { await restoreBackup(preview.token); } finally { setBusy(null); }
        },
      });
    });
  }

  async function verifyOperation(operationId: string, note: string) {
    await runAction("legacy-review", "保存核验失败：", async () => {
      await confirmExternalOperationStopped(operationId, note);
      setOperations(current => current?.filter(operation => operation.operationId !== operationId));
      setVerificationNotes(current => {
        const next = { ...current };
        delete next[operationId];
        return next;
      });
      try {
        await loadOperations();
      } catch (error) {
        return `核验已保存，列表刷新失败：${errorMessage(error)}`;
      }
      return "核验已保存，已解除这项操作的目录限制。";
    });
  }

  return (
    <SettingsSection navigation={navigation} title="本机数据"
      desc="备份会话和项目资料，换机或重装后可从备份恢复。">
      {dialog}
      <SettingsGroup title="备份与恢复" settingId="backups">
        <DataActionRow title="创建备份" icon={<Download size={17} aria-hidden="true" />}
          description="把会话、项目资料、文档版本和记忆保存为 ZIP 文件。API Key、组织登录凭据和外部工作目录的文件需另行准备。"
          feedback={feedback.export}
          action={<button type="button" className="settings-btn settings-btn--primary" disabled={busy !== null}
            aria-busy={busy === "export"} onClick={() => void runAction("export", "备份未生成。", async () => {
              const path = await exportBackup();
              if (path) return `备份已保存到 ${path}`;
            })}>{busy === "export" ? "正在导出…" : "导出备份"}</button>} />
        <DataActionRow title="从备份恢复" icon={<ArchiveRestore size={17} aria-hidden="true" />}
          description="选择并检查备份文件。确认恢复后会替换同名记录并重启应用。"
          feedback={feedback.restore ?? (lastRestoreError ? { message: lastRestoreError, error: true } : undefined)}
          action={<button type="button" className="settings-btn" disabled={busy !== null} aria-busy={busy === "restore"}
            onClick={event => void chooseBackup(event.currentTarget)}>{busy === "restore" ? "正在处理…" : "选择备份文件"}</button>} />
      </SettingsGroup>

      <SettingsGroup title="本地存储" settingId="data-directory">
        <DataActionRow title="本地数据目录" icon={<Folder size={17} aria-hidden="true" />}
          description={directory.path ? <code className="data-settings__path">{directory.path}</code> : directory.error ? "暂时无法显示存储位置。" : "正在读取存储位置…"}
          feedback={directory.error ? { message: directory.error, error: true } : feedback.directory}
          action={directory.error
            ? <button type="button" className="settings-btn" onClick={() => void loadDirectory()}>重试读取</button>
            : <button type="button" className="settings-btn" disabled={busy !== null || !directory.path}
              aria-busy={busy === "directory"} onClick={() => void runAction("directory", "打开数据目录失败：", openEchoAgentDataDir)}>
              <FolderOpen size={15} aria-hidden="true" />打开文件夹
            </button>} />
      </SettingsGroup>

      {(hasRecoveryArchive || recoveryRequested) && (
        <SettingsGroup title="待恢复数据" settingId="ui-recovery">
          <DataActionRow title="草稿与待发送消息" icon={<ArchiveRestore size={17} aria-hidden="true" />}
            description="导出恢复时暂未写入应用的草稿、待发送消息等数据。"
            feedback={feedback.recovery}
            action={<button type="button" className="settings-btn" disabled={busy !== null} aria-busy={busy === "recovery"}
              onClick={() => void runAction("recovery", "导出恢复档案失败：", async () => {
                const path = await exportDeferredUiArchive();
                if (path) return `恢复档案已保存到 ${path}`;
              })}>{busy === "recovery" ? "正在导出…" : "导出恢复档案"}</button>} />
        </SettingsGroup>
      )}

      {(showLegacyExport || showLegacyReview) && (
        <SettingsGroup title={showLegacyExport ? "旧版工作台资料" : "待核验的外部操作"}
          settingId="legacy-workspace">
          {showLegacyExport && <DataActionRow title="旧版草稿和队列" icon={<Download size={17} aria-hidden="true" />}
            description={legacyArchive === "error" ? "无法确认档案状态，可以尝试导出并查看结果。" : "将旧版计划、草稿和待发送消息导出为 JSON。"}
            feedback={feedback["legacy-export"]}
            action={<button type="button" className="settings-btn" disabled={busy !== null} aria-busy={busy === "legacy-export"}
              onClick={() => void runAction("legacy-export", "导出旧版资料失败：", async () => {
                const path = await exportLegacyWorkspaceArchive();
                if (path) return `旧版工作台资料已保存到 ${path}`;
              })}>{busy === "legacy-export" ? "正在导出…" : "导出旧版草稿和队列"}</button>} />}
          {(showLegacyReview || legacyRequested) && <DataActionRow title="核验目录限制" icon={<ListChecks size={17} aria-hidden="true" />}
            description={operations?.length === 0 ? "没有待核验的旧版外部操作。" : "确认旧版操作已经停止后，可解除对应的目录限制。"}
            feedback={reviewFeedback}
            action={<button type="button" className="settings-btn" disabled={busy !== null || checkingOperations}
              aria-busy={busy === "legacy-review" || checkingOperations}
              onClick={() => void runAction("legacy-review", "读取旧版外部操作失败：", loadOperations)}>
              {checkingOperations ? "正在读取…" : operations ? "刷新操作列表" : "查看待核验操作"}
            </button>} />}
          {operations?.map(operation => {
            const note = verificationNotes[operation.operationId] ?? "";
            return (
              <div className="data-settings__verification" key={operation.operationId}>
                <strong className="settings-row__name">{operation.toolName}</strong>
                <code className="data-settings__path">{operation.workspace}</code>
                <label className="settings-row__description">
                  核验依据（至少 8 个字）
                  <textarea className="settings-input" maxLength={2000} value={note} onChange={event => {
                    const value = event.currentTarget.value;
                    setVerificationNotes(current => ({ ...current, [operation.operationId]: value }));
                  }} />
                </label>
                <button type="button" className="settings-btn" disabled={busy !== null || [...note.trim()].length < 8}
                  onClick={() => void verifyOperation(operation.operationId, note)}>核验并解除目录限制</button>
              </div>
            );
          })}
        </SettingsGroup>
      )}
    </SettingsSection>
  );
}
