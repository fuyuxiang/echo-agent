/** Everyday notification preferences, with optional external delivery setup. */
import { useCallback, useEffect, useId, useRef, useState } from "react";
import { ChevronDown, Plus } from "lucide-react";
import {
  loadNotifyChannels,
  saveNotifyChannel,
  removeNotifyChannel,
  setNotifyChannelEnabled,
  testNotifyChannel,
  loadDesktopNotifyStatus,
  setDesktopNotifyEnabled,
  testDesktopNotify,
  type DesktopNotifyStatus,
  type NotifyChannel,
  type ChannelKind,
} from "@/lib/notify-channels";
import { useAppDialog } from "./AppDialog";
import { SettingsGroup } from "./SettingsSections";
import { desktopPreferencesGet, desktopNotificationPreviewSave } from "@/lib/agent-client";
import { useSettingsDraft } from "@/lib/settings-drafts";

const KIND_LABELS: Record<ChannelKind, string> = {
  "slack-webhook": "Slack",
  "discord-webhook": "Discord",
  "generic-webhook": "Webhook",
  email: "邮件（旧配置，已停用）",
  desktop: "桌面通知",
};

const ADDABLE_KINDS: ChannelKind[] = [
  "slack-webhook",
  "discord-webhook",
  "generic-webhook",
];

