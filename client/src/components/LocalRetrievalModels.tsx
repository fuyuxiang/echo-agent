import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { filesystemPickDirectory, openExternalUrl } from "@/lib/agent-client";
import { isTauriAvailable } from "@/lib/tauri-kb-reader";
import modelAssets from "../../vendor/echo-agent-build/crates/codegen/echo-agent-memory/src/model-assets.json";
import "@/styles/local-retrieval.css";

interface ModelStatus {
  state: "missing" | "downloading" | "verifying" | "ready" | "error" | "unsupported";
  source?: "bundled" | "repaired" | "missing";
  message?: string | null;
  operationError?: string | null;
  downloadedBytes: number;
  totalBytes: number;
  embeddingReady: boolean;
  rerankerReady: boolean;
}
function valid(value: unknown): value is ModelStatus {
  return !!value && typeof value === "object"
    && ["missing","downloading","verifying","ready","error","unsupported"].includes((value as ModelStatus).state)
    && Number.isFinite((value as ModelStatus).totalBytes)
    && Number.isFinite((value as ModelStatus).downloadedBytes)
    && typeof (value as ModelStatus).embeddingReady === "boolean"
    && typeof (value as ModelStatus).rerankerReady === "boolean";
}

export function LocalRetrievalModels() {
  const alive = useRef(true);
  useEffect(() => { alive.current=true; return () => { alive.current=false; }; }, []);
  const [status, setStatus] = useState<ModelStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const preparing = status?.state === "downloading" || status?.state === "verifying";
  useEffect(() => {
    if (!isTauriAvailable()) return;
    let mounted = true;
    const refresh = () => void invoke<unknown>("retrieval_models_status").then(value => {
      if (mounted && valid(value)) setStatus(value);
    }).catch(() => {});
    refresh();
    const timer = window.setInterval(refresh, preparing ? 750 : 5_000);
    return () => { mounted = false; window.clearInterval(timer); };
  }, [preparing]);
  const act = async (command: string, args?: Record<string, unknown>) => {
    setBusy(true); setError(null);
    try {
      const value = await invoke<unknown>(command, args);
      if (alive.current && valid(value)) setStatus(value);
      else {
        const refreshed = await invoke<unknown>("retrieval_models_status");
        if (alive.current && valid(refreshed)) setStatus(refreshed);
      }
    } catch (error) { if (alive.current) setError(String(error).replace(/^Error:\s*/, "")); }
    finally { if (alive.current) setBusy(false); }
  };
  const importModels = async () => {
    setBusy(true); setError(null);
    try {
      const directory = await filesystemPickDirectory();
      if (directory) await act("retrieval_models_import", { directory });
    } catch (error) { if (alive.current) setError(String(error).replace(/^Error:\s*/, "")); }
    finally { if (alive.current) setBusy(false); }
  };
  if (!status) return null;
  const unsupported = status.state === "unsupported";
  const ready = status.state === "ready" && status.embeddingReady && status.rerankerReady;
  const progress = status.totalBytes > 0 ? Math.min(100, Math.floor(status.downloadedBytes / status.totalBytes * 100)) : 0;
  return <section className="local-retrieval" aria-label="本地语义检索">
    <div className="local-retrieval__description">
      <strong>{ready ? "本地语义检索已就绪" : unsupported ? "关键词检索可用" : preparing ? "正在检查与修复模型" : "本地模型需要修复"}</strong>
      <span>{ready
        ? `${status.source === "repaired" ? "使用已校验的本地模型。" : "客户端已内置模型，无需下载。"}向量生成与相关性排序在本机完成；用于回答的资料会交给当前对话模型。`
        : unsupported ? "当前电脑使用关键词搜索，知识库与记忆仍可使用。" : preparing ? "正在处理本地模型文件，关键词搜索仍可使用。" : "内置模型暂不可用，可修复或重新安装客户端。修复期间，关键词搜索仍可使用。"}</span>
    </div>
    {preparing && <div className="local-retrieval__progress" role="status">
      <progress aria-label="本地模型准备进度" value={status.downloadedBytes} max={status.totalBytes || 1} />
      <span>{status.state === "verifying" ? "正在校验并加载模型" : `正在下载修复文件 · ${progress}%`}</span>
    </div>}
    {(error || status.operationError || status.state === "error" || (!preparing && status.message)) && <p role={error || status.operationError || status.state === "error" ? "alert" : "status"}>
      {error || status.operationError || status.message}
      {ready && (error || status.operationError) && " 当前模型仍可使用。"}
    </p>}
    {!ready && !unsupported && <div className="local-retrieval__actions">
      {preparing
        ? <button type="button" disabled={busy} onClick={() => void act("retrieval_models_cancel")}>暂停修复</button>
        : <><button type="button" disabled={busy} onClick={() => void act("retrieval_models_prepare")}>
          {busy ? "正在处理…" : status.state === "error" ? "重试修复模型" : "修复本地模型"}
        </button><button type="button" disabled={busy} onClick={() => void importModels()}>导入离线模型</button></>}
    </div>}
    <details><summary>模型与隐私说明</summary>
      <p>客户端内置中文向量模型 BGE base zh v1.5、相关性模型 BGE reranker base，均使用 INT8 版本，约占 400MB 安装空间。首次检索会加载模型，空闲后自动释放。个人记忆自动回忆不运行重排；组织知识仍由组织服务检索。</p>
      <p>首次使用无需联网下载模型。仅修复缺失或损坏的模型时可能下载公开文件，不上传知识库；回答和自动整理记忆使用当前对话模型。</p>
    </details>
    {ready && <details><summary>模型维护</summary>
      <p>检查并校验现有模型，仅在文件缺失或损坏时下载修复。修复与离线导入使用用户数据目录，不改动客户端安装文件。</p>
      <div className="local-retrieval__actions"><button type="button" disabled={busy} onClick={() => void act("retrieval_models_prepare")}>检查并修复模型</button><button type="button" disabled={busy} onClick={() => void importModels()}>导入离线模型</button></div>
    </details>}
    {!ready && !preparing && !unsupported && <details><summary>离线导入说明</summary>
      <p>选择包含 embedding 和 reranker 两个目录的模型包，每个目录包含 model.onnx 与 tokenizer.json。按下方清单下载并重命名到对应路径，导入时会自动校验文件。</p>
      <ul>{modelAssets.assets.map(asset => <li key={asset.path}><a href={`https://huggingface.co/${asset.repo}/resolve/${asset.revision}/${asset.remote}?download=true`} onClick={event => {
        event.preventDefault(); void openExternalUrl(event.currentTarget.href).catch(error => { if (alive.current) setError(String(error)); });
      }}>{asset.path}</a></li>)}</ul>
    </details>}
  </section>;
}
