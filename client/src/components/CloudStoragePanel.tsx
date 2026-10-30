import { SettingsDisclosure } from "./SettingsDisclosure";
/**
 * 云存储浏览面板 —— 腾讯 Drive/文档 替代的 UI。
 *
 * 列出已注册的 WebDAV 存储源，并提供浏览、读取、写入与删除操作。
 */
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import {
  hydrateStorageProviders,
  saveStorageProviderConfig,
  removeStorageProviderConfig,
  testStorageProviderConfig,
  createTauriWebDavProvider,
  registerStorageProvider,
  unregisterStorageProvider,
  listStorageProviders,
  getStorageProvider,
  joinStoragePath,
  normalizePath,
  type StorageEntry,
  type StorageProviderConfig,
} from "@/lib/cloud-storage";
import { ArrowUp, Download, Eye, FileText, Folder, Trash2 } from "lucide-react";
import { useAppDialog } from "./AppDialog";
import { useSettingsDraft } from "@/lib/settings-drafts";

export function CloudStoragePanel({ onToast, onUnsavedChange }: {
  onToast?: (msg: string) => void;
  onUnsavedChange?: (dirty: boolean) => void;
}) {
  const [providers, setProviders] = useState<Array<{ id: string; label: string }>>([]);
  const [configs, setConfigs] = useState<StorageProviderConfig[]>([]);
  const configsRef = useRef<StorageProviderConfig[]>([]);
  const [selectedProvider, setSelectedProvider] = useState<string | null>(null);
  const selectedProviderRef = useRef<string | null>(null);
  // Keep registry-only providers and make disabled persisted configs manageable.
  const providerOptions = [...providers, ...configs
    .filter((config) => !config.enabled && !providers.some((provider) => provider.id === config.id))
    .map(({ id, label }) => ({ id, label }))];
  const selectedEnabled = Boolean(selectedProvider
    && configs.find((config) => config.id === selectedProvider)?.enabled !== false
    && providers.some((provider) => provider.id === selectedProvider));
  const [entries, setEntries] = useState<StorageEntry[]>([]);
  const [currentPath, setCurrentPath] = useState("/");
  const [loading, setLoading] = useState(false);
  const [configLoading, setConfigLoading] = useState(true);
  const [configError, setConfigError] = useState<string | null>(null);
  const [browseError, setBrowseError] = useState<string | null>(null);
  const [showConfig, setShowConfig] = useState(false);
  const [preview, setPreview] = useState<{
    providerId: string;
    name: string;
    path: string;
    content: string;
    savedContent: string;
  } | null>(null);
  const [readingPath, setReadingPath] = useState<string | null>(null);
  const previewRef = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    if (!preview) return;
    previewRef.current?.scrollIntoView?.({ block: "end" });
    previewRef.current?.focus({ preventScroll: true });
  }, [preview?.providerId, preview?.path]);
  const previewDirty = Boolean(preview && preview.content !== preview.savedContent);

  useEffect(() => {
    onUnsavedChange?.(previewDirty);
  }, [onUnsavedChange, previewDirty]);

  useEffect(() => {
    return () => onUnsavedChange?.(false);
  }, [onUnsavedChange]);
  const [busyAction, setBusyAction] = useState<string | null>(null);
  const [configSaving, setConfigSaving] = useState(false);
  const { requestConfirmation, requestInput, dialog } = useAppDialog(`${selectedProvider ?? ""}\u0000${currentPath}`);
  const configGeneration = useRef(0);
  const browseGeneration = useRef(0);
  const previewReadGeneration = useRef(0);
  const configSavingRef = useRef(false);
  const [draft, setDraft] = useState<StorageProviderConfig>({
    id: "",
    label: "",
    kind: "webdav",
    baseUrl: "",
    username: "",
    password: "",
    enabled: true,
  });
  const configBaseline = useRef(JSON.stringify(draft));
  const configDirty = showConfig && JSON.stringify(draft) !== configBaseline.current;
  useSettingsDraft({ dirty: configDirty || previewDirty, busy: configSaving || busyAction !== null, label: "云存储配置或文件" });
  const confirmConfigChange = (action: () => void) => {
    if (configSavingRef.current) return;
    if (!configDirty) { action(); return; }
    requestConfirmation({ title: "舍弃存储源的修改？", description: "当前存储源配置尚未保存。", confirmLabel: "舍弃修改", danger: true, action });
  };

  const loadConfigs = useCallback(async () => {
    const generation = ++configGeneration.current;
    setConfigLoading(true);
    setConfigError(null);
    try {
      const loaded = await hydrateStorageProviders();
      if (configGeneration.current !== generation) return;
      configsRef.current = loaded;
      setConfigs(loaded);
      setProviders(listStorageProviders());
    } catch (error) {
      if (configGeneration.current !== generation) return;
      const message = String(error).replace(/^Error:\s*/, "");
      setConfigError(message);
      onToast?.(`读取存储配置失败：${message}`);
    } finally {
      if (configGeneration.current === generation) setConfigLoading(false);
    }
  }, [onToast]);

  useEffect(() => {
    void loadConfigs();
    return () => {
      configGeneration.current += 1;
    };
  }, [loadConfigs]);

  useEffect(() => () => {
    browseGeneration.current += 1;
  }, []);

  const saveConfig = async () => {
    if (configSavingRef.current) return;
    const id = draft.id.trim() || `webdav-${Date.now()}`;
    const config = { ...draft, id, label: draft.label.trim() || "WebDAV", baseUrl: draft.baseUrl.trim() };
    const persist = async () => {
      if (configSavingRef.current) return;
      configSavingRef.current = true;
      setConfigSaving(true);
      try {
        await saveStorageProviderConfig(config);
        registerStorageProvider(createTauriWebDavProvider(config));
        const next = [...configsRef.current.filter((item) => item.id !== config.id), config];
        configsRef.current = next;
        setConfigs(next);
        setProviders(listStorageProviders());
        if (selectedProviderRef.current === config.id) {
          browseGeneration.current += 1;
          previewReadGeneration.current += 1;
          setLoading(false);
          setReadingPath(null);
          setBrowseError(null);
          setEntries([]);
          if (!config.enabled) setPreview(null);
          else void browse(config.id, currentPath);
        }
        setDraft({ id: "", label: "", kind: "webdav", baseUrl: "", username: "", password: "", enabled: true });
        setShowConfig(false);
      } catch (error) {
        onToast?.(`保存失败：${String(error).replace(/^Error:\s*/, "")}`);
      } finally {
        configSavingRef.current = false;
        setConfigSaving(false);
      }
    };
    if (!config.enabled && selectedProvider === config.id && previewDirty) {
      requestConfirmation({
        title: "舍弃未保存的文件修改？",
        description: `“${preview?.name}”的修改尚未保存，禁用存储源会关闭文件预览。`,
        confirmLabel: "舍弃修改并禁用",
        danger: true,
        action: persist,
      });
      return;
    }
    await persist();
  };

  const removeConfig = () => {
    if (!selectedProvider) return;
    const providerId = selectedProvider;
    const label = providerOptions.find((provider) => provider.id === providerId)?.label ?? providerId;
    requestConfirmation({
      title: `移除存储源“${label}”？`,
      description: "只会移除本机的连接配置，远端文件不会被删除。",
      confirmLabel: "移除配置",
      danger: true,
      action: async () => {
        await removeStorageProviderConfig(providerId);
        unregisterStorageProvider(providerId);
        const next = configsRef.current.filter((item) => item.id !== providerId);
        configsRef.current = next;
        setConfigs(next);
        setProviders(listStorageProviders());
        selectedProviderRef.current = null;
        setSelectedProvider(null);
        setEntries([]);
        setPreview(null);
        browseGeneration.current += 1;
        previewReadGeneration.current += 1;
        setLoading(false);
        setReadingPath(null);
        setBrowseError(null);
      },
    });
  };

  const browse = async (providerId: string, path: string) => {
    if (configsRef.current.some((config) => config.id === providerId && !config.enabled)) return;
    const provider = getStorageProvider(providerId);
    if (!provider) return;
    const generation = ++browseGeneration.current;
    setLoading(true);
    setBrowseError(null);
    try {
      const list = await provider.list(normalizePath(path));
      if (browseGeneration.current !== generation) return;
      setEntries(list.sort((a, b) => (a.isDir === b.isDir ? a.name.localeCompare(b.name) : a.isDir ? -1 : 1)));
      setCurrentPath(normalizePath(path));
    } catch (e) {
      if (browseGeneration.current !== generation) return;
      const message = String(e).replace(/^Error:\s*/, "");
      setBrowseError(message);
      onToast?.(`浏览失败：${message}`);
    } finally {
      if (browseGeneration.current === generation) setLoading(false);
    }
  };

  const leavePreview = (action: () => void) => {
    previewReadGeneration.current += 1;
    setReadingPath(null);
    if (preview && preview.content !== preview.savedContent) {
      requestConfirmation({
        title: "舍弃未保存的文件修改？",
        description: `“${preview.name}”的修改尚未保存，继续操作会丢失这些修改。`,
        confirmLabel: "舍弃修改",
        danger: true,
        action: async () => {
          setPreview(null);
          action();
        },
      });
      return;
    }
    setPreview(null);
    action();
  };

  const selectProvider = (id: string) => {
    if (id === selectedProvider) return;
    leavePreview(() => {
      browseGeneration.current += 1;
      setLoading(false);
      setBrowseError(null);
      selectedProviderRef.current = id || null;
      setSelectedProvider(id || null);
      setEntries([]);
      setCurrentPath("/");
      if (id) void browse(id, "/");
    });
  };

  const openEntry = (entry: StorageEntry) => {
    if (entry.isDir && selectedProvider) {
      const providerId = selectedProvider;
      leavePreview(() => { void browse(providerId, entry.path); });
    }
  };

  const goUp = () => {
    if (!selectedProvider) return;
    const parts = currentPath.split("/").filter(Boolean);
    parts.pop();
    const providerId = selectedProvider;
    leavePreview(() => { void browse(providerId, "/" + parts.join("/")); });
  };

  const deleteEntry = (entry: StorageEntry) => {
    if (!selectedProvider) return;
    const providerId = selectedProvider;
    const provider = getStorageProvider(providerId);
    if (!provider) return;
    const path = currentPath;
    requestConfirmation({
      title: `删除${entry.isDir ? "文件夹" : "文件"}“${entry.name}”？`,
      description: "此操作会修改远端存储，且可能无法撤销。",
      confirmLabel: "从远端删除",
      danger: true,
      action: async () => {
        const ok = await provider.delete(entry.path);
        if (!ok) throw new Error("服务端拒绝删除");
        await browse(providerId, path);
      },
    });
  };

  const readFile = async (entry: StorageEntry) => {
    if (!selectedProvider) return;
    const providerId = selectedProvider;
    const provider = getStorageProvider(providerId);
    if (!provider) return;
    leavePreview(() => {
      const generation = ++previewReadGeneration.current;
      setReadingPath(entry.path);
      void (async () => {
        try {
          const content = await provider.readText(entry.path);
          if (generation !== previewReadGeneration.current) return;
          if (content != null) {
            setPreview({ providerId, name: entry.name, path: entry.path, content, savedContent: content });
          } else {
            onToast?.("这个文件暂不支持文本预览，请下载后打开");
          }
        } catch (error) {
          if (generation === previewReadGeneration.current) onToast?.(`读取失败：${String(error).replace(/^Error:\s*/, "")}`);
        } finally {
          if (generation === previewReadGeneration.current) setReadingPath(null);
        }
      })();
    });
  };

  const beginNewConfig = () => confirmConfigChange(() => {
    const next: StorageProviderConfig = { id: "", label: "", kind: "webdav", baseUrl: "", username: "", password: "", enabled: true };
    configBaseline.current = JSON.stringify(next);
    setDraft(next);
    setShowConfig(true);
  });

  const editSelectedConfig = () => confirmConfigChange(() => {
    const config = configs.find((item) => item.id === selectedProvider);
    if (!config) return;
    configBaseline.current = JSON.stringify(config);
    setDraft({ ...config });
    setShowConfig(true);
  });

  const uploadFiles = async () => {
    if (!selectedProvider) return;
    const provider = getStorageProvider(selectedProvider);
    if (!provider?.uploadFile) return;
    try {
      const picked = await openDialog({ multiple: true, directory: false });
      const paths = !picked ? [] : Array.isArray(picked) ? picked : [picked];
      if (paths.length === 0) return;
      const existing = new Set(entries.filter((entry) => !entry.isDir).map((entry) => entry.name));
      const conflicts = paths.filter((path) => existing.has(localBasename(path))).length;
      const providerId = selectedProvider;
      const path = currentPath;
      const performUpload = async () => {
        setBusyAction("上传中…");
        try {
          for (const localPath of paths) {
            await provider.uploadFile!(joinStoragePath(path, localBasename(localPath)), localPath);
          }
          await browse(providerId, path);
          onToast?.(`已上传 ${paths.length} 个文件`);
        } finally {
          setBusyAction(null);
        }
      };
      if (conflicts > 0) {
        requestConfirmation({
          title: `覆盖 ${conflicts} 个同名文件？`,
          description: "远端的同名文件将被本地文件替换，此操作可能无法撤销。",
          confirmLabel: "覆盖并上传",
          danger: true,
          action: performUpload,
        });
        return;
      }
      await performUpload();
    } catch (error) {
      onToast?.(`上传失败：${String(error).replace(/^Error:\s*/, "")}`);
    }
  };

  const downloadFile = async (entry: StorageEntry) => {
    if (!selectedProvider) return;
    const provider = getStorageProvider(selectedProvider);
    if (!provider?.downloadFile) return;
    try {
      const destination = await saveDialog({ defaultPath: entry.name });
      if (!destination) return;
      setBusyAction("下载中…");
      const bytes = await provider.downloadFile(entry.path, destination);
      onToast?.(`已下载“${entry.name}”（${formatSize(bytes)}）`);
    } catch (error) {
      onToast?.(`下载失败：${String(error).replace(/^Error:\s*/, "")}`);
    } finally {
      setBusyAction(null);
    }
  };

  const savePreview = async () => {
    if (!preview || selectedProvider !== preview.providerId) return;
    const snapshot = preview;
    setBusyAction("保存中…");
    try {
      const ok = await getStorageProvider(snapshot.providerId)?.writeText(snapshot.path, snapshot.content);
      if (!ok) throw new Error("写入失败");
      setPreview((current) => current?.providerId === snapshot.providerId && current.path === snapshot.path
        ? { ...current, savedContent: snapshot.content }
        : current);
      if (selectedProvider === snapshot.providerId) await browse(snapshot.providerId, currentPath);
    } catch (error) {
      onToast?.(`保存失败：${String(error).replace(/^Error:\s*/, "")}`);
    } finally {
      setBusyAction(null);
    }
  };

  const createFolder = () => {
    if (!selectedProvider) return;
    const providerId = selectedProvider;
    const path = currentPath;
    requestInput({
      title: "新建文件夹",
      description: `将在 ${path} 中创建。`,
      confirmLabel: "创建",
      fields: [{ name: "name", label: "文件夹名称", required: true, maxLength: 255 }],
      validate: (values) => validateStorageName(values.name),
      action: async (values) => {
        const name = values.name.trim();
        const ok = await getStorageProvider(providerId)?.makeDir(joinStoragePath(path, name));
        if (!ok) throw new Error("服务端未创建文件夹");
        await browse(providerId, path);
      },
    });
  };

  const createTextFile = () => {
    if (!selectedProvider) return;
    const providerId = selectedProvider;
    const path = currentPath;
    requestInput({
      title: "新建文本文件",
      description: `将在 ${path} 中创建。文件名和内容可以一次填写。`,
      confirmLabel: "创建文件",
      fields: [
        { name: "name", label: "文件名", defaultValue: "note.md", required: true, maxLength: 255 },
        { name: "content", label: "文件内容", multiline: true, rows: 8, maxLength: 1024 * 1024 },
      ],
      validate: (values) => validateStorageName(values.name),
      action: async (values) => {
        const name = values.name.trim();
        const ok = await getStorageProvider(providerId)?.writeText(joinStoragePath(path, name), values.content);
        if (!ok) throw new Error("服务端未写入文件");
        await browse(providerId, path);
      },
    });
  };

  return (
    <div className="storage-panel" role="region" aria-label="云存储">
      <div className="storage-panel__head">
        <span className="storage-panel__title">存储源</span>
        {providerOptions.length > 0 ? (
          <select className="form-control" aria-label="选择云存储源" value={selectedProvider ?? ""} onChange={(e) => selectProvider(e.target.value)}>
            <option value="">选择存储源…</option>
            {providerOptions.map((p) => (
              <option key={p.id} value={p.id}>{p.label}{configs.some((config) => config.id === p.id && !config.enabled) ? "（已禁用）" : ""}</option>
            ))}
          </select>
        ) : (
          <span className="storage-panel__muted">
            {configLoading ? "正在读取存储配置…" : configError ? "存储配置暂不可用" : "未配置存储源"}
          </span>
        )}
        <div className="storage-panel__head-actions">
          <button type="button" className="form-button form-button--primary" onClick={beginNewConfig} disabled={configLoading || Boolean(configError) || configSaving}>添加存储源</button>
          {selectedProvider && selectedEnabled && <button type="button" className="form-button" onClick={() => void testStorageProviderConfig(selectedProvider).then(() => onToast?.("连接正常")).catch((e) => onToast?.(`连接失败：${String(e).replace(/^Error:\s*/, "")}`))}>测试连接</button>}
          {selectedProvider && <button type="button" className="form-button" onClick={editSelectedConfig} disabled={configSaving}>编辑配置</button>}
          {selectedProvider && <button type="button" className="form-button form-button--danger" onClick={() => void removeConfig()} disabled={configSaving}>移除配置</button>}
        </div>
      </div>

      {configError && (
        <div className="panel-inline-error" role="alert">
          <span>读取存储配置失败：{configError}</span>
          <button type="button" className="form-button" onClick={() => void loadConfigs()} disabled={configLoading}>
            {configLoading ? "重试中…" : "重试"}
          </button>
        </div>
      )}

      {showConfig && (
        <div className="storage-panel__config">
          <strong>{draft.id ? "编辑 WebDAV" : "添加 WebDAV"}</strong>
          <label className="storage-panel__field">显示名<input className="form-control" aria-label="存储源显示名" value={draft.label} disabled={configSaving} onChange={(e) => setDraft({ ...draft, label: e.target.value })} placeholder="例如 团队文件" /></label>
          <label className="storage-panel__field">WebDAV 地址<input className="form-control" aria-label="WebDAV 地址" value={draft.baseUrl} disabled={configSaving} onChange={(e) => setDraft({ ...draft, baseUrl: e.target.value })} placeholder="https://dav.example.com/path" /></label>
          <label className="storage-panel__field">用户名（可选）<input className="form-control" aria-label="WebDAV 用户名" value={draft.username ?? ""} disabled={configSaving} onChange={(e) => setDraft({ ...draft, username: e.target.value })} autoComplete="off" /></label>
          <label className="storage-panel__field">密码或应用令牌（可选）<input className="form-control" aria-label="WebDAV 密码或应用令牌" type="password" value={draft.password ?? ""} disabled={configSaving} onChange={(e) => setDraft({ ...draft, password: e.target.value })} autoComplete="new-password" /></label>
          <label className="storage-panel__enabled"><input type="checkbox" checked={draft.enabled} disabled={configSaving} onChange={(e) => setDraft({ ...draft, enabled: e.target.checked })} /> 启用此存储源</label>
          <div className="storage-panel__config-actions">
            <button type="button" className="form-button" onClick={() => confirmConfigChange(() => setShowConfig(false))} disabled={configSaving}>取消</button>
            <button type="button" className="form-button form-button--primary" onClick={() => void saveConfig()} disabled={configSaving || !draft.baseUrl.trim()}>
              {configSaving ? "保存中…" : "保存"}
            </button>
          </div>
        </div>
      )}

      {/* 文件列表 */}
      {!configLoading && !configError && !selectedProvider && !showConfig && (
        <div className="storage-panel__empty">{providerOptions.length ? "选择存储源以管理连接，或展开文件浏览。" : "连接 WebDAV 存储源，即可浏览、上传和下载云端文件。"}</div>
      )}
      {selectedProvider && !selectedEnabled && (
        <div className="storage-panel__empty">此存储源已禁用，可编辑配置重新启用或移除配置。</div>
      )}
      {selectedEnabled && <SettingsDisclosure title="浏览云端文件" description="查看目录，上传、下载或编辑文件" defaultOpen={Boolean(preview || browseError)}>
      {/* 面包屑 */}
      {selectedProvider && selectedEnabled && (
        <div className="storage-panel__breadcrumb">
          {currentPath !== "/" && (
            <button type="button" onClick={goUp} className="form-button form-button--compact storage-panel__up"><ArrowUp size={14} />上级</button>
          )}
          <span className="storage-panel__path">{currentPath}</span>
          <button type="button" className="form-button form-button--compact" onClick={() => void createFolder()}>新建文件夹</button>
          <button type="button" className="form-button form-button--compact" onClick={() => void createTextFile()}>新建文本</button>
          <button type="button" className="form-button form-button--compact" onClick={() => void uploadFiles()} disabled={!!busyAction}>
            {busyAction === "上传中…" ? busyAction : "上传文件"}
          </button>
        </div>
      )}

      {browseError && selectedProvider && selectedEnabled && (
        <div className="panel-inline-error" role="alert">
          <span>目录加载失败：{browseError}</span>
          <button type="button" className="form-button" onClick={() => void browse(selectedProvider, currentPath)} disabled={loading}>
            {loading ? "重试中…" : "重试"}
          </button>
        </div>
      )}

      {selectedEnabled && loading ? (
        <div className="storage-panel__loading">加载中…</div>
      ) : selectedEnabled && entries.length > 0 ? (
        <ul className="storage-panel__list">
          {entries.map((e) => (
            <li key={e.path} className={"storage-panel__entry" + (e.isDir ? " dir" : "") + (preview?.providerId === selectedProvider && preview.path === e.path ? " storage-panel__entry--selected" : "")}>
              <span className="storage-panel__entry-icon">{e.isDir ? <Folder size={18} /> : <FileText size={18} />}</span>
              {e.isDir ? (
                <button
                  type="button"
                  className="storage-panel__entry-name storage-panel__entry-name--button"
                  onClick={() => openEntry(e)}
                  title="打开目录"
                >
                  {e.name}
                </button>
              ) : (
                <span className="storage-panel__entry-name">{e.name}</span>
              )}
              {!e.isDir && e.size != null && (
                <span className="storage-panel__entry-size">{formatSize(e.size)}</span>
              )}
              <div className="storage-panel__entry-actions">
                {!e.isDir && (
                  <button type="button" className="form-button form-button--compact form-button--ghost" onClick={() => void readFile(e)} title={readingPath === e.path ? "读取中…" : "读取"} aria-label={readingPath === e.path ? `正在读取 ${e.name}` : `读取 ${e.name}`} disabled={readingPath === e.path}>{readingPath === e.path ? "读取中…" : <Eye size={16} />}</button>
                )}
                {!e.isDir && (
                  <button type="button" className="form-button form-button--compact form-button--ghost form-button--icon-only" onClick={() => void downloadFile(e)} title="下载" aria-label={`下载 ${e.name}`} disabled={!!busyAction}><Download size={16} /></button>
                )}
                <button type="button" onClick={() => void deleteEntry(e)} title="删除" aria-label={`删除 ${e.name}`} className="form-button form-button--compact form-button--ghost form-button--danger form-button--icon-only"><Trash2 size={16} /></button>
              </div>
            </li>
          ))}
        </ul>
      ) : selectedEnabled && !loading && !browseError ? (
        <div className="storage-panel__empty">空目录</div>
      ) : null}
      {preview && selectedEnabled && (
        <div ref={previewRef} className="storage-panel__preview" role="region" aria-label={`文件预览：${preview.name}`} tabIndex={-1}>
          <div>
            <strong>{preview.name}</strong>
            <span>
              <button type="button" className="form-button form-button--primary" onClick={() => void savePreview()} disabled={!!busyAction || !previewDirty}>{busyAction === "保存中…" ? busyAction : previewDirty ? "保存" : "已保存"}</button>
              <button type="button" className="form-button" onClick={() => leavePreview(() => {})}>关闭</button>
            </span>
          </div>
          <textarea className="form-control" value={preview.content} onChange={(event) => setPreview({ ...preview, content: event.target.value })} aria-label={`编辑 ${preview.name}`} />
        </div>
      )}
      </SettingsDisclosure>}
      {dialog}
    </div>
  );
}

function localBasename(path: string): string {
  return path.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || "upload";
}

function validateStorageName(value: string): string | null {
  const name = value.trim();
  if (!name) return "名称不能为空。";
  if (name === "." || name === ".." || /[\\/\u0000-\u001f\u007f]/.test(name)) {
    return "名称不能是 . 或 ..，也不能包含路径分隔符或控制字符。";
  }
  return null;
}

function formatSize(bytes: number): string {
  if (bytes >= 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)}MB`;
  if (bytes >= 1024) return `${(bytes / 1024).toFixed(1)}KB`;
  return `${bytes}B`;
}