export function NotifyChannelsPanel({ onToast, sessionId, onOpenWeixin }: { onToast?: (msg: string) => void; sessionId?: string; onOpenWeixin?: () => void }) {
  const [channels, setChannels] = useState<NotifyChannel[]>([]);
  const [desktop, setDesktop] = useState<DesktopNotifyStatus | null>(null);
  const [desktopTestResult, setDesktopTestResult] = useState<string | null>(null);
  const [desktopLoading, setDesktopLoading] = useState(true);
  const [desktopError, setDesktopError] = useState<string | null>(null);
  const [showTaskTitle, setShowTaskTitle] = useState(false);
  const [previewLoading, setPreviewLoading] = useState(true);
  const [previewSaving, setPreviewSaving] = useState(false);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [previewReload, setPreviewReload] = useState(0);
  const [advancedOpen, setAdvancedOpen] = useState(false);
  const [editorOpen, setEditorOpen] = useState(false);
  const advancedId = useId();
  // 新增和编辑共用表单；编辑时绝不回填后端遮蔽的 Webhook 凭据。
  const [editingId, setEditingId] = useState<string | null>(null);
  const [newKind, setNewKind] = useState<ChannelKind>("slack-webhook");
  const [newLabel, setNewLabel] = useState("");
  const [newEndpoint, setNewEndpoint] = useState("");
  const { requestConfirmation, dialog } = useAppDialog();
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [mutating, setMutating] = useState<string | null>(null);
  const loadGeneration = useRef(0);
  const desktopGeneration = useRef(0);
  const mutationInFlight = useRef(false);
  const [feedback, setFeedback] = useState<{ text: string; error: boolean } | null>(null);
  const reportFeedback = (text: string, error = true) => { setFeedback({ text, error }); onToast?.(text); };
  const originalChannel = channels.find(channel => channel.id === editingId);
  const editorDirty = editorOpen && (editingId
    ? newLabel !== originalChannel?.label || Boolean(newEndpoint.trim())
    : Boolean(newLabel.trim() || newEndpoint.trim() || newKind !== "slack-webhook"));
  useSettingsDraft({ dirty: Boolean(editorDirty), busy: mutating !== null || previewSaving, label: "通知渠道" });
  const discardEditor = (action: () => void) => {
    if (mutationInFlight.current) return;
    if (!editorDirty) { action(); return; }
    requestConfirmation({ title: "舍弃通知渠道的修改？", description: "当前渠道尚未保存。已保存的通知配置不会改变。", confirmLabel: "舍弃修改", danger: true, action });
  };

  const load = useCallback(async () => {
    const generation = ++loadGeneration.current;
    setLoading(true);
    setLoadError(null);
    try {
      const items = await loadNotifyChannels();
      if (loadGeneration.current === generation) setChannels(items);
      return loadGeneration.current === generation;
    } catch (error) {
      if (loadGeneration.current !== generation) return false;
      const message = String(error).replace(/^Error:\s*/, "");
      setLoadError(message);
      return false;
    } finally {
      if (loadGeneration.current === generation) setLoading(false);
    }
  }, []);

  const loadDesktop = useCallback(async () => {
    const generation = ++desktopGeneration.current;
    setDesktopLoading(true);
    setDesktopError(null);
    try {
      const status = await loadDesktopNotifyStatus();
      if (desktopGeneration.current === generation) setDesktop(status);
    } catch (error) {
      if (desktopGeneration.current === generation) setDesktopError(String(error).replace(/^Error:\s*/, ""));
    } finally {
      if (desktopGeneration.current === generation) setDesktopLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
    void loadDesktop();
    return () => {
      loadGeneration.current += 1;
      desktopGeneration.current += 1;
    };
  }, [load, loadDesktop]);

  useEffect(() => {
    let active = true;
    setPreviewLoading(true);
    setPreviewError(null);
    void desktopPreferencesGet().then((preferences) => {
      if (active) setShowTaskTitle(preferences.showNotificationTaskTitle ?? false);
    }).catch((error) => {
      if (active) setPreviewError(String(error).replace(/^Error:\s*/, ""));
    }).finally(() => {
      if (active) setPreviewLoading(false);
    });
    return () => { active = false; };
  }, [previewReload]);

  const savePreview = async (enabled: boolean) => {
    setPreviewSaving(true);
    setPreviewError(null);
    try {
      const saved = await desktopNotificationPreviewSave(enabled);
      setShowTaskTitle(saved.showNotificationTaskTitle);
    } catch (error) {
      setPreviewError(String(error).replace(/^Error:\s*/, ""));
    } finally {
      setPreviewSaving(false);
    }
  };

  const save = async () => {
    if (mutationInFlight.current) return;
    const existing = editingId ? channels.find((item) => item.id === editingId) : undefined;
    if (editingId && !existing) return;
    mutationInFlight.current = true;
    const id = editingId ?? `ch_${Date.now()}`;
    const kind = existing?.kind ?? newKind;
    const label = newLabel.trim() || KIND_LABELS[kind];
    const channel: NotifyChannel = {
      id,
      label,
      kind,
      endpoint: newEndpoint.trim() || undefined,
      enabled: existing?.enabled ?? true,
    };
    setMutating("save");
    let refreshed = false;
    try {
      await saveNotifyChannel(channel);
      // Only the backend's redacted view may enter list state. Never render a
      // freshly submitted webhook secret in the channel list.
      refreshed = await load();
    } catch (error) {
      reportFeedback(`${editingId ? "保存" : "添加"}失败：${String(error).replace(/^Error:\s*/, "")}`);
      return;
    } finally {
      mutationInFlight.current = false;
      setMutating(null);
    }
    setNewLabel("");
    setNewEndpoint("");
    setNewKind("slack-webhook");
    setEditingId(null);
    setEditorOpen(false);
    reportFeedback(refreshed ? `通知渠道“${label}”已保存，立即用于后续通知。` : `通知渠道“${label}”已保存，列表暂未更新，请刷新重试`, false);
  };

  const edit = (channel: NotifyChannel) => {
    setEditingId(channel.id);
    setNewKind(channel.kind);
    setNewLabel(channel.label);
    setNewEndpoint("");
    setEditorOpen(true);
  };

  const cancelEdit = () => {
    setEditingId(null);
    setNewKind("slack-webhook");
    setNewLabel("");
    setNewEndpoint("");
    setEditorOpen(false);
  };

  const remove = (channel: NotifyChannel) => {
    requestConfirmation({
      title: "移除通知渠道？",
      description: `将移除“${channel.label}”的本机配置，之后不再向该渠道发送通知。`,
      confirmLabel: "移除",
      danger: true,
      action: async () => {
        await removeNotifyChannel(channel.id);
        setChannels((items) => items.filter((item) => item.id !== channel.id));
        if (editingId === channel.id) cancelEdit();
      },
    });
  };

  const toggle = async (id: string) => {
    if (mutationInFlight.current) return;
    const ch = channels.find((c) => c.id === id);
    if (!ch) return;
    mutationInFlight.current = true;
    setMutating(`toggle:${id}`);
    try {
      await setNotifyChannelEnabled(id, !ch.enabled);
      setChannels((items) => items.map((item) => item.id === id ? { ...item, enabled: !item.enabled } : item));
    } catch (error) {
      reportFeedback(`修改失败：${String(error).replace(/^Error:\s*/, "")}`);
    } finally {
      mutationInFlight.current = false;
      setMutating(null);
    }
  };

  const testSend = async (id: string) => {
    if (mutationInFlight.current) return;
    mutationInFlight.current = true;
    setMutating(`test:${id}`);
    try {
      const result = await testNotifyChannel(id);
      reportFeedback(result.ok ? "测试通知已向渠道提交" : `发送失败：${result.error ?? "检查渠道地址"}`, !result.ok);
    } catch (error) {
      reportFeedback(`发送失败：${String(error).replace(/^Error:\s*/, "")}`);
    } finally {
      mutationInFlight.current = false;
      setMutating(null);
    }
  };

  const toggleDesktop = async () => {
    if (!desktop || mutationInFlight.current) return;
    mutationInFlight.current = true;
    setMutating("desktop-toggle");
    setDesktopTestResult(null);
    try {
      await setDesktopNotifyEnabled(!desktop.enabled);
      await loadDesktop();
    } catch (error) { reportFeedback(`更新桌面通知失败：${String(error).replace(/^Error:\s*/, "")}`); }
    finally { mutationInFlight.current = false; setMutating(null); }
  };
  const testDesktop = async () => {
    if (mutationInFlight.current) return;
    mutationInFlight.current = true;
    setMutating("desktop-test");
    setDesktopTestResult(null);
    try {
      const result = await testDesktopNotify(sessionId);
      setDesktopTestResult(result.ok
        ? `已向系统提交通知。${sessionId ? "点击提醒可返回当前任务。" : ""}若未看到，请检查系统通知设置和勿扰模式。`
        : `桌面提醒提交失败：${result.error ?? "未知错误"}`);
    } catch (error) { setDesktopTestResult(`桌面提醒提交失败：${String(error).replace(/^Error:\s*/, "")}`); }
    finally { mutationInFlight.current = false; setMutating(null); }
  };
  const customChannels = channels.filter(channel => channel.kind !== "desktop");

  return (
    <div className="notify-panel" role="region" aria-label="通知设置">
      <SettingsGroup title="桌面通知" settingId="desktop-notifications">
        <div className="settings-row settings-row--comfortable notify-panel__preference">
          <div className="settings-row__label settings-row__label--stacked">
            <span className="settings-row__name">桌面通知</span>
            <span className="settings-row__description">任务完成、失败或需要确认时，通过系统通知提醒你。关闭后仍保留应用内通知记录。</span>
            <span className="settings-preference-state" aria-live="polite">{desktopLoading ? "正在读取状态…" : desktopError ? "读取失败，请重试" : mutating === "desktop-toggle" ? "正在保存…" : desktop?.source === "policy" ? "组织策略已限制桌面通知" : desktop?.enabled ? desktop.source === "default" ? "已开启 · 默认设置" : "已开启" : "已关闭"}</span>
          </div>
          <label className="sk-toggle">
            <input type="checkbox" role="switch" aria-label="桌面通知" checked={desktop?.enabled ?? false}
              onChange={() => void toggleDesktop()} disabled={desktopLoading || !!desktopError || mutating !== null || !desktop || desktop.source === "policy"} />
            <span className="sk-toggle-track"><span className="sk-toggle-thumb" /></span>
          </label>
        </div>
        {desktopError && <div className="panel-inline-error" role="alert">
          <span>桌面提醒状态不可用：{desktopError}</span>
          <button type="button" className="settings-btn" onClick={() => void loadDesktop()} disabled={desktopLoading}>重试读取桌面提醒</button>
        </div>}
        {desktop?.policyError && <p className="settings-msg settings-msg--warn">{desktop.policyError}</p>}
        <div className="notify-panel__test-row">
          <details className="notify-panel__help">
            <summary>收不到桌面通知？</summary>
            <p>请检查系统通知权限与勿扰模式。macOS：系统设置 → 通知；Windows：设置 → 系统 → 通知，找到 EchoAgent 并允许通知。</p>
            <p>查看当前任务时，不重复弹出该任务的常规提醒。定时任务按各自的通知设置提醒。</p>
            <p>应用目前无法读取系统授权状态。测试提交成功表示系统已接收请求，是否弹出仍由系统决定。</p>
          </details>
          <button type="button" className="settings-btn" onClick={() => void testDesktop()} disabled={!desktop?.enabled || desktopLoading || !!desktopError || mutating !== null}>{mutating === "desktop-test" ? "正在提交…" : "发送测试通知"}</button>
        </div>
        {desktopTestResult && <p className="notify-panel__feedback" role="status">{desktopTestResult}</p>}
      </SettingsGroup>

      <SettingsGroup title="通知内容" settingId="notification-content">
        <div className="settings-row settings-row--comfortable notify-panel__preference">
          <div className="settings-row__label settings-row__label--stacked">
            <span className="settings-row__name">在通知中显示任务标题</span>
            <span className="settings-row__description">关闭后，新通知将隐藏任务标题，减少锁屏信息暴露。点击通知仍可回到关联任务。</span>
          </div>
          <label className="sk-toggle">
            <input type="checkbox" role="switch" aria-label="在通知中显示任务标题" checked={showTaskTitle}
              disabled={previewLoading || previewSaving || Boolean(previewError)}
              onChange={(event) => void savePreview(event.target.checked)} />
            <span className="sk-toggle-track"><span className="sk-toggle-thumb" /></span>
          </label>
        </div>
        {previewError && <p className="settings-msg settings-msg--warn" role="alert">
          通知内容设置不可用：{previewError}
          <button type="button" className="settings-btn" onClick={() => setPreviewReload((value) => value + 1)}>重试读取通知内容</button>
        </p>}
      </SettingsGroup>

      <section className="notify-advanced" data-setting-id="external-notifications">
        <button type="button" className="notify-advanced__toggle" aria-expanded={advancedOpen} aria-controls={advancedId} onClick={() => setAdvancedOpen(!advancedOpen)}>
          <span className="notify-advanced__heading"><span className="settings-group__title">推送到其他应用</span><span className="settings-group__desc">高级选项 · Slack、Discord 和 Webhook</span></span>
          <span className="notify-advanced__count">{loading ? "正在读取…" : loadError ? "读取失败" : customChannels.length ? `${customChannels.length} 个已配置` : "未配置"}</span>
          <ChevronDown size={16} aria-hidden="true" />
        </button>
        {advancedOpen && <div className="notify-advanced__content" id={advancedId}>
          <p className="notify-panel__external-note">将任务事件发送到已配置的外部应用，接收范围遵循各任务的通知设置。远程对话与账号绑定请前往{onOpenWeixin ? <button type="button" className="settings-btn settings-btn--ghost settings-btn--compact" onClick={onOpenWeixin}>微信连接</button> : "“设置 → 微信连接”"}。</p>
          {loadError && (
            <div className="panel-inline-error" role="alert">
              <span>通知渠道加载失败：{loadError}</span>
              <button type="button" className="settings-btn" onClick={() => void load()} disabled={loading}>
                {loading ? "重试中…" : "重试"}
              </button>
            </div>
          )}
          {loading && customChannels.length === 0 && <div className="notify-panel__empty">正在加载通知渠道…</div>}

          {customChannels.length > 0 ? (
            <ul className="notify-panel__list">
              {customChannels.map((ch) => (
                <li key={ch.id} className={"notify-panel__row" + (ch.enabled ? "" : " disabled")}>
                  <div className="notify-panel__channel-info">
                    <span className="notify-panel__row-label">{ch.label}</span>
                    <span className="notify-panel__channel-description">{KIND_LABELS[ch.kind]}{ch.kind === "email" ? " · 仅保留旧配置，可移除" : ch.endpoint ? ` · ${ch.endpoint}` : ""}</span>
                  </div>
                  <div className="notify-panel__row-actions">
                    {ch.kind !== "email" && <>
                      <button type="button" className="settings-btn settings-btn--compact notify-panel__btn" role="switch" aria-checked={ch.enabled} onClick={() => void toggle(ch.id)} title={ch.enabled ? "禁用" : "启用"} aria-label={`${ch.enabled ? "禁用" : "启用"} ${ch.label}`} disabled={loading || mutating !== null || !!loadError}>
                        {ch.enabled ? "已开启" : "已关闭"}
                      </button>
                      <button type="button" className="settings-btn settings-btn--compact notify-panel__btn" onClick={() => discardEditor(() => edit(ch))} disabled={loading || mutating !== null || !!loadError}>编辑</button>
                      <button type="button" className="settings-btn settings-btn--compact notify-panel__btn" onClick={() => void testSend(ch.id)} disabled={!ch.enabled || mutating !== null || !!loadError} title="测试发送">
                        测试
                      </button>
                    </>}
                    <button type="button" className="settings-btn settings-btn--compact settings-btn--danger notify-panel__btn" onClick={() => remove(ch)} title="移除" aria-label={`移除 ${ch.label}`} disabled={mutating !== null}>
                      移除
                    </button>
                  </div>
                </li>
              ))}
            </ul>
          ) : !loading && !loadError ? (
            <div className="notify-panel__empty">尚未添加外部推送，日常桌面提醒无需配置这里。</div>
          ) : null}

          {!editorOpen && <button type="button" className="settings-btn notify-panel__add-button" onClick={() => { cancelEdit(); setEditorOpen(true); }} disabled={loading || !!loadError || mutating !== null}><Plus size={14} />添加渠道</button>}
          {editorOpen && <form className="notify-panel__editor" aria-label={editingId ? "编辑通知渠道" : "添加通知渠道"} onSubmit={(event) => { event.preventDefault(); void save(); }}>
            <h4>{editingId ? "编辑渠道" : "添加渠道"}</h4>
            {editingId && <p className="notify-panel__edit-hint">正在编辑“{channels.find((item) => item.id === editingId)?.label}”。Webhook 地址已隐藏；留空表示保持原地址。</p>}
            <div className="notify-panel__add">
              <label className="notify-panel__field">渠道类型<select className="form-control" autoFocus={!editingId} aria-label="通知渠道类型" value={newKind} disabled={Boolean(editingId) || mutating !== null || loading || Boolean(loadError)} onChange={(e) => setNewKind(e.target.value as ChannelKind)}>
                {ADDABLE_KINDS.map((kind) => (
                  <option key={kind} value={kind}>{KIND_LABELS[kind]}</option>
                ))}
              </select></label>
              <label className="notify-panel__field">显示名（可选）<input
                className="form-control"
                type="text"
                autoFocus={Boolean(editingId)}
                aria-label="通知渠道显示名"
                placeholder="显示名(可选)"
                value={newLabel}
                disabled={mutating !== null || loading || Boolean(loadError)}
                onChange={(e) => setNewLabel(e.target.value)}
              /></label>
              {newKind !== "desktop" && <label className="notify-panel__field notify-panel__field--endpoint">{editingId ? "替换 Webhook URL（留空保留）" : "Webhook URL"}<input
                className="form-control"
                type="url"
                autoComplete="off"
                spellCheck={false}
                required={!editingId}
                aria-label="Webhook URL"
                placeholder={editingId ? "留空保留现有地址" : "Webhook URL"}
                value={newEndpoint}
                onChange={(e) => setNewEndpoint(e.target.value)}
                disabled={mutating !== null || loading || Boolean(loadError)}
              /></label>}
              <div className="notify-panel__form-actions">
                <button type="button" className="form-button" onClick={() => discardEditor(cancelEdit)} disabled={mutating !== null}>取消</button>
                <button type="submit" className="form-button form-button--primary" disabled={mutating !== null || loading || Boolean(loadError) || (!editingId && newKind !== "desktop" && !newEndpoint.trim())}>
                  {mutating === "save" ? "保存中…" : editingId ? "保存修改" : "+ 添加"}
                </button>
              </div>
            </div>
          </form>}
        </div>}
      </section>
      {feedback && <p className={`settings-msg${feedback.error ? " settings-msg--warn" : ""}`} role={feedback.error ? "alert" : "status"}>{feedback.text}</p>}
      {dialog}
    </div>
  );
}
