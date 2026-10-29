import { AlertTriangle, CheckCircle2, CircleSlash, Clock, LoaderCircle, Play, ShieldAlert, Square } from "lucide-react";

import type { DetectedCommand, VerificationKind, VerificationRecord } from "../lib/types";

function verificationLabel(kind: VerificationKind): string {
  switch (kind) {
    case "build": return "构建";
    case "lint": return "静态检查";
    case "type_check": return "类型检查";
    case "test": return "测试";
    default: return "命令";
  }
}

interface VerificationViewProps {
  records: VerificationRecord[];
  currentRevision?: string;
  currentConfigurationRevision?: string;
  dirty?: boolean;
  detected: DetectedCommand[];
  running: boolean;
  hasTask: boolean;
  executionBlocked?: boolean;
  onRun: (command: DetectedCommand) => void;
  onRunAll: () => void;
  onOpenOutput: (record: VerificationRecord) => void;
  onCancel?: () => void;
  redCheckpoint?: boolean;
  redRecorded?: boolean;
  onRunRed?: (command: DetectedCommand) => void;
}

function formatDuration(durationMs: number): string {
  if (durationMs < 1_000) return `${durationMs}ms`;
  return `${(durationMs / 1_000).toFixed(durationMs < 10_000 ? 1 : 0)}s`;
}

/**
 * Verification runs and their results.
 *
 * A record's verdict comes from its exit code, and `structured: false` is shown
 * explicitly — a passing run whose output could not be parsed must not look like
 * a clean structured result.
 */
export function VerificationView({
  records,
  currentRevision, currentConfigurationRevision, dirty = false,
  detected,
  running,
  hasTask,
  executionBlocked = false,
  onRun,
  onRunAll,
  onOpenOutput,
  onCancel,
  redCheckpoint = false,
  redRecorded = false,
  onRunRed,
}: VerificationViewProps) {
  const latest = [...records].reverse();

  return (
    <div className="coding-panel">
      <div className="coding-panel__actions">
        <button type="button" disabled={executionBlocked || !hasTask || running || detected.length === 0 || redCheckpoint} onClick={onRunAll}>
          {running ? <LoaderCircle size={12} className="is-spinning" /> : <Play size={12} />}
          运行全部验证
        </button>
        {running && onCancel && (
          <button type="button" onClick={onCancel}>
            <Square size={12} /> 停止验证
          </button>
        )}
        {detected.map((command) => (
          <button
            key={command.command}
            type="button"
            disabled={executionBlocked || !hasTask || running || redCheckpoint}
            onClick={() => onRun(command)}
            title={command.requiresApproval
              ? `${command.command}\n来自 Agent 执行计划，点击后需确认`
              : command.command}
          >
            {command.requiresApproval && <ShieldAlert size={12} aria-label="需要确认" />}
            {command.label}
          </button>
        ))}
        {redCheckpoint && !redRecorded && detected.filter((command) => command.kind === "test").map((command) => (
          <button
            key={`red:${command.command}`}
            type="button"
            disabled={executionBlocked || running}
            onClick={() => onRunRed?.(command)}
            title={`预期失败的测试先行检查：${command.command}`}
          >
            {running ? <LoaderCircle size={12} className="is-spinning" /> : <Play size={12} />}
            运行 RED · {command.label}
          </button>
        ))}
        {redCheckpoint && redRecorded && <span className="coding-panel__hint">RED 已记录，可继续实现</span>}
        {detected.length === 0 && (
          <span className="coding-panel__hint">未从工程清单识别到验证命令</span>
        )}
      </div>

      {dirty && <p role="status" className="coding-panel__hint">编辑器有未保存内容；磁盘检查结果不代表当前 buffer。运行前需要保存。</p>}
      {latest.length === 0 ? (
        <div className="coding-panel__empty">尚未运行构建或测试</div>
      ) : (
        <div className="coding-panel__list">
          {latest.map((record) => {
            const stale = dirty || Boolean(currentRevision && record.contentRevision !== currentRevision)
              || Boolean(currentConfigurationRevision && record.configurationRevision !== currentConfigurationRevision);
            return (
            <button
              key={record.id}
              type="button"
              className={`coding-verification is-${record.status}`}
              onClick={() => onOpenOutput(record)}
            >
              {record.status === "passed" ? (
                <CheckCircle2 size={13} />
              ) : record.status === "running" ? (
                <LoaderCircle size={13} className="is-spinning" />
              ) : record.status === "cancelled" ? (
                <CircleSlash size={13} />
              ) : record.status === "timed_out" ? (
                <Clock size={13} />
              ) : (
                <AlertTriangle size={13} />
              )}
              <em>{verificationLabel(record.kind)}</em>
              <code>{record.command}</code>
              {stale && <b title="源码、检查配置或运行环境依据已改变">已过期</b>}
              {!record.contentRevision && <b>旧记录未绑定版本</b>}
              <span>
                {record.testSummary
                  ? `${record.testSummary.passed} 通过 / ${record.testSummary.failed} 失败${
                      record.testSummary.skipped > 0 ? ` / ${record.testSummary.skipped} 跳过` : ""
                    }`
                  : record.status === "running" ? "运行中" : record.status === "passed"
                    ? "通过"
                    : record.status === "environment_unavailable"
                      ? "环境未就绪"
                    : record.status === "timed_out"
                      ? "超时"
                      : record.status === "cancelled"
                        ? "已取消"
                        : "失败"}
              </span>
              {!record.structured && record.kind === "test" && (
                <b title="未能解析测试摘要，结论仅依据退出码">未结构化</b>
              )}
              <small>
                退出码 {record.exitCode ?? "无"} · {formatDuration(record.durationMs)}
              </small>
            </button>
          );})}
        </div>
      )}
    </div>
  );
}
