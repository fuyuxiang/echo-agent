// Every native acceptance launcher starts from this credential-free environment.
// OS HOME stays intact; Runtime compatibility discovery uses compat-home inside
// the explicitly selected data-home instead of vendor folders in the real HOME.
export function cleanNativeReviewEnvironment(environment = process.env) {
  return {
    ...Object.fromEntries(Object.entries(environment).filter(([name]) =>
      !/^(?:ECHO_AGENT_|ECHO_DESKTOP_REVIEW|ECHO_DESKTOP_VALIDATION|ECHO_LOCAL_REVIEW|ECHO_WEBDAV_REVIEW|GROK_|OTEL_|CLAUDE_|CURSOR_|OPENAI_|ANTHROPIC_|MINIMAX_|DEEPSEEK_|CODEX_)/i.test(name)
      && !/^(?:HISTFILE|CONFIG_PATH|CONFIG|NODE_OPTIONS|NODE_PATH|PYTHONPATH|PYTHONHOME|BASH_ENV|ENV|ZDOTDIR|XDG_CONFIG_HOME|XDG_DATA_HOME|XDG_CACHE_HOME|RUSTC_WRAPPER|RUSTC_WORKSPACE_WRAPPER|LD_PRELOAD|DYLD_INSERT_LIBRARIES|_ECHO_AGENT_CLAUDE_MARKER_OVERRIDE)$/i.test(name))),
    ECHO_AGENT_REVIEW_ISOLATED: '1',
    // A fresh data home otherwise probes the host hardware ID during ACP
    // initialize (unbounded PowerShell/WMI queries on Windows). Identity is
    // synthetic in these isolated reviews, just like their model credentials.
    ECHO_AGENT_AGENT_ID: '00000000-0000-4000-8000-000000000001',
    ECHO_AGENT_TELEMETRY_ENABLED: 'false',
    ECHO_AGENT_TELEMETRY_TRACE_UPLOAD: 'false',
  };
}

export function withNativeReviewLoopbackBypass(environment) {
  const bypass = [...new Set(['127.0.0.1', 'localhost', '::1',
    ...(environment.NO_PROXY ?? '').split(','), ...(environment.no_proxy ?? '').split(',')])].filter(Boolean).join(',');
  return { ...environment, NO_PROXY: bypass, no_proxy: bypass };
}
