import { useEffect, useRef, useState } from "react";
import { codingApi } from "../lib/tauri-api";
import type { VerificationConfig, VerificationKind } from "../lib/types";

function parseEnvironment(text: string): Record<string, string> {
  const env: Record<string, string> = Object.create(null);
  for (const [index, raw] of text.split(/\r?\n/).entries()) {
    if (!raw.trim()) continue;
    const separator = raw.indexOf("=");
    const key = raw.slice(0, separator).trim();
    if (separator < 1 || !/^[A-Za-z_][A-Za-z0-9_]*$/.test(key))
      throw new Error(`环境变量第 ${index + 1} 行应为 KEY=value`);
    if (Object.prototype.hasOwnProperty.call(env, key)) throw new Error(`环境变量 ${key} 重复`);
    env[key] = raw.slice(separator + 1);
  }
  return env;
}

export function ProjectChecksEditor({
  root,
  onSaved,
}: {
  root: string;
  onSaved: () => void;
}) {
  const [config, setConfig] = useState<VerificationConfig>({ checks: [] });
  const [environmentDrafts, setEnvironmentDrafts] = useState<Record<string, string>>({});
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [busy, setBusy] = useState(false);
  const [ready, setReady] = useState(false);
  const [reload, setReload] = useState(0);
  const generation = useRef(0);
  useEffect(() => setNotice(""), [config, environmentDrafts]);
  useEffect(() => {
    generation.current += 1;
    let disposed = false;
    setReady(false);
    setBusy(false);
    setConfig({ checks: [] });
    setEnvironmentDrafts({});
    void codingApi
      .verificationConfig(root)
      .then((value) => {
        if (!value || !Array.isArray(value.checks))
          throw new Error("检查配置响应无效，请重试");
        if (!disposed) {
          setConfig(value);
          setEnvironmentDrafts(Object.fromEntries(value.checks.map(check => [check.id,
            Object.entries(check.env).map(([key, value]) => `${key}=${value}`).join("\n")])));
          setReady(true);
          setError("");
        }
      })
      .catch((error) => {
        if (!disposed) setError(String(error));
      });
    return () => {
      disposed = true;
      generation.current += 1;
    };
  }, [root, reload]);
  return (
    <details className="coding-check-config">
      <summary>项目检查配置</summary>
      <p>
        保存在应用数据中。自动运行默认关闭；环境覆盖仅接受非敏感值，密钥沿用现有安全配置。
      </p>
      {config.checks.map((check, index) => (
        <fieldset key={check.id} disabled={busy}>
          <legend>{check.label || `检查 ${index + 1}`}</legend>
          <label>
            名称
            <input
              value={check.label}
              onChange={(event) =>
                setConfig((current) => ({
                  ...current,
                  checks: current.checks.map((item, i) =>
                    i === index ? { ...item, label: event.target.value } : item,
                  ),
                }))
              }
            />
          </label>
          <label>
            类型
            <select
              value={check.kind}
              onChange={(event) =>
                setConfig((current) => ({
                  ...current,
                  checks: current.checks.map((item, i) =>
                    i === index
                      ? {
                          ...item,
                          kind: event.target.value as VerificationKind,
                        }
                      : item,
                  ),
                }))
              }
            >
              {["test", "build", "lint", "type_check", "custom"].map((kind) => (
                <option key={kind} value={kind}>
                  {kind}
                </option>
              ))}
            </select>
          </label>
          <label>
            命令
            <input
              value={check.command}
              onChange={(event) =>
                setConfig((current) => ({
                  ...current,
                  checks: current.checks.map((item, i) =>
                    i === index
                      ? { ...item, command: event.target.value }
                      : item,
                  ),
                }))
              }
            />
          </label>
          <label>
            相对工作目录
            <input
              value={check.cwd}
              placeholder="."
              onChange={(event) =>
                setConfig((current) => ({
                  ...current,
                  checks: current.checks.map((item, i) =>
                    i === index ? { ...item, cwd: event.target.value } : item,
                  ),
                }))
              }
            />
          </label>
          <label>
            超时（秒）
            <input
              type="number"
              min="1"
              max="3600"
              value={check.timeoutSecs ?? 300}
              onChange={(event) =>
                setConfig((current) => ({
                  ...current,
                  checks: current.checks.map((item, i) =>
                    i === index
                      ? { ...item, timeoutSecs: Number(event.target.value) }
                      : item,
                  ),
                }))
              }
            />
          </label>
          <label>
            非敏感环境覆盖（KEY=value，每行一项）
            <textarea
              rows={2}
              value={environmentDrafts[check.id] ?? ""}
              onChange={(event) => setEnvironmentDrafts(current => ({ ...current, [check.id]: event.target.value }))}
            />
          </label>
          <label>
            <input
              type="checkbox"
              checked={check.autoRun}
              onChange={(event) =>
                setConfig((current) => ({
                  ...current,
                  checks: current.checks.map((item, i) =>
                    i === index
                      ? { ...item, autoRun: event.target.checked }
                      : item,
                  ),
                }))
              }
            />
            本轮修改后自动运行
          </label>
          <button
            type="button"
            onClick={() =>
              setConfig((current) => ({
                checks: current.checks.filter((_, i) => i !== index),
              }))
            }
          >
            移除
          </button>
        </fieldset>
      ))}
      {error && (
        <p role="alert">
          {error}
          {!ready && <button type="button" onClick={() => setReload((value) => value + 1)}>
            重试读取
          </button>}
        </p>
      )}
      <button
        type="button"
        disabled={!ready || busy}
        onClick={() =>
          setConfig((current) => ({
            checks: [
              ...current.checks,
              {
                id: crypto.randomUUID(),
                kind: "test",
                command: "",
                label: "",
                cwd: ".",
                env: {},
                timeoutSecs: 300,
                autoRun: false,
              },
            ],
          }))
        }
      >
        添加检查
      </button>
      <button
        type="button"
        disabled={!ready || busy}
        onClick={async () => {
          const savingGeneration = generation.current;
          setBusy(true);
          try {
            const prepared = { checks: config.checks.map(check => {
              const timeout = check.timeoutSecs ?? 300;
              if (!Number.isInteger(timeout) || timeout < 1 || timeout > 3600)
                throw new Error(`检查“${check.label || check.command}”的超时应为 1～3600 秒的整数`);
              return { ...check, timeoutSecs: timeout, command: check.command.trim(), env: parseEnvironment(environmentDrafts[check.id] ?? "") };
            }) };
            await codingApi.saveVerificationConfig(root, prepared);
            if (savingGeneration !== generation.current) return;
            setError("");
            setNotice("检查配置已保存");
            onSaved();
          } catch (error) {
            if (savingGeneration === generation.current) setError(String(error));
          } finally {
            if (savingGeneration === generation.current) setBusy(false);
          }
        }}
      >
        {busy ? "保存中…" : "保存配置"}
      </button>
      {notice && <p role="status">{notice}</p>}
    </details>
  );
}
