/**
 * 团队状态视图 —— 展示当前会话中已创建的专家团（来自 create_team 工具调用）。
 *
 * 数据源：已落盘的团队运行时快照 + 当前会话 transcript 中尚在创建或失败的过程。
 * 与 SubagentPanel（展示子代理运行时进度）互补：本视图展示「有哪些团队」，
 * SubagentPanel 展示「团队派发的子任务在跑成什么样」。
 */
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { teamSnapshot, type RuntimeTeamInfo } from "@/lib/agent-client";
import { deriveTeams, isCreateTeamTool, teamStats, type TeamInfo } from "@/lib/team-derive";
import type { ChatMessage } from "@/stores/session-store";

interface TeamStatusViewProps {
  messages?: ChatMessage[];
}

const STATUS_LABEL: Record<string, string> = {
  in_progress: "创建中",
  completed: "已就绪",
  failed: "失败",
};

export function TeamStatusView({ messages }: TeamStatusViewProps) {
  const transcriptTeams = useMemo(() => {
    if (!messages) return [];
    return deriveTeams(messages);
  }, [messages]);
  const [runtimeTeams, setRuntimeTeams] = useState<RuntimeTeamInfo[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const refreshGenerationRef = useRef(0);
  // The persisted registry changes with team tools, not with every streamed
  // text token. Watch lifecycle transitions while keeping manual refresh.
  const teamLifecycle = useMemo(() => JSON.stringify((messages ?? []).flatMap((message) =>
    message.parts.flatMap((part) => {
      if (part.kind !== "tool_call") return [];
      const tool = part.toolCall;
      if (!isCreateTeamTool(tool) && !/team_(?:delete|status)/i.test(`${tool.kind} ${tool.title}`)) return [];
      return [[tool.toolCallId, tool.kind, tool.status]];
    }),
  )), [messages]);

  const refresh = useCallback((): Promise<void> => {
    const generation = ++refreshGenerationRef.current;
    setLoading(true);
    return teamSnapshot()
      .then((snapshot) => {
        if (refreshGenerationRef.current !== generation) return;
        setRuntimeTeams(snapshot);
        setLoaded(true);
        setError(null);
      })
      .catch((cause) => {
        if (refreshGenerationRef.current !== generation) return;
        setError(String(cause).replace(/^Error:\s*/, ""));
      })
      .finally(() => {
        if (refreshGenerationRef.current === generation) setLoading(false);
      });
  }, []);

  useEffect(() => {
    void refresh().catch(() => {});
  }, [refresh, teamLifecycle]);

  useEffect(() => () => { refreshGenerationRef.current += 1; }, []);

  const teams = useMemo<TeamInfo[]>(() => {
    if (!loaded) return transcriptTeams;
    const durable: TeamInfo[] = runtimeTeams.map((team) => ({
      id: `runtime:${team.teamId}`,
      teamId: team.teamId,
      members: team.members,
      createdAt: team.createdAt,
      status: "completed",
    }));
    const durableIds = new Set(durable.map((team) => team.teamId));
    const transient = transcriptTeams.filter(
      (team) => team.status !== "completed" && !durableIds.has(team.teamId),
    );
    return [...transient, ...durable];
  }, [loaded, runtimeTeams, transcriptTeams]);

  const stats = useMemo(() => teamStats(teams), [teams]);

  return (
    <div className="team-status-view" role="region" aria-label="团队状态">
      <div className="team-status-view__head">
        <span className="team-status-view__title">团队</span>
        <div className="team-status-view__head-actions">
          {teams.length > 0 && (
            <span className="team-status-view__summary">
              {stats.teamCount} 个 · {stats.memberCount} 名成员
            </span>
          )}
          <button type="button" className="runtime-panel-refresh" onClick={() => void refresh().catch(() => {})} disabled={loading}>
            {loading ? "刷新中…" : "刷新"}
          </button>
        </div>
      </div>
      {error && (
        <div className="runtime-panel-empty runtime-panel-empty--error" role="alert">
          团队运行时读取失败：{error}{loaded ? "。已保留上次结果，可重新刷新。" : "。请重试刷新。"}
        </div>
      )}
      {teams.length === 0 ? (
        !error && (
        <div className="runtime-panel-empty" role={loading && !loaded ? "status" : undefined}>{loading && !loaded ? "正在读取团队状态…" : "当前没有活动团队。Agent 创建团队后，成员和状态会在这里持久展示。"}</div>
        )
      ) : (
        <ul className="team-status-view__list">
        {teams.map((t) => (
          <li
            key={t.id}
            className={"team-status-view__row team-status-view__row--" + t.status}
          >
            <span className="team-status-view__icon">👥</span>
            <div className="team-status-view__info">
              <div className="team-status-view__name-line">
                <span className="team-status-view__team-id">{t.teamId || "(未命名)"}</span>
                <span className="team-status-view__status">
                  {STATUS_LABEL[t.status] ?? t.status}
                </span>
              </div>
              {t.members.length > 0 && (
                <div className="team-status-view__members">
                  {t.members.map((m, i) => (
                    <span key={i} className="team-status-view__member-chip">
                      {m}
                    </span>
                  ))}
                </div>
              )}
            </div>
          </li>
        ))}
        </ul>
      )}
    </div>
  );
}
