import { useEffect, useState } from "react";
import { codingApi } from "../lib/tauri-api";
import { useCodingActionScope } from "../hooks/useCodingActionScope";

export function ExternalToolRecovery({root, onRecovered}: {root:string;onRecovered:()=>void}) {
  const [tools,setTools]=useState<Awaited<ReturnType<typeof codingApi.externalToolsPending>>>([]);
  const [notes,setNotes]=useState<Record<string,string>>({});
  const [confirmed,setConfirmed]=useState<Record<string,boolean>>({});
  const [busy,setBusy]=useState("");
  const [error,setError]=useState("");
  const [statusError,setStatusError]=useState("");
  const [reload,setReload]=useState(0);
  const isCurrent=useCodingActionScope(root);
  useEffect(()=>{
    let disposed=false;
    setTools([]);setNotes({});setConfirmed({});setError("");setStatusError("");setBusy("");
    const refresh=()=>Promise.resolve().then(()=>codingApi.externalToolsPending(root)).then(value=>{
      if(!Array.isArray(value))throw new Error("外部工具状态响应无效");
      if(!disposed){setTools(value);setStatusError("");}
    }).catch(error=>{if(!disposed)setStatusError(`停止状态读取失败：${String(error)}`);});
    void refresh();
    const timer=setInterval(()=>void refresh(),3000);
    return()=>{disposed=true;clearInterval(timer);};
  },[root,reload]);
  if(!tools.length&&!error&&!statusError)return null;
  return <section className="coding-panel coding-external-recovery" aria-label="外部工具恢复">
    <strong>核验外部工具后继续</strong>
    <p>请先到对应工具确认调用已经结束或停止，并检查已经发生的修改。恢复会保留现有内容和核验记录，不会重放调用。</p>
    {tools.map(tool=><fieldset key={tool.operationId} disabled={!!busy}>
      <legend>{tool.toolName} · {tool.runId.slice(0,8)}</legend>
      {tool.localInflight ? <p role="status">本地调用仍在等待结果；此时不能提前解除保护。</p> : <>
        <label>核验依据<textarea aria-label={`核验依据 ${tool.operationId}`} value={notes[tool.operationId]??""} maxLength={2000}
          onChange={event=>setNotes(current=>({...current,[tool.operationId]:event.target.value}))} /></label>
        <label><input type="checkbox" checked={confirmed[tool.operationId]??false}
          onChange={event=>setConfirmed(current=>({...current,[tool.operationId]:event.target.checked}))} />我已确认该调用结束或停止，并核对现有修改</label>
        <button type="button" disabled={!confirmed[tool.operationId]||(notes[tool.operationId]?.trim().length??0)<8}
          onClick={async()=>{
            setBusy(tool.operationId);setError("");
            try{await codingApi.recoverExternalTool(root,tool.operationId,notes[tool.operationId]);if(isCurrent()){setReload(value=>value+1);onRecovered();}}
            catch(error){if(isCurrent())setError(String(error));}
            finally{if(isCurrent())setBusy("");}
          }}>记录核验并恢复工程</button>
      </>}
    </fieldset>)}
    {error&&<p role="alert">{error}</p>}
    {statusError&&<p role="alert">{statusError}</p>}
  </section>;
}
