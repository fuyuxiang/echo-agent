import { isQuotaBlocking, loadQuotaConfig, loadUsage, recordUsage } from "@/lib/usage-quota";
import { forwardRef, useCallback, useEffect, useImperativeHandle, useLayoutEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Code2, LoaderCircle, RotateCw } from "lucide-react";
import { useAppDialog } from "@/components/AppDialog";
import { registerWorkspaceEditor, type DocumentSnapshot, type EditOperation, type ApplyResult, type EditorContextSnapshot } from "./lib/workspace-editor";
import { idePath, relativeToWorkspace } from "./lib/windows-path";

export interface TheiaMutationTicket {
  taskId: string | null;
  closeRound: boolean;
}

export interface TheiaIdeFrameHandle {
  getDirtyCount: () => Promise<number>;
  saveAll: () => Promise<boolean>;
  getDocument: (path?: string, full?: boolean) => Promise<DocumentSnapshot>;
  getContext: () => Promise<EditorContextSnapshot>;
  applyEdits: (operation: EditOperation) => Promise<ApplyResult>;
  openDiff: (path: string, original: string, title?: string) => Promise<void>;
}

interface TheiaIdeFrameProps {
  root: string;
  modelId?: string;
  completionEnabled?: boolean;
  onCompletionStatus?: (status: string) => void;
  onBeforeMutation: (operation: string, paths: string[]) => Promise<TheiaMutationTicket | null>;
  onAfterMutation: (ticket: TheiaMutationTicket, success: boolean, error?: string) => Promise<void>;
  onActiveFile?: (path: string | null) => void;
  onActiveSymbol?: (value: { path: string; symbol: string } | null) => void;
  onPreviewUrl?: (url: string) => void;
  onToast?: (message: string) => void;
  previewRequest?: { url: string; id: number } | null;
  openFileRequest?: { path: string; id: number; line?: number } | null;
  onDirtyChange?: (count: number | null) => void;
}

interface TheiaEndpoint {
  url: string;
  embedToken: string;
}

interface ActiveTheiaEndpoint extends TheiaEndpoint {
  root: string;
}

function belongsToWorkspace(root: string, path: string): boolean {
  return relativeToWorkspace(root, path) !== null;
}

/** Theia owns the IDE surface; EchoAgent owns the surrounding task UI. */
export const TheiaIdeFrame = forwardRef<TheiaIdeFrameHandle, TheiaIdeFrameProps>(function TheiaIdeFrame({
  root,
  modelId,
  completionEnabled = true,
  onCompletionStatus,
  onBeforeMutation,
  onAfterMutation,
  onActiveFile,
  onActiveSymbol,
  onPreviewUrl,
  onToast,
  previewRequest,
  openFileRequest,
  onDirtyChange,
}, ref) {
  const frameRef = useRef<HTMLIFrameElement>(null);
  const instanceId = useRef<string | null>(null);
  const capabilities = useRef(new Set<string>());
  const [canonicalRoot,setCanonicalRoot] = useState(root);
  const editorLeaseReady = useRef(false);
  const protocolRequests = useRef(new Map<string, { resolve: (value: unknown) => void; reject: (error: Error) => void; timer: ReturnType<typeof setTimeout> }>());
  const completionCalls = useRef(new Set<string>());
  const completionConfig = useRef({modelId, completionEnabled, onCompletionStatus});
  completionConfig.current = {modelId, completionEnabled, onCompletionStatus};
  const pendingRequests = useRef(new Map<string, { resolve: (count: number) => void; reject: (reason: Error) => void; timer: ReturnType<typeof setTimeout> }>());
  const lastKnownDirtyCount = useRef<number | null>(null);
  const hasBeenReady = useRef(false);
  const resettingWorkspace = useRef(false);
  const token = useMemo(() => crypto.randomUUID(), [root]);
  const [endpoint, setEndpoint] = useState<ActiveTheiaEndpoint | null>(null);
  const activeEndpoint = endpoint?.root === root ? endpoint : null;
  const baseUrl = activeEndpoint?.url ?? null;
  const [status, setStatus] = useState<"starting" | "loading" | "ready" | "error">("starting");
  const [error, setError] = useState("");
  const [attempt, setAttempt] = useState(0);
  const [reloadKey, setReloadKey] = useState(0);
  const [connectionWarning, setConnectionWarning] = useState("");
  const { requestConfirmation, dialog } = useAppDialog(`${root}:${connectionWarning}`);
  const [theme, setTheme] = useState(() => document.documentElement.getAttribute("data-theme") === "dark" ? "dark" : "light");
  const bridgeCallbacks = useRef({ onActiveFile, onActiveSymbol, onPreviewUrl, onAfterMutation,
    onBeforeMutation, onDirtyChange, onToast });
  useLayoutEffect(() => {
    bridgeCallbacks.current = { onActiveFile, onActiveSymbol, onPreviewUrl, onAfterMutation,
      onBeforeMutation, onDirtyChange, onToast };
  }, [onActiveFile, onActiveSymbol, onPreviewUrl, onAfterMutation, onBeforeMutation, onDirtyChange, onToast]);

  const reconnect = () => {
    const restart = () => setAttempt((value) => value + 1);
    if (!hasBeenReady.current || lastKnownDirtyCount.current === 0) {
      restart();
      return;
    }
    const detail = lastKnownDirtyCount.current === null
      ? "无法确认是否有未保存文件"
      : `当前有 ${lastKnownDirtyCount.current} 个未保存文件`;
    requestConfirmation({
      title: "保留编辑内容后重新连接？",
      description: `${detail}。重新连接会丢失当前编辑器中未保存的内容。请取消并复制需要保留的内容；确认已保留后再继续。`,
      cancelLabel: "保留编辑内容",
      confirmLabel: "已保留内容，重新连接",
      danger: true,
      action: restart,
    });
  };

  const requestCount = useCallback((type: "echo/save-all" | "echo/get-dirty" | "echo/reload-clean", requireAck = false) => {
    if (!requireAck && type === "echo/get-dirty" && status !== "ready") {
      if (lastKnownDirtyCount.current !== null) return Promise.resolve(lastKnownDirtyCount.current);
      if (!hasBeenReady.current) return Promise.resolve(0);
      return Promise.reject(new Error("IDE 已断开，无法确认未保存文件"));
    }
    if (status !== "ready" || !baseUrl || !frameRef.current?.contentWindow) {
      return Promise.reject(new Error("IDE 尚未加载完成"));
    }
    const id = crypto.randomUUID();
    return new Promise<number>((resolve, reject) => {
      const timer = setTimeout(() => {
        pendingRequests.current.delete(id);
        reject(new Error("等待 IDE 响应超时"));
      }, 20_000);
      pendingRequests.current.set(id, { resolve, reject, timer });
      frameRef.current?.contentWindow?.postMessage({ type, token, id }, new URL(baseUrl).origin);
    });
  }, [baseUrl, status, token]);

  const requestProtocol = useCallback(<T,>(type: string, payload: Record<string, unknown> = {}): Promise<T> => {
    if (status !== "ready" || !baseUrl || !instanceId.current || !frameRef.current?.contentWindow) {
      return Promise.reject(new Error("IDE 版本化协议尚未连接"));
    }
    const capability = type === "echo/open-diff" ? "native-diff"
      : type === "echo/get-document" || type === "echo/get-context" ? "document-snapshots" : "versioned-edits";
    if (!capabilities.current.has(capability)) return Promise.reject(new Error(`当前 IDE 缺少 ${capability} 能力，请重新构建并连接`));
    const id = crypto.randomUUID();
    return new Promise<T>((resolve, reject) => {
      const timer = setTimeout(() => { protocolRequests.current.delete(id); reject(new Error("等待 IDE 文档响应超时")); }, 20_000);
      protocolRequests.current.set(id, {resolve: value => resolve(value as T), reject, timer});
      frameRef.current!.contentWindow!.postMessage({type, token, id, instanceId:instanceId.current, ...payload}, new URL(baseUrl).origin);
    });
  }, [status, baseUrl, token]);
  const getDocument = useCallback((path?: string, full = false, range?: {offset:number;limit:number}) => requestProtocol<DocumentSnapshot>("echo/get-document", {path, full, range}), [requestProtocol]);
  const getContext = useCallback(() => requestProtocol<EditorContextSnapshot>("echo/get-context"), [requestProtocol]);
  const applyEdits = useCallback((operation: EditOperation) => requestProtocol<ApplyResult>("echo/apply-edits", {operation}), [requestProtocol]);
  const openDiff = useCallback((path: string, original: string, title?: string) => {
    if (!belongsToWorkspace(root, path)) return Promise.reject(new Error("Diff 文档不属于当前工程"));
    return requestProtocol<void>("echo/open-diff", {path, original, title});
  }, [root, requestProtocol]);

  useLayoutEffect(() => registerWorkspaceEditor(root, {
    workspace:canonicalRoot,
    getDirtyCount: () => requestCount("echo/get-dirty", true),
    saveAll: async () => (await requestCount("echo/save-all", true)) === 0,
    getDocument, getContext, applyEdits,
    cancelRun: runId => requestProtocol<boolean>("echo/cancel-run",{runId}),
    refreshFromDisk: async () => { await requestCount("echo/reload-clean", true); },
    lockEditing: (locked) => {
      if (frameRef.current) {
        if (locked) frameRef.current.blur();
        frameRef.current.inert = locked;
      }
    },
  }), [root, canonicalRoot, requestCount, getDocument, getContext, applyEdits, requestProtocol]);

  useImperativeHandle(ref, () => ({
    getDocument, getContext, applyEdits, openDiff,
    getDirtyCount: () => requestCount("echo/get-dirty"),
    saveAll: async () => {
      const count = await requestCount("echo/save-all");
      return count === 0;
    },
  }), [requestCount, getDocument, getContext, applyEdits, openDiff]);

  useEffect(() => () => {
    for (const request of pendingRequests.current.values()) {
      clearTimeout(request.timer);
      request.reject(new Error("IDE 已关闭"));
    }
    pendingRequests.current.clear();
    for (const request of protocolRequests.current.values()) { clearTimeout(request.timer); request.reject(new Error("IDE 连接已失效")); }
    protocolRequests.current.clear();
    for (const requestId of completionCalls.current) void invoke("coding_inline_cancel", {requestId}).catch(() => undefined);
    completionCalls.current.clear();
  }, []);

  useEffect(() => {
    instanceId.current = null; capabilities.current.clear();
    editorLeaseReady.current = false;
    lastKnownDirtyCount.current = null;
    hasBeenReady.current = false;
  }, [root]);

  useEffect(() => {
    for (const request of pendingRequests.current.values()) {
      clearTimeout(request.timer);
      request.reject(new Error("IDE 已重新加载，请重试"));
    }
    pendingRequests.current.clear();
    for (const request of protocolRequests.current.values()) { clearTimeout(request.timer); request.reject(new Error("IDE 连接已失效")); }
    protocolRequests.current.clear();
    for (const requestId of completionCalls.current) void invoke("coding_inline_cancel", {requestId}).catch(() => undefined);
    completionCalls.current.clear();
  }, [reloadKey, root]);

  useEffect(() => {
    const observer = new MutationObserver(() => {
      setTheme(document.documentElement.getAttribute("data-theme") === "dark" ? "dark" : "light");
    });
    observer.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    if (status !== "ready" || !baseUrl) return;
    frameRef.current?.contentWindow?.postMessage({ type: "echo/set-theme", token, theme }, new URL(baseUrl).origin);
  }, [baseUrl, status, theme, token]);

  useEffect(() => {
    let cancelled = false;
    setStatus("starting");
    setEndpoint(null);
    setConnectionWarning("");
    onDirtyChange?.(null);
    invoke<TheiaEndpoint>("coding_theia_start", { root })
      .then((next) => {
        if (cancelled) return;
        setEndpoint({ ...next, root });
        setStatus("loading");
      })
      .catch((reason) => {
        if (cancelled) return;
        setError(String(reason));
        setStatus("error");
      });
    return () => { cancelled = true; };
  }, [root, attempt]);

  useEffect(() => {
    if (status !== "loading") return;
    const timer = window.setTimeout(() => {
      setError("IDE 页面加载超时。请重试；若持续失败，请查看应用数据目录中的 theia.log。");
      setStatus("error");
    }, 60_000);
    return () => window.clearTimeout(timer);
  }, [status]);

  const frameUrl = useMemo(() => {
    if (!activeEndpoint) return null;
    const url = new URL(activeEndpoint.url);
    url.hash = encodeURI(idePath(root));
    return url.toString();
  }, [activeEndpoint, root]);
  const frameName = useMemo(() => activeEndpoint ? `echo-embed:${JSON.stringify({
    embedToken: activeEndpoint.embedToken,
    bridgeToken: token,
    parentOrigin: window.location.origin,
  })}` : undefined, [activeEndpoint, token]);

  useEffect(() => {
    editorLeaseReady.current=false;
    const epoch=`${token}:${attempt}`;let disposed=false;let attachedRoot:string|undefined;
    void invoke<string>("coding_document_editor_attach",{root,instanceId:epoch}).then(value=>{
      if(typeof value!=="string") throw new Error("编辑器工作区注册响应无效");
      attachedRoot=value;
      if(disposed) void invoke("coding_document_editor_detach",{root:value,instanceId:epoch});
      else {setCanonicalRoot(value);editorLeaseReady.current=true;if(hasBeenReady.current)setStatus("ready");}
    }).catch(reason=>{if(!disposed){setError(`编辑器协调不可用：${String(reason)}`);setStatus("error");}});
    return()=>{disposed=true;if(attachedRoot)void invoke("coding_document_editor_detach",{root:attachedRoot,instanceId:epoch}).catch(()=>undefined);};
  },[root,token,attempt]);

  useEffect(() => {
    if (status !== "ready" || !baseUrl) return;
    frameRef.current?.contentWindow?.postMessage({type:"echo/set-inline-completion",token,instanceId:instanceId.current,enabled:completionEnabled && !!modelId}, new URL(baseUrl).origin);
  }, [status, baseUrl, token, completionEnabled, modelId]);

  useEffect(() => {
    if (status !== "ready" || !baseUrl || !previewRequest) return;
    frameRef.current?.contentWindow?.postMessage({
      type: "echo/open-preview", token, url: previewRequest.url,
    }, new URL(baseUrl).origin);
  }, [baseUrl, previewRequest, status, token]);

  useEffect(() => {
    if (status !== "ready" || !baseUrl || !openFileRequest) return;
    frameRef.current?.contentWindow?.postMessage({
      type: "echo/open-file", token, path: openFileRequest.path, line: openFileRequest.line,
    }, new URL(baseUrl).origin);
  }, [baseUrl, openFileRequest, status, token]);

  useLayoutEffect(() => {
    if (!baseUrl) return;
    const theiaOrigin = new URL(baseUrl).origin;
    let offline = false;
    let disposed = false;
    let recoveryFailed = false;
    let recoveryTimer: number | undefined;
    const clearRecovery = () => {
      if (recoveryTimer !== undefined) window.clearTimeout(recoveryTimer);
      recoveryTimer = undefined;
    };
    const reloadFrame = () => {
      if (lastKnownDirtyCount.current !== 0) {
        const detail = lastKnownDirtyCount.current === null
          ? "无法确认是否有未保存文件"
          : `当前有 ${lastKnownDirtyCount.current} 个未保存文件`;
        setConnectionWarning(`IDE 连接已断开，${detail}。请先复制需要保留的内容，再重新连接。`);
        return;
      }
      bridgeCallbacks.current.onDirtyChange?.(null);
      setStatus("loading");
      setReloadKey((value) => value + 1);
    };
    const onMessage = (event: MessageEvent) => {
      if (event.origin !== theiaOrigin || event.source !== frameRef.current?.contentWindow
          || typeof event.data !== "object" || !event.data) return;
      const message = event.data as Record<string, unknown>;
      if (message.token !== token || typeof message.type !== "string") return;
      const { onActiveFile, onActiveSymbol, onPreviewUrl, onAfterMutation,
        onBeforeMutation, onDirtyChange, onToast } = bridgeCallbacks.current;
      if (message.type !== "echo/ready" && instanceId.current && message.instanceId !== instanceId.current) return;
      if (message.type === "echo/ready") {
        if (typeof message.instanceId === "string") {
          if (instanceId.current && instanceId.current !== message.instanceId) {
            for (const request of protocolRequests.current.values()) { clearTimeout(request.timer); request.reject(new Error("IDE 实例已更换")); }
            protocolRequests.current.clear();
          }
          instanceId.current = message.instanceId;
          capabilities.current = new Set(Array.isArray(message.capabilities) ? message.capabilities.filter((value):value is string => typeof value === "string") : []);
        }
        offline = false;
        clearRecovery();
        setConnectionWarning("");
        resettingWorkspace.current = false;
        hasBeenReady.current = true;
        if(editorLeaseReady.current) setStatus("ready");
        return;
      }
      if (["echo/inline-completion-shown","echo/inline-completion-accepted","echo/inline-completion-discarded","echo/inline-completion-error"].includes(message.type)) {
        if (message.type === "echo/inline-completion-error") completionConfig.current.onCompletionStatus?.(`Tab 暂不可用：${String(message.error ?? "请求超时").slice(0,180)}`);
        if (typeof message.requestId === "string") void invoke("coding_inline_observe",{event:{kind:message.type.replace("echo/inline-completion-",""),requestId:message.requestId,
          uri:message.uri,documentVersion:message.version,elapsedMs:message.latencyMs,characters:message.characters,reason:message.reason}}).catch(() => undefined);
        return;
      }
      if (message.type === "echo/editor-capabilities") {
        capabilities.current = new Set(Array.isArray(message.capabilities) ? message.capabilities.filter((value):value is string => typeof value === "string") : []); return;
      }
      if (message.type === "echo/cancel-inline-completion" && typeof message.requestId === "string") {
        completionCalls.current.delete(message.requestId);
        void invoke("coding_inline_cancel", {requestId:message.requestId}).catch(() => undefined);
        return;
      }
      if (message.type === "echo/inline-completion" && typeof message.id === "string") {
        const config = completionConfig.current;
        const requestId = message.requestId;
        const target = event.source as Window;
        const currentInstance = instanceId.current;
        const respond = (ok: boolean, value?: unknown, error?: string) => {
          if (disposed || currentInstance !== instanceId.current) return;
          target.postMessage({type:"echo/response",token,instanceId:currentInstance,id:message.id,ok,value,error},theiaOrigin);
        };
        if (!config.completionEnabled || !config.modelId) { respond(true,{text:""}); return; }
        if (typeof requestId !== "string" || message.workspace !== idePath(root) || typeof message.path !== "string"
          || !belongsToWorkspace(root,message.path) || typeof message.prefix !== "string" || typeof message.suffix !== "string") {
          respond(false,undefined,"补全上下文不属于当前工程"); return;
        }
        if (isQuotaBlocking(loadUsage(),loadQuotaConfig())) { config.onCompletionStatus?.("Tab 已达到本地用量上限"); respond(false,undefined,"用量已达到上限"); return; }
        completionCalls.current.add(requestId);
        void invoke<{text:string;requestId:string;documentVersion:number;modelId:string;elapsedMs:number;inputTokens:number;outputTokens:number;cached:boolean}>("coding_inline_complete", {request:{
          requestId,workspace:root,uri:message.uri,documentVersion:message.version,prefix:message.prefix,suffix:message.suffix,language:message.language,modelId:config.modelId,
        }}).then(result => {
          // Account for completed model work even if the editor has since moved.
          if (!result.cached) recordUsage(loadUsage(), {modelId:result.modelId,promptTokens:result.inputTokens,completionTokens:result.outputTokens,
            sessionId:"inline-completion",promptId:result.requestId},loadQuotaConfig() ?? undefined);
          if (!completionCalls.current.has(requestId)) return;
          config.onCompletionStatus?.(`Tab ${result.elapsedMs}ms${result.cached ? " · 缓存" : ""}`);
          respond(true,{text:result.text});
        }).catch(error => {
          if (!completionCalls.current.has(requestId)) return;
          config.onCompletionStatus?.(`Tab 暂不可用：${String(error).replace(/^Error:\s*/, "").slice(0,180)}`);
          respond(false,undefined,String(error));
        }).finally(() => completionCalls.current.delete(requestId));
        return;
      }
      if (message.type === "echo/backend-connection") {
        if (message.online === true) {
          offline = false;
          clearRecovery();
          setConnectionWarning("");
          if (recoveryFailed) {
            recoveryFailed = false;
            setStatus("ready");
          }
        } else if (message.online === false && !offline) {
          offline = true;
          recoveryTimer = window.setTimeout(() => {
            recoveryTimer = undefined;
            if (disposed || !offline) return;
            if (lastKnownDirtyCount.current !== 0) {
              reloadFrame();
              return;
            }
            void invoke<TheiaEndpoint>("coding_theia_start", { root }).then((next) => {
              if (disposed || !offline) return;
              // The editor may have become dirty while the native restart
              // was pending. Recheck before changing the iframe URL/token.
              if (lastKnownDirtyCount.current !== 0) {
                reloadFrame();
                return;
              }
              if (next.url !== activeEndpoint?.url || next.embedToken !== activeEndpoint?.embedToken) {
                setEndpoint({ ...next, root });
                reloadFrame();
              } else {
                // The native server recovered, but Theia's websocket may still
                // be stuck. Give its own reconnect a chance before reloading.
                recoveryTimer = window.setTimeout(() => {
                  recoveryTimer = undefined;
                  if (!disposed && offline) reloadFrame();
                }, 6_000);
              }
            }).catch((reason) => {
              if (disposed || !offline) return;
              recoveryFailed = true;
              setError(`IDE 连接中断，自动恢复失败：${String(reason)}`);
              setStatus("error");
            });
          }, 1_500);
        }
        return;
      }
      if (message.type === "echo/dirty-state") {
        if (typeof message.count === "number" && Number.isSafeInteger(message.count) && message.count >= 0) {
          lastKnownDirtyCount.current = message.count;
          onDirtyChange?.(message.count);
        }
        return;
      }
      if (message.type === "echo/response" && typeof message.id === "string") {
        const protocol = protocolRequests.current.get(message.id);
        if (protocol) {
          protocolRequests.current.delete(message.id); clearTimeout(protocol.timer);
          if (message.ok === true) protocol.resolve(message.value);
          else protocol.reject(new Error(typeof message.error === "string" ? message.error : "IDE 文档操作失败"));
          return;
        }
        const request = pendingRequests.current.get(message.id);
        if (!request) return;
        pendingRequests.current.delete(message.id);
        clearTimeout(request.timer);
        if (message.ok === true && typeof message.value === "number" && Number.isSafeInteger(message.value) && message.value >= 0) {
          lastKnownDirtyCount.current = message.value;
          onDirtyChange?.(message.value);
          request.resolve(message.value);
        }
        else request.reject(new Error(typeof message.error === "string" ? message.error : "IDE 保存失败"));
        return;
      }
      if (message.type === "echo/workspace") {
        if (typeof message.path !== "string" || !belongsToWorkspace(root, message.path)
            || !belongsToWorkspace(message.path, root)) {
          if (!resettingWorkspace.current) {
            resettingWorkspace.current = true;
            onToast?.("请使用顶部项目切换器打开代码库，IDE 已返回当前项目");
            onDirtyChange?.(null);
            setStatus("loading");
            setReloadKey((value) => value + 1);
          }
        }
        return;
      }
      if (message.type === "echo/active-file") {
        onActiveFile?.(typeof message.path === "string" && belongsToWorkspace(root, message.path) ? message.path : null);
        return;
      }
      if (message.type === "echo/active-symbol") {
        onActiveSymbol?.(
          typeof message.path === "string"
          && belongsToWorkspace(root, message.path)
          && typeof message.symbol === "string"
          && /^[A-Za-z_$][A-Za-z0-9_$]{0,127}$/.test(message.symbol)
            ? { path: message.path, symbol: message.symbol }
            : null,
        );
        return;
      }
      if (message.type === "echo/preview-url") {
        if (typeof message.url !== "string") return;
        try {
          const url = new URL(message.url);
          if ((url.protocol === "http:" || url.protocol === "https:")
              && ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname)) {
            onPreviewUrl?.(url.toString());
          }
        } catch {
          // Ignore terminal text that is not a valid local preview URL.
        }
        return;
      }
      if (typeof message.id !== "string" || !event.source) return;
      const target = event.source;
      const respond = (ok: boolean, value?: unknown, reason?: unknown) => {
        (target as Window).postMessage({
          type: "echo/response", token, instanceId:instanceId.current, id: message.id, ok, value,
          error: reason === undefined ? undefined : String(reason),
        }, theiaOrigin);
      };
      if (message.type === "echo/before-mutation") {
        const paths = message.paths;
        if (!Array.isArray(paths) || paths.length === 0 || !paths.every(
          (path): path is string => typeof path === "string" && belongsToWorkspace(root, path),
        )) {
          respond(false, undefined, "只能修改当前项目内的文件；跨项目移动请在项目外单独处理");
          return;
        }
        void onBeforeMutation(String(message.operation ?? "write"), paths)
          .then((ticket) => ticket ? respond(true, ticket) : respond(false, undefined, "当前任务阶段不能修改文件"))
          .catch((reason) => respond(false, undefined, reason));
      } else if (message.type === "echo/after-mutation") {
        const ticket = message.ticket as TheiaMutationTicket | undefined;
        if (!ticket || typeof ticket.closeRound !== "boolean") {
          respond(false, undefined, "无效的文件操作凭据");
          return;
        }
        void onAfterMutation(ticket, message.success === true, typeof message.error === "string" ? message.error : undefined)
          .then(() => respond(true))
          .catch((reason) => {
            onToast?.(`文件已修改，但任务同步失败：${String(reason)}`);
            respond(false, undefined, reason);
          });
      }
    };
    window.addEventListener("message", onMessage);
    return () => {
      disposed = true;
      clearRecovery();
      window.removeEventListener("message", onMessage);
    };
  }, [activeEndpoint, baseUrl, root, token]);

  return (
    <div className="echo-theia" aria-label="Theia 代码工作台">
      {frameUrl && (
        <iframe
          ref={frameRef}
          key={`${frameUrl}:${reloadKey}`}
          className="echo-theia__frame"
          src={frameUrl}
          name={frameName}
          title="Echo Code IDE"
          referrerPolicy="no-referrer"
          onError={() => {
            setError("Theia 页面无法加载");
            setStatus("error");
          }}
        />
      )}
      {status !== "ready" && (
        <div className="echo-theia__cover" role="status">
          <Code2 size={26} />
          {status === "error" ? (
            <>
              <strong>IDE 启动失败</strong>
              <p>{error.startsWith("Theia 启动后退出：")
                ? "本地 IDE 进程启动后退出。请重试；若仍失败，请将下方日志位置提供给技术支持。"
                : error}</p>
              {error.startsWith("Theia 启动后退出：") && (
                <details className="echo-theia__diagnostics">
                  <summary>查看错误和日志位置</summary>
                  <pre>{error}</pre>
                </details>
              )}
              <button type="button" onClick={reconnect}>
                <RotateCw size={14} /> 重试
              </button>
            </>
          ) : (
            <>
              <strong>正在启动 Echo Code IDE</strong>
              <p>{status === "starting" ? "准备本地 Theia 服务…" : "正在加载项目、编辑器与终端…"}</p>
              <LoaderCircle size={17} className="is-spinning" />
            </>
          )}
        </div>
      )}
      {status === "ready" && connectionWarning && (
        <div className="echo-theia__connection-warning" role="alert">
          <span>{connectionWarning}</span>
          <button type="button" onClick={reconnect}>重新连接</button>
        </div>
      )}
      {dialog}
    </div>
  );
});
