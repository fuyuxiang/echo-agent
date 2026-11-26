use std::cell::RefCell;
use std::rc::Rc;

use rhai::{Dynamic, EvalAltResult, Position};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::host::{AgentOpts, HostError, WorkflowHostRequest};
use crate::journal::{HOST_ERROR_KEY, Journal, JournalError, request_hash};
use crate::run::{PauseKind, WorkflowOutcome};
use crate::{MAX_HOST_CALLS, MAX_PARALLEL};

pub struct WorkflowRunParams {
    pub script: String,
    pub args: serde_json::Value,
    pub journal: Journal,
    pub host_tx: mpsc::UnboundedSender<WorkflowHostRequest>,
    pub cancel: CancellationToken,
    pub max_ops: u64,
}

impl WorkflowRunParams {
    pub const DEFAULT_MAX_OPS: u64 = 100_000_000;
}

#[derive(Debug, Clone)]
enum ControlToken {
    Complete(serde_json::Value),
    Pause(PauseKind, String),
    Budget(String),
    Cancelled,
    Fatal(String),
}

struct Ctx {
    host_tx: mpsc::UnboundedSender<WorkflowHostRequest>,
    journal: Journal,
    seq: u64,
}

impl Ctx {
    fn replaying(&self) -> bool {
        self.journal.covers(self.seq)
    }

    fn next_seq(&mut self) -> ScriptResult<u64> {
        Ok(self.reserve_seqs(1)?.start)
    }

    fn reserve_seqs(&mut self, count: usize) -> ScriptResult<std::ops::Range<u64>> {
        let count = u64::try_from(count).map_err(|_| {
            terminated(ControlToken::Fatal(
                "workflow host-call count overflowed".into(),
            ))
        })?;
        let end = self.seq.checked_add(count).ok_or_else(|| {
            terminated(ControlToken::Fatal(
                "workflow host-call count overflowed".into(),
            ))
        })?;
        if end > MAX_HOST_CALLS {
            return Err(terminated(ControlToken::Fatal(format!(
                "workflow exceeded the maximum of {MAX_HOST_CALLS} result-bearing host calls"
            ))));
        }
        let start = self.seq;
        self.seq = end;
        Ok(start..end)
    }

    fn record(
        &mut self,
        seq: u64,
        kind: &str,
        hash: String,
        value: serde_json::Value,
    ) -> ScriptResult<()> {
        self.journal
            .record(seq, kind, hash, value)
            .map_err(journal_fatal)
    }
}

type ScriptResult<T> = Result<T, Box<EvalAltResult>>;

enum PendingAgent {
    Replayed(serde_json::Value),
    Live {
        seq: u64,
        hash: String,
        reply_rx: oneshot::Receiver<Result<crate::host::AgentResult, HostError>>,
    },
}

fn drain_parallel_replies(pending: Vec<PendingAgent>) {
    for entry in pending {
        if let PendingAgent::Live { reply_rx, .. } = entry {
            let _ = reply_rx.blocking_recv();
        }
    }
}

pub fn run_workflow(params: WorkflowRunParams) -> WorkflowOutcome {
    let WorkflowRunParams {
        script,
        args,
        journal,
        host_tx,
        cancel,
        max_ops,
    } = params;

    let ctx = Rc::new(RefCell::new(Ctx {
        host_tx,
        journal,
        seq: 0,
    }));

    let mut engine = rhai::Engine::new();
    engine.set_max_operations(max_ops);
    engine.set_max_call_levels(64);
    engine.set_max_expr_depths(128, 64);
    engine.set_max_string_size(16 * 1024 * 1024);
    engine.set_max_array_size(65_536);
    engine.set_max_map_size(65_536);
    engine.set_module_resolver(rhai::module_resolvers::DummyModuleResolver::new());
    engine.disable_symbol("eval");
    engine.register_fn("timestamp", || -> ScriptResult<()> {
        Err(runtime_error(
            "timestamp() is unavailable: workflow scripts must be deterministic (wall-clock \
             time breaks resume). Pass timestamps in via `args` instead.",
        ))
    });
    engine.register_fn("sleep", |_seconds: i64| -> ScriptResult<()> {
        Err(runtime_error(
            "sleep() is unavailable in workflow scripts — host calls already block until \
             their work finishes.",
        ))
    });
    engine.register_fn("sleep", |_seconds: f64| -> ScriptResult<()> {
        Err(runtime_error(
            "sleep() is unavailable in workflow scripts — host calls already block until \
             their work finishes.",
        ))
    });
    engine.register_fn("exit", || -> ScriptResult<()> {
        Err(runtime_error(
            "exit() is unavailable — end a workflow with complete(value) or pause(kind, msg).",
        ))
    });

    engine.on_progress(move |_ops| {
        if cancel.is_cancelled() {
            Some(Dynamic::from(ControlToken::Cancelled))
        } else {
            None
        }
    });

    register_host_fns(&mut engine, &ctx);

    let ast = match engine.compile(&script) {
        Ok(ast) => ast,
        Err(e) => {
            return WorkflowOutcome::Failed {
                error: format!("script failed to compile: {e}"),
            };
        }
    };

    let mut scope = rhai::Scope::new();
    let args_dyn = match rhai::serde::to_dynamic(&args) {
        Ok(d) => d,
        Err(e) => {
            return WorkflowOutcome::Failed {
                error: format!("invalid workflow args: {e}"),
            };
        }
    };
    scope.push_dynamic("args", args_dyn);

    match engine.eval_ast_with_scope::<Dynamic>(&mut scope, &ast) {
        Ok(value) => WorkflowOutcome::Completed {
            result: dynamic_to_value(value),
        },
        Err(err) => outcome_from_error(*err),
    }
}

fn outcome_from_error(err: EvalAltResult) -> WorkflowOutcome {
    if let Some(token) = find_control_token(&err) {
        return match token {
            ControlToken::Complete(result) => WorkflowOutcome::Completed { result },
            ControlToken::Pause(kind, message) => WorkflowOutcome::Paused { kind, message },
            ControlToken::Budget(message) => WorkflowOutcome::BudgetExceeded { message },
            ControlToken::Cancelled => WorkflowOutcome::Cancelled,
            ControlToken::Fatal(error) => WorkflowOutcome::Failed { error },
        };
    }
    WorkflowOutcome::Failed {
        error: crate::with_rhai_hint(err.to_string()),
    }
}

fn find_control_token(err: &EvalAltResult) -> Option<ControlToken> {
    match err {
        EvalAltResult::ErrorTerminated(token, _) => token.clone().try_cast::<ControlToken>(),
        EvalAltResult::ErrorInFunctionCall(_, _, inner, _) => find_control_token(inner),
        EvalAltResult::ErrorInModule(_, inner, _) => find_control_token(inner),
        _ => None,
    }
}

fn terminated(token: ControlToken) -> Box<EvalAltResult> {
    Box::new(EvalAltResult::ErrorTerminated(
        Dynamic::from(token),
        Position::NONE,
    ))
}

fn runtime_error(message: impl Into<String>) -> Box<EvalAltResult> {
    Box::new(EvalAltResult::ErrorRuntime(
        Dynamic::from(message.into()),
        Position::NONE,
    ))
}

fn journal_fatal(error: JournalError) -> Box<EvalAltResult> {
    terminated(ControlToken::Fatal(error.to_string()))
}

fn dynamic_to_value(d: Dynamic) -> serde_json::Value {
    rhai::serde::from_dynamic::<serde_json::Value>(&d).unwrap_or(serde_json::Value::Null)
}

fn value_to_dynamic(v: &serde_json::Value) -> ScriptResult<Dynamic> {
    rhai::serde::to_dynamic(v).map_err(|e| runtime_error(format!("host result conversion: {e}")))
}

fn map_to_value(map: rhai::Map) -> ScriptResult<serde_json::Value> {
    rhai::serde::from_dynamic::<serde_json::Value>(&Dynamic::from_map(map))
        .map_err(|e| runtime_error(format!("invalid options map: {e}")))
}

fn replay_spawn_agent(
    journal: &Journal,
    seq: u64,
    payload: &serde_json::Value,
    hash: &str,
) -> Result<Option<serde_json::Value>, JournalError> {
    let normal = journal.replay(seq, "spawn_agent", hash);
    match &normal {
        Ok(Some(_)) => return normal,
        Ok(None) | Err(JournalError::Divergence { .. }) => {}
        Err(_) => return normal,
    }

    let Some(mut legacy_payload) = payload.as_object().cloned() else {
        return normal;
    };
    if legacy_payload.remove("effort").is_none() {
        return normal;
    }
    let legacy_hash = request_hash("spawn_agent", &serde_json::Value::Object(legacy_payload));
    journal.replay(seq, "spawn_agent", &legacy_hash)
}

fn host_call<T>(
    ctx: &Rc<RefCell<Ctx>>,
    kind: &'static str,
    payload: serde_json::Value,
    build: impl FnOnce(oneshot::Sender<Result<T, HostError>>) -> WorkflowHostRequest,
    to_result: impl FnOnce(T) -> serde_json::Value,
) -> ScriptResult<serde_json::Value> {
    let hash = request_hash(kind, &payload);
    let seq = ctx.borrow_mut().next_seq()?;

    let replayed = {
        let ctx = ctx.borrow();
        if kind == "spawn_agent" {
            replay_spawn_agent(&ctx.journal, seq, &payload, &hash)
        } else {
            ctx.journal.replay(seq, kind, &hash)
        }
    };
    match replayed {
        Ok(Some(recorded)) => {
            if let Some(err) = replay_host_error(&recorded) {
                return Err(err);
            }
            return Ok(recorded);
        }
        Ok(None) => {}
        Err(error) => return Err(journal_fatal(error)),
    }

    let (reply_tx, reply_rx) = oneshot::channel();
    ctx.borrow()
        .host_tx
        .send(build(reply_tx))
        .map_err(|_| terminated(ControlToken::Fatal("workflow host channel closed".into())))?;

    let reply = reply_rx
        .blocking_recv()
        .map_err(|_| terminated(ControlToken::Fatal("workflow host dropped reply".into())))?;

    let value = match reply {
        Ok(v) => to_result(v),
        Err(HostError::AgentCallQuotaExceeded { requested, maximum }) => {
            return Err(runtime_error(format!(
                "workflow agent-call quota exceeded: requested {requested}, maximum {maximum}"
            )));
        }
        Err(HostError::BudgetExceeded) => {
            return Err(terminated(ControlToken::Budget(
                "workflow agent budget exceeded".into(),
            )));
        }
        Err(HostError::Cancelled) => return Err(terminated(ControlToken::Cancelled)),
        Err(HostError::Unsupported(msg)) => {
            let sentinel = host_error_sentinel(&msg);
            ctx.borrow_mut().record(seq, kind, hash, sentinel)?;
            return Err(runtime_error(msg));
        }
        Err(HostError::Failed(msg)) => {
            let sentinel = host_error_sentinel(&msg);
            ctx.borrow_mut().record(seq, kind, hash, sentinel)?;
            return Err(runtime_error(msg));
        }
    };

    ctx.borrow_mut().record(seq, kind, hash, value.clone())?;
    Ok(value)
}

const HOST_TERMINAL_KEY: &str = "__echo_agent_workflow_parallel_terminal";
const TERMINAL_BUDGET: &str = "budget_exceeded";
const TERMINAL_CANCELLED: &str = "cancelled";
const TERMINAL_DROPPED_REPLY: &str = "dropped_reply";

fn host_error_sentinel(message: &str) -> serde_json::Value {
    serde_json::json!({ HOST_ERROR_KEY: message })
}

fn replay_host_error(recorded: &serde_json::Value) -> Option<Box<rhai::EvalAltResult>> {
    let message = recorded.get(HOST_ERROR_KEY)?.as_str()?;
    Some(runtime_error(message.to_string()))
}

fn host_terminal_sentinel(kind: &str) -> serde_json::Value {
    serde_json::json!({ HOST_TERMINAL_KEY: kind })
}

fn host_terminal_error(kind: &str) -> Box<rhai::EvalAltResult> {
    match kind {
        TERMINAL_BUDGET => terminated(ControlToken::Budget(
            "workflow agent budget exceeded".into(),
        )),
        TERMINAL_CANCELLED => terminated(ControlToken::Cancelled),
        TERMINAL_DROPPED_REPLY => {
            terminated(ControlToken::Fatal("workflow host dropped reply".into()))
        }
        _ => terminated(ControlToken::Fatal(
            "workflow journal contains an unknown terminal marker".into(),
        )),
    }
}

fn is_host_terminal_sentinel(recorded: &serde_json::Value) -> bool {
    recorded.get(HOST_TERMINAL_KEY).is_some()
}

fn host_emit(ctx: &Rc<RefCell<Ctx>>, build: impl FnOnce(bool) -> WorkflowHostRequest) {
    let (replaying, tx) = {
        let ctx = ctx.borrow();
        (ctx.replaying(), ctx.host_tx.clone())
    };
    let _ = tx.send(build(replaying));
}

fn reserve_agent_calls(ctx: &Rc<RefCell<Ctx>>, count: usize) -> ScriptResult<()> {
    if count == 0 {
        return Ok(());
    }
    let count =
        u64::try_from(count).map_err(|_| runtime_error("workflow agent-call count overflowed"))?;
    let (reply_tx, reply_rx) = oneshot::channel();
    ctx.borrow()
        .host_tx
        .send(WorkflowHostRequest::ReserveAgentCalls {
            count,
            reply: reply_tx,
        })
        .map_err(|_| terminated(ControlToken::Fatal("workflow host channel closed".into())))?;
    match reply_rx
        .blocking_recv()
        .map_err(|_| terminated(ControlToken::Fatal("workflow host dropped reply".into())))?
    {
        Ok(()) => Ok(()),
        Err(HostError::AgentCallQuotaExceeded { requested, maximum }) => {
            Err(terminated(ControlToken::Budget(format!(
                "workflow agent budget exceeded: requested {requested}, maximum {maximum}"
            ))))
        }
        Err(HostError::Cancelled) => Err(terminated(ControlToken::Cancelled)),
        Err(HostError::BudgetExceeded) => Err(terminated(ControlToken::Budget(
            "workflow agent budget exceeded".into(),
        ))),
        Err(HostError::Unsupported(message) | HostError::Failed(message)) => {
            Err(runtime_error(message))
        }
    }
}

fn release_agent_calls(ctx: &Rc<RefCell<Ctx>>, count: usize) {
    if count == 0 {
        return;
    }
    let Ok(count) = u64::try_from(count) else {
        return;
    };
    let (reply_tx, reply_rx) = oneshot::channel();
    if ctx
        .borrow()
        .host_tx
        .send(WorkflowHostRequest::ReleaseAgentCalls {
            count,
            reply: reply_tx,
        })
        .is_err()
    {
        return;
    }
    let _ = reply_rx.blocking_recv();
}

fn is_resumable_unjournaled_terminal(err: &EvalAltResult) -> bool {
    matches!(
        find_control_token(err),
        Some(ControlToken::Cancelled | ControlToken::Budget(_))
    )
}

fn spawn_agent_call(ctx: &Rc<RefCell<Ctx>>, opts: AgentOpts) -> ScriptResult<Dynamic> {
    let payload = serde_json::to_value(&opts)
        .map_err(|e| runtime_error(format!("invalid agent options: {e}")))?;
    let hash = request_hash("spawn_agent", &payload);
    let is_live = {
        let ctx = ctx.borrow();
        match replay_spawn_agent(&ctx.journal, ctx.seq, &payload, &hash) {
            Ok(Some(_)) => false,
            Ok(None) => true,
            Err(error) => return Err(journal_fatal(error)),
        }
    };
    if is_live {
        reserve_agent_calls(ctx, 1)?;
    }
    let value = match host_call(
        ctx,
        "spawn_agent",
        payload,
        |reply| WorkflowHostRequest::SpawnAgent { opts, reply },
        |result| serde_json::to_value(result).unwrap_or(serde_json::Value::Null),
    ) {
        Ok(value) => value,
        Err(err) => {
            if is_live && is_resumable_unjournaled_terminal(&err) {
                release_agent_calls(ctx, 1);
            }
            return Err(err);
        }
    };
    value_to_dynamic(&value)
}

fn agent_opts_from_map(prompt: Option<&str>, map: rhai::Map) -> ScriptResult<AgentOpts> {
    let value = map_to_value(map)?;
    let mut opts: AgentOpts = serde_json::from_value(value)
        .map_err(|e| runtime_error(format!("invalid agent options: {e}")))?;
    if let Some(prompt) = prompt {
        opts.prompt = prompt.to_string();
    }
    if opts.prompt.trim().is_empty() {
        return Err(runtime_error("agent prompt must not be empty"));
    }
    Ok(opts)
}

fn register_host_fns(engine: &mut rhai::Engine, ctx: &Rc<RefCell<Ctx>>) {
    let c = ctx.clone();
    engine.register_fn("agent", move |prompt: &str| -> ScriptResult<Dynamic> {
        spawn_agent_call(
            &c,
            AgentOpts {
                prompt: prompt.to_string(),
                ..Default::default()
            },
        )
    });
    let c = ctx.clone();
    engine.register_fn(
        "agent",
        move |prompt: &str, opts: rhai::Map| -> ScriptResult<Dynamic> {
            let opts = agent_opts_from_map(Some(prompt), opts)?;
            spawn_agent_call(&c, opts)
        },
    );

    let c = ctx.clone();
    engine.register_fn(
        "parallel",
        move |items: rhai::Array| -> ScriptResult<rhai::Array> {
            if items.len() > MAX_PARALLEL {
                return Err(runtime_error(format!(
                    "parallel() accepts at most {MAX_PARALLEL} items per call (got {})",
                    items.len()
                )));
            }
            let mut opts_list = Vec::with_capacity(items.len());
            for item in items {
                let map = item
                    .try_cast::<rhai::Map>()
                    .ok_or_else(|| runtime_error("parallel() items must be option maps"))?;
                opts_list.push(agent_opts_from_map(None, map)?);
            }

            let requests = opts_list
                .into_iter()
                .map(|opts| {
                    let payload = serde_json::to_value(&opts)
                        .map_err(|e| runtime_error(format!("invalid agent options: {e}")))?;
                    let hash = request_hash("spawn_agent", &payload);
                    Ok((opts, payload, hash))
                })
                .collect::<ScriptResult<Vec<_>>>()?;
            let live_count = {
                let ctx = c.borrow();
                let mut seq = ctx.seq;
                let mut live = 0usize;
                for (_, payload, hash) in &requests {
                    match replay_spawn_agent(&ctx.journal, seq, payload, hash) {
                        Ok(Some(_)) => {}
                        Ok(None) => live += 1,
                        Err(error) => return Err(journal_fatal(error)),
                    }
                    seq = seq.checked_add(1).ok_or_else(|| {
                        terminated(ControlToken::Fatal(
                            "workflow host-call count overflowed".into(),
                        ))
                    })?;
                }
                live
            };
            reserve_agent_calls(&c, live_count)?;
            let mut pending = Vec::with_capacity(requests.len());
            for (opts, payload, hash) in requests {
                let seq = c.borrow_mut().next_seq().inspect_err(|_| {
                    drain_parallel_replies(std::mem::take(&mut pending));
                })?;
                match replay_spawn_agent(&c.borrow().journal, seq, &payload, &hash) {
                    Ok(Some(value)) => pending.push(PendingAgent::Replayed(value)),
                    Ok(None) => {
                        let (reply_tx, reply_rx) = oneshot::channel();
                        if c.borrow()
                            .host_tx
                            .send(WorkflowHostRequest::SpawnAgent {
                                opts,
                                reply: reply_tx,
                            })
                            .is_err()
                        {
                            drain_parallel_replies(pending);
                            return Err(terminated(ControlToken::Fatal(
                                "workflow host channel closed".into(),
                            )));
                        }
                        pending.push(PendingAgent::Live {
                            seq,
                            hash,
                            reply_rx,
                        });
                    }
                    Err(error) => {
                        drain_parallel_replies(pending);
                        return Err(journal_fatal(error));
                    }
                }
            }

            let mut resolved: Vec<(Option<(u64, String)>, serde_json::Value)> =
                Vec::with_capacity(pending.len());
            let mut terminal_kind: Option<String> = None;
            let mut terminal_error = None;
            let mut resumable_terminal = false;
            for entry in pending {
                match entry {
                    PendingAgent::Replayed(value) => {
                        if is_host_terminal_sentinel(&value) {
                            let kind = value
                                .get(HOST_TERMINAL_KEY)
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or("unknown")
                                .to_string();
                            if kind == TERMINAL_BUDGET || kind == TERMINAL_CANCELLED {
                                terminal_kind.get_or_insert(kind);
                                resumable_terminal = true;
                            } else {
                                terminal_kind.get_or_insert(kind);
                            }
                            continue;
                        }
                        if let Some(error) = replay_host_error(&value) {
                            terminal_error.get_or_insert(error);
                            continue;
                        }
                        resolved.push((None, value));
                    }
                    PendingAgent::Live {
                        seq,
                        hash,
                        reply_rx,
                    } => {
                        let value = match reply_rx.blocking_recv() {
                            Ok(Ok(result)) => {
                                serde_json::to_value(result).unwrap_or(serde_json::Value::Null)
                            }
                            Ok(Err(HostError::BudgetExceeded)) => {
                                terminal_kind.get_or_insert_with(|| TERMINAL_BUDGET.to_string());
                                resumable_terminal = true;
                                host_terminal_sentinel(TERMINAL_BUDGET)
                            }
                            Ok(Err(HostError::Cancelled)) => {
                                terminal_kind.get_or_insert_with(|| TERMINAL_CANCELLED.to_string());
                                resumable_terminal = true;
                                host_terminal_sentinel(TERMINAL_CANCELLED)
                            }
                            Err(_) => {
                                terminal_kind
                                    .get_or_insert_with(|| TERMINAL_DROPPED_REPLY.to_string());
                                host_terminal_sentinel(TERMINAL_DROPPED_REPLY)
                            }
                            Ok(Err(
                                HostError::AgentCallQuotaExceeded { .. }
                                | HostError::Unsupported(_)
                                | HostError::Failed(_),
                            )) => serde_json::Value::Null,
                        };
                        resolved.push((Some((seq, hash)), value));
                    }
                }
            }

            if resumable_terminal {
                release_agent_calls(&c, live_count);
            } else {
                for (live, value) in &resolved {
                    let Some((seq, hash)) = live else {
                        continue;
                    };
                    if let Err(error) =
                        c.borrow_mut()
                            .record(*seq, "spawn_agent", hash.clone(), value.clone())
                    {
                        terminal_error.get_or_insert(error);
                        break;
                    }
                }
            }

            if let Some(kind) = terminal_kind {
                return Err(host_terminal_error(&kind));
            }
            if let Some(error) = terminal_error {
                return Err(error);
            }

            let mut results = rhai::Array::with_capacity(resolved.len());
            for (_, value) in resolved {
                match value_to_dynamic(&value) {
                    Ok(value) => results.push(value),
                    Err(error) => return Err(error),
                }
            }
            Ok(results)
        },
    );

    let c = ctx.clone();
    engine.register_fn("phase", move |title: &str| {
        let title = title.to_string();
        host_emit(&c, |replayed| WorkflowHostRequest::Phase {
            title,
            replayed,
        });
    });

    let c = ctx.clone();
    engine.register_fn("log", move |message: &str| {
        let message = message.to_string();
        host_emit(&c, |replayed| WorkflowHostRequest::Log {
            message,
            replayed,
        });
    });
    let c = ctx.clone();
    engine.on_print(move |message| {
        let message = message.to_string();
        host_emit(&c, |replayed| WorkflowHostRequest::Log {
            message,
            replayed,
        });
    });

    let c = ctx.clone();
    engine.on_debug(move |message, _source, _pos| {
        let message = message.to_string();
        host_emit(&c, |replayed| WorkflowHostRequest::Log {
            message,
            replayed,
        });
    });

    let c = ctx.clone();
    engine.register_fn(
        "telemetry_event",
        move |name: &str, fields: rhai::Map| -> ScriptResult<()> {
            let fields = map_to_value(fields)?;
            let name = name.to_string();
            host_emit(&c, |replayed| WorkflowHostRequest::Telemetry {
                name,
                fields,
                replayed,
            });
            Ok(())
        },
    );

    engine.register_fn("complete", move |value: Dynamic| -> ScriptResult<()> {
        Err(terminated(ControlToken::Complete(dynamic_to_value(value))))
    });
    engine.register_fn("complete", move || -> ScriptResult<()> {
        Err(terminated(ControlToken::Complete(serde_json::Value::Null)))
    });

    engine.register_fn(
        "pause",
        move |kind: &str, message: &str| -> ScriptResult<()> {
            let kind: PauseKind = kind.parse().map_err(|e: String| runtime_error(e))?;
            Err(terminated(ControlToken::Pause(kind, message.to_string())))
        },
    );

    let c = ctx.clone();
    engine.register_fn(
        "await_user",
        move |kind: &str, message: &str| -> ScriptResult<()> {
            let parsed: PauseKind = kind.parse().map_err(|e: String| runtime_error(e))?;
            let payload = serde_json::json!({ "kind": kind, "message": message });
            let hash = request_hash("await_user", &payload);
            let seq = c.borrow_mut().next_seq()?;
            let replayed = c.borrow().journal.replay(seq, "await_user", &hash);
            match replayed {
                Ok(Some(_)) => Ok(()),
                Ok(None) => {
                    c.borrow_mut()
                        .record(seq, "await_user", hash, serde_json::Value::Null)?;
                    Err(terminated(ControlToken::Pause(parsed, message.to_string())))
                }
                Err(error) => Err(journal_fatal(error)),
            }
        },
    );

    let c = ctx.clone();
    engine.register_fn("budget", move || -> ScriptResult<Dynamic> {
        let value = host_call(
            &c,
            "budget",
            serde_json::Value::Null,
            |reply| WorkflowHostRequest::BudgetQuery { reply },
            |state| serde_json::to_value(state).unwrap_or(serde_json::Value::Null),
        )?;
        value_to_dynamic(&value)
    });

    let c = ctx.clone();
    engine.register_fn(
        "render_template",
        move |name: &str, vars: rhai::Map| -> ScriptResult<Dynamic> {
            let vars = map_to_value(vars)?;
            let payload = serde_json::json!({ "name": name, "vars": vars });
            let name = name.to_string();
            let value = host_call(
                &c,
                "render_template",
                payload,
                |reply| WorkflowHostRequest::RenderTemplate { name, vars, reply },
                serde_json::Value::String,
            )?;
            value_to_dynamic(&value)
        },
    );

    let c = ctx.clone();
    engine.register_fn(
        "write_scratch_file",
        move |name: &str, content: &str| -> ScriptResult<Dynamic> {
            let payload = serde_json::json!({ "name": name, "content": content });
            let (name, content) = (name.to_string(), content.to_string());
            let value = host_call(
                &c,
                "write_scratch_file",
                payload,
                |reply| WorkflowHostRequest::WriteScratchFile {
                    name,
                    content,
                    reply,
                },
                serde_json::Value::String,
            )?;
            value_to_dynamic(&value)
        },
    );

    let c = ctx.clone();
    engine.register_fn(
        "read_scratch_file",
        move |name: &str| -> ScriptResult<Dynamic> {
            let payload = serde_json::json!({ "name": name });
            let name = name.to_string();
            let value = host_call(
                &c,
                "read_scratch_file",
                payload,
                |reply| WorkflowHostRequest::ReadScratchFile { name, reply },
                serde_json::Value::String,
            )?;
            value_to_dynamic(&value)
        },
    );

    let c = ctx.clone();
    engine.register_fn(
        "git_diff_since",
        move |commit: &str| -> ScriptResult<Dynamic> {
            let payload = serde_json::json!({ "commit": commit });
            let commit = commit.to_string();
            let value = host_call(
                &c,
                "git_diff_since",
                payload,
                |reply| WorkflowHostRequest::GitDiffSince { commit, reply },
                serde_json::Value::String,
            )?;
            value_to_dynamic(&value)
        },
    );

    engine.register_fn("fingerprint", |text: &str| -> String {
        crate::journal::request_hash("fingerprint", &serde_json::Value::String(text.to_string()))
    });
    engine.register_fn("json_encode", |value: Dynamic| -> ScriptResult<String> {
        serde_json::to_string(&dynamic_to_value(value))
            .map_err(|error| runtime_error(format!("json encoding failed: {error}")))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::{AgentResult, BudgetState};

    fn spawn_mock_host(
        mut rx: mpsc::UnboundedReceiver<WorkflowHostRequest>,
        mut on_request: impl FnMut(WorkflowHostRequest) + Send + 'static,
    ) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            while let Some(req) = rx.blocking_recv() {
                match req {
                    WorkflowHostRequest::ReserveAgentCalls { reply, .. }
                    | WorkflowHostRequest::ReleaseAgentCalls { reply, .. } => {
                        let _ = reply.send(Ok(()));
                    }
                    other => on_request(other),
                }
            }
        })
    }

    fn spawn_budget_tracking_host(
        mut rx: mpsc::UnboundedReceiver<WorkflowHostRequest>,
        agents_used: std::sync::Arc<std::sync::atomic::AtomicU64>,
        mut on_request: impl FnMut(WorkflowHostRequest) + Send + 'static,
    ) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            while let Some(req) = rx.blocking_recv() {
                match req {
                    WorkflowHostRequest::ReserveAgentCalls { count, reply } => {
                        agents_used.fetch_add(count, std::sync::atomic::Ordering::SeqCst);
                        let _ = reply.send(Ok(()));
                    }
                    WorkflowHostRequest::ReleaseAgentCalls { count, reply } => {
                        let _ = agents_used.fetch_update(
                            std::sync::atomic::Ordering::SeqCst,
                            std::sync::atomic::Ordering::SeqCst,
                            |used| Some(used.saturating_sub(count)),
                        );
                        let _ = reply.send(Ok(()));
                    }
                    other => on_request(other),
                }
            }
        })
    }

    fn agent_result(output: &str) -> AgentResult {
        AgentResult {
            agent_id: "child-1".into(),
            success: true,
            output: serde_json::Value::String(output.into()),
            cancelled: false,
            tokens_used: 10,
            duration_ms: 5,
        }
    }

    fn params(
        script: &str,
        journal: Journal,
        host_tx: mpsc::UnboundedSender<WorkflowHostRequest>,
    ) -> WorkflowRunParams {
        WorkflowRunParams {
            script: script.to_string(),
            args: serde_json::json!({ "objective": "test" }),
            journal,
            host_tx,
            cancel: CancellationToken::new(),
            max_ops: WorkflowRunParams::DEFAULT_MAX_OPS,
        }
    }

    fn legacy_spawn_agent_hash(opts: &AgentOpts) -> String {
        let mut payload = serde_json::to_value(opts).unwrap();
        assert!(payload.as_object_mut().unwrap().remove("effort").is_some());
        request_hash("spawn_agent", &payload)
    }

    #[test]
    fn happy_path_completes_with_agent_output() {
        let (tx, rx) = mpsc::unbounded_channel();
        let host = spawn_mock_host(rx, |req| match req {
            WorkflowHostRequest::SpawnAgent { reply, .. } => {
                let _ = reply.send(Ok(agent_result("agent says hi")));
            }
            WorkflowHostRequest::Phase { .. } | WorkflowHostRequest::Log { .. } => {}
            other => panic!("unexpected request: {other:?}"),
        });

        let outcome = run_workflow(params(
            r#"
            let meta = #{ name: "t", description: "d" };
            phase("Work");
            let r = agent("do it", #{ label: "worker" });
            complete(r.output);
            "#,
            Journal::new(None),
            tx,
        ));
        drop(host);

        match outcome {
            WorkflowOutcome::Completed { result } => {
                assert_eq!(result, serde_json::json!("agent says hi"));
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    fn catchable_host_failure_journals_and_replays() {
        let dir = tempfile::tempdir().unwrap();
        let journal_path = dir.path().join("journal.jsonl");
        let script = r#"
            let meta = #{ name: "t", description: "d" };
            let d = "";
            try { d = git_diff_since("abc"); } catch (e) { d = "fallback"; }
            let r = agent("work");
            complete(r.output + ":" + d);
        "#;

        let (tx, rx) = mpsc::unbounded_channel();
        let host = spawn_mock_host(rx, |req| match req {
            WorkflowHostRequest::GitDiffSince { reply, .. } => {
                let _ = reply.send(Err(crate::host::HostError::Failed("boom".into())));
            }
            WorkflowHostRequest::SpawnAgent { reply, .. } => {
                let _ = reply.send(Ok(agent_result("one")));
            }
            WorkflowHostRequest::Phase { .. } | WorkflowHostRequest::Log { .. } => {}
            other => panic!("unexpected request: {other:?}"),
        });
        let outcome = run_workflow(params(script, Journal::new(Some(journal_path.clone())), tx));
        drop(host);
        match outcome {
            WorkflowOutcome::Completed { result } => {
                assert_eq!(result, serde_json::json!("one:fallback"));
            }
            other => panic!("expected Completed, got {other:?}"),
        }

        let (tx, rx) = mpsc::unbounded_channel();
        let host = spawn_mock_host(rx, |req| match req {
            WorkflowHostRequest::Phase { .. } | WorkflowHostRequest::Log { .. } => {}
            other => panic!("replay must not hit the host: {other:?}"),
        });
        let outcome = run_workflow(params(script, Journal::load(journal_path).unwrap(), tx));
        drop(host);
        match outcome {
            WorkflowOutcome::Completed { result } => {
                assert_eq!(result, serde_json::json!("one:fallback"));
            }
            other => panic!("expected replayed Completed, got {other:?}"),
        }
    }

    #[test]
    fn await_user_pauses_once_then_passes_on_resume() {
        let dir = tempfile::tempdir().unwrap();
        let journal_path = dir.path().join("journal.jsonl");
        let script = r#"
            let meta = #{ name: "t", description: "d" };
            await_user("back_off", "needs a human");
            complete("resumed");
        "#;

        let (tx, _rx) = mpsc::unbounded_channel();
        let outcome = run_workflow(params(script, Journal::new(Some(journal_path.clone())), tx));
        match outcome {
            WorkflowOutcome::Paused { kind, message } => {
                assert_eq!(kind, PauseKind::BackOff);
                assert_eq!(message, "needs a human");
            }
            other => panic!("expected Paused, got {other:?}"),
        }

        let (tx, _rx) = mpsc::unbounded_channel();
        let outcome = run_workflow(params(script, Journal::load(journal_path).unwrap(), tx));
        match outcome {
            WorkflowOutcome::Completed { result } => {
                assert_eq!(result, serde_json::json!("resumed"));
            }
            other => panic!("expected Completed after resume, got {other:?}"),
        }
    }

    #[test]
    fn timestamp_is_blocked_for_determinism() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let outcome = run_workflow(params(
            r#"
            let meta = #{ name: "t", description: "d" };
            let t = timestamp();
            complete("unreachable");
            "#,
            Journal::new(None),
            tx,
        ));
        match outcome {
            WorkflowOutcome::Failed { error } => {
                assert!(error.contains("deterministic"), "unexpected error: {error}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn args_are_visible_to_script() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let outcome = run_workflow(params(
            r#"
            let meta = #{ name: "t", description: "d" };
            complete(args.objective);
            "#,
            Journal::new(None),
            tx,
        ));
        match outcome {
            WorkflowOutcome::Completed { result } => {
                assert_eq!(result, serde_json::json!("test"));
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    fn pause_maps_kind() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let outcome = run_workflow(params(
            r#"
            let meta = #{ name: "t", description: "d" };
            pause("back_off", "too many rejections");
            "#,
            Journal::new(None),
            tx,
        ));
        match outcome {
            WorkflowOutcome::Paused { kind, message } => {
                assert_eq!(kind, PauseKind::BackOff);
                assert_eq!(message, "too many rejections");
            }
            other => panic!("expected Paused, got {other:?}"),
        }
    }

    #[test]
    fn budget_exceeded_terminates() {
        let (tx, rx) = mpsc::unbounded_channel();
        let host = spawn_mock_host(rx, |req| {
            if let WorkflowHostRequest::SpawnAgent { reply, .. } = req {
                let _ = reply.send(Err(HostError::BudgetExceeded));
            }
        });
        let outcome = run_workflow(params(
            r#"
            let meta = #{ name: "t", description: "d" };
            agent("expensive");
            complete("unreachable");
            "#,
            Journal::new(None),
            tx,
        ));
        drop(host);
        assert!(matches!(outcome, WorkflowOutcome::BudgetExceeded { .. }));
    }

    #[test]
    fn cancellation_wins_over_pure_loop() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let mut p = params(
            r#"
            let meta = #{ name: "t", description: "d" };
            let x = 0;
            loop { x += 1; }
            "#,
            Journal::new(None),
            tx,
        );
        p.cancel = cancel;
        assert!(matches!(run_workflow(p), WorkflowOutcome::Cancelled));
    }

    #[test]
    fn parallel_rejects_oversized_fanout_before_spawning() {
        let (tx, rx) = mpsc::unbounded_channel();
        let requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let requests_in_host = requests.clone();
        let host = spawn_mock_host(rx, move |req| {
            requests_in_host.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if let WorkflowHostRequest::SpawnAgent { reply, .. } = req {
                let _ = reply.send(Ok(agent_result("unexpected")));
            }
        });
        let script = format!(
            r#"
            let meta = #{{ name: "t", description: "d" }};
            let jobs = [];
            for i in 0..{} {{ jobs.push(#{{ prompt: "job" + i.to_string() }}); }}
            parallel(jobs);
            "#,
            MAX_PARALLEL + 1
        );
        let outcome = run_workflow(params(&script, Journal::new(None), tx));
        drop(host);
        match outcome {
            WorkflowOutcome::Failed { error } => {
                assert!(error.contains("parallel() accepts at most"), "got: {error}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[test]
    fn host_call_limit_is_non_catchable_and_prevents_sends() {
        let (tx, rx) = mpsc::unbounded_channel();
        let requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let requests_in_host = requests.clone();
        let host = spawn_mock_host(rx, move |req| {
            requests_in_host.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if let WorkflowHostRequest::BudgetQuery { reply } = req {
                let _ = reply.send(Ok(BudgetState {
                    total: None,
                    spent: 0,
                    reserved: 0,
                    remaining: None,
                }));
            }
        });
        let mut journal = Journal::new(None);
        let hash = request_hash("budget", &serde_json::Value::Null);
        for seq in 0..MAX_HOST_CALLS {
            journal
                .record(
                    seq,
                    "budget",
                    hash.clone(),
                    serde_json::json!({ "total": null, "spent": 0, "reserved": 0, "remaining": null }),
                )
                .unwrap();
        }
        let script = format!(
            r#"
            let meta = #{{ name: "t", description: "d" }};
            for i in 0..{} {{ budget(); }}
            try {{ budget(); }} catch (e) {{ complete("caught"); }}
            complete("unreachable");
            "#,
            MAX_HOST_CALLS
        );
        let outcome = run_workflow(params(&script, journal, tx));
        drop(host);
        match outcome {
            WorkflowOutcome::Failed { error } => {
                assert!(error.contains("maximum of"), "got: {error}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[test]
    fn parallel_setup_error_drains_already_sent_replies() {
        let (tx, rx) = mpsc::unbounded_channel();
        let first_reply_observed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let first_reply_in_host = first_reply_observed.clone();
        let host = spawn_mock_host(rx, move |req| {
            if let WorkflowHostRequest::SpawnAgent { reply, .. } = req {
                first_reply_in_host.store(
                    reply.send(Ok(agent_result("drained"))).is_ok(),
                    std::sync::atomic::Ordering::SeqCst,
                );
            }
        });
        let mut journal = Journal::new(None);
        let hash = request_hash("budget", &serde_json::Value::Null);
        for seq in 0..MAX_HOST_CALLS - 1 {
            journal
                .record(
                    seq,
                    "budget",
                    hash.clone(),
                    serde_json::json!({ "total": null, "spent": 0, "reserved": 0, "remaining": null }),
                )
                .unwrap();
        }
        let script = format!(
            r#"
            let meta = #{{ name: "t", description: "d" }};
            for i in 0..{} {{ budget(); }}
            parallel([#{{ prompt: "live" }}, #{{ prompt: "over-limit" }}]);
            "#,
            MAX_HOST_CALLS - 1
        );
        let outcome = run_workflow(params(&script, journal, tx));
        drop(host);
        match outcome {
            WorkflowOutcome::Failed { error } => {
                assert!(error.contains("maximum of"), "got: {error}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        assert!(first_reply_observed.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn parallel_budget_exceeded_leaves_panel_unjournaled_for_raised_cap_resume() {
        let dir = tempfile::tempdir().unwrap();
        let journal_path = dir.path().join("journal.jsonl");
        let (tx, rx) = mpsc::unbounded_channel();
        let second_reply_observed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let second_reply_in_host = second_reply_observed.clone();
        let mut request_index = 0;
        let host = spawn_mock_host(rx, move |req| {
            if let WorkflowHostRequest::SpawnAgent { reply, .. } = req {
                if request_index == 0 {
                    let _ = reply.send(Err(HostError::BudgetExceeded));
                } else {
                    second_reply_in_host.store(
                        reply.send(Ok(agent_result("drained"))).is_ok(),
                        std::sync::atomic::Ordering::SeqCst,
                    );
                }
                request_index += 1;
            }
        });
        let script = r#"
            let meta = #{ name: "t", description: "d" };
            parallel([#{ prompt: "first" }, #{ prompt: "second" }]);
        "#;
        let outcome = run_workflow(params(script, Journal::new(Some(journal_path.clone())), tx));
        drop(host);
        assert!(matches!(outcome, WorkflowOutcome::BudgetExceeded { .. }));
        assert!(second_reply_observed.load(std::sync::atomic::Ordering::SeqCst));

        let journal = Journal::load(journal_path).unwrap();
        assert_eq!(
            journal.len(),
            0,
            "resumable budget terminal must not journal the parallel panel"
        );

        let (tx, rx) = mpsc::unbounded_channel();
        let live_again = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let live_again_host = live_again.clone();
        let host = spawn_mock_host(rx, move |req| {
            if let WorkflowHostRequest::SpawnAgent { reply, .. } = req {
                live_again_host.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let _ = reply.send(Ok(agent_result("after raise")));
            }
        });
        let replay = run_workflow(params(script, journal, tx));
        drop(host);
        assert!(matches!(replay, WorkflowOutcome::Completed { .. }));
        assert_eq!(live_again.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[test]
    fn cancelled_live_agent_releases_budget_so_resume_does_not_double_charge() {
        use std::sync::atomic::{AtomicU64, Ordering};

        let dir = tempfile::tempdir().unwrap();
        let journal_path = dir.path().join("journal.jsonl");
        let agents_used = std::sync::Arc::new(AtomicU64::new(0));
        let script = r#"
            let meta = #{ name: "t", description: "d" };
            let r = agent("work");
            complete(r.output);
        "#;

        {
            let used = agents_used.clone();
            let (tx, rx) = mpsc::unbounded_channel();
            let host = spawn_budget_tracking_host(rx, used, |req| {
                if let WorkflowHostRequest::SpawnAgent { reply, .. } = req {
                    let _ = reply.send(Err(HostError::Cancelled));
                }
            });
            let outcome =
                run_workflow(params(script, Journal::new(Some(journal_path.clone())), tx));
            drop(host);
            assert!(matches!(outcome, WorkflowOutcome::Cancelled));
            assert_eq!(
                agents_used.load(Ordering::SeqCst),
                0,
                "cancelled agent must ReleaseAgentCalls the reserved slot"
            );
            assert_eq!(
                Journal::load(journal_path.clone()).unwrap().len(),
                0,
                "cancelled agent must leave the spawn unjournaled"
            );
        }

        {
            let used = agents_used.clone();
            let (tx, rx) = mpsc::unbounded_channel();
            let host = spawn_budget_tracking_host(rx, used, |req| {
                if let WorkflowHostRequest::SpawnAgent { reply, .. } = req {
                    let _ = reply.send(Ok(agent_result("after resume")));
                }
            });
            let outcome = run_workflow(params(
                script,
                Journal::load(journal_path.clone()).unwrap(),
                tx,
            ));
            drop(host);
            match outcome {
                WorkflowOutcome::Completed { result } => {
                    assert_eq!(result, serde_json::json!("after resume"));
                }
                other => panic!("expected Completed after resume, got {other:?}"),
            }
            assert_eq!(
                agents_used.load(Ordering::SeqCst),
                1,
                "resume must not double-charge a slot left over from cancelled first run"
            );
        }
    }

    #[test]
    fn cancelled_parallel_releases_budget_so_resume_does_not_double_charge() {
        use std::sync::atomic::{AtomicU64, Ordering};

        let dir = tempfile::tempdir().unwrap();
        let journal_path = dir.path().join("journal.jsonl");
        let agents_used = std::sync::Arc::new(AtomicU64::new(0));
        let script = r#"
            let meta = #{ name: "t", description: "d" };
            let results = parallel([#{ prompt: "first" }, #{ prompt: "second" }]);
            complete(results);
        "#;

        {
            let used = agents_used.clone();
            let (tx, rx) = mpsc::unbounded_channel();
            let host = spawn_budget_tracking_host(rx, used, |req| {
                if let WorkflowHostRequest::SpawnAgent { reply, .. } = req {
                    let _ = reply.send(Err(HostError::Cancelled));
                }
            });
            let outcome =
                run_workflow(params(script, Journal::new(Some(journal_path.clone())), tx));
            drop(host);
            assert!(matches!(outcome, WorkflowOutcome::Cancelled));
            assert_eq!(
                agents_used.load(Ordering::SeqCst),
                0,
                "cancelled parallel must release live_count reserved slots"
            );
            assert_eq!(Journal::load(journal_path.clone()).unwrap().len(), 0);
        }

        {
            let used = agents_used.clone();
            let (tx, rx) = mpsc::unbounded_channel();
            let host = spawn_budget_tracking_host(rx, used, |req| {
                if let WorkflowHostRequest::SpawnAgent { reply, .. } = req {
                    let _ = reply.send(Ok(agent_result("ok")));
                }
            });
            let outcome = run_workflow(params(
                script,
                Journal::load(journal_path.clone()).unwrap(),
                tx,
            ));
            drop(host);
            assert!(matches!(outcome, WorkflowOutcome::Completed { .. }));
            assert_eq!(
                agents_used.load(Ordering::SeqCst),
                2,
                "resume must charge exactly live_count, not leftover + live_count"
            );
        }
    }

    #[test]
    fn budget_exceeded_live_agent_releases_budget_so_resume_does_not_double_charge() {
        use std::sync::atomic::{AtomicU64, Ordering};

        let dir = tempfile::tempdir().unwrap();
        let journal_path = dir.path().join("journal.jsonl");
        let agents_used = std::sync::Arc::new(AtomicU64::new(0));
        let script = r#"
            let meta = #{ name: "t", description: "d" };
            let r = agent("work");
            complete(r.output);
        "#;

        {
            let used = agents_used.clone();
            let (tx, rx) = mpsc::unbounded_channel();
            let host = spawn_budget_tracking_host(rx, used, |req| {
                if let WorkflowHostRequest::SpawnAgent { reply, .. } = req {
                    let _ = reply.send(Err(HostError::BudgetExceeded));
                }
            });
            let outcome =
                run_workflow(params(script, Journal::new(Some(journal_path.clone())), tx));
            drop(host);
            assert!(matches!(outcome, WorkflowOutcome::BudgetExceeded { .. }));
            assert_eq!(
                agents_used.load(Ordering::SeqCst),
                0,
                "budget-exceeded agent must ReleaseAgentCalls the reserved slot"
            );
            assert_eq!(Journal::load(journal_path.clone()).unwrap().len(), 0);
        }

        {
            let used = agents_used.clone();
            let (tx, rx) = mpsc::unbounded_channel();
            let host = spawn_budget_tracking_host(rx, used, |req| {
                if let WorkflowHostRequest::SpawnAgent { reply, .. } = req {
                    let _ = reply.send(Ok(agent_result("after raise")));
                }
            });
            let outcome = run_workflow(params(
                script,
                Journal::load(journal_path.clone()).unwrap(),
                tx,
            ));
            drop(host);
            match outcome {
                WorkflowOutcome::Completed { result } => {
                    assert_eq!(result, serde_json::json!("after raise"));
                }
                other => panic!("expected Completed after resume, got {other:?}"),
            }
            assert_eq!(
                agents_used.load(Ordering::SeqCst),
                1,
                "resume after budget terminal must not double-charge"
            );
        }
    }

    #[test]
    fn parallel_journals_soft_failure_null_and_later_success() {
        let dir = tempfile::tempdir().unwrap();
        let journal_path = dir.path().join("journal.jsonl");
        let (tx, rx) = mpsc::unbounded_channel();
        let mut request_index = 0;
        let host = spawn_mock_host(rx, move |req| {
            if let WorkflowHostRequest::SpawnAgent { reply, .. } = req {
                if request_index == 0 {
                    let _ = reply.send(Err(HostError::Failed("boom".into())));
                } else {
                    let _ = reply.send(Ok(agent_result("ok")));
                }
                request_index += 1;
            }
        });
        let script = r#"
            let meta = #{ name: "t", description: "d" };
            let results = parallel([#{ prompt: "first" }, #{ prompt: "second" }]);
            complete(results);
        "#;
        let outcome = run_workflow(params(script, Journal::new(Some(journal_path.clone())), tx));
        drop(host);
        assert!(matches!(outcome, WorkflowOutcome::Completed { .. }));
        let journal = Journal::load(journal_path).unwrap();
        assert_eq!(journal.len(), 2);

        let (tx, mut rx) = mpsc::unbounded_channel();
        let replay = run_workflow(params(script, journal, tx));
        assert!(matches!(replay, WorkflowOutcome::Completed { .. }));
        assert!(
            rx.try_recv().is_err(),
            "dense soft-failure replay must not reexecute either sibling"
        );
    }

    #[test]
    fn parallel_replays_catchable_failure_sentinel() {
        let mut journal = Journal::new(None);
        let opts = AgentOpts {
            prompt: "replayed".into(),
            ..Default::default()
        };
        let hash = request_hash("spawn_agent", &serde_json::to_value(&opts).unwrap());
        journal
            .record(
                0,
                "spawn_agent",
                hash,
                host_error_sentinel("replayed failure"),
            )
            .unwrap();
        let (tx, _rx) = mpsc::unbounded_channel();
        let outcome = run_workflow(params(
            r#"
            let meta = #{ name: "t", description: "d" };
            try {
                parallel([#{ prompt: "replayed" }]);
                complete("not caught");
            } catch (e) {
                complete("caught:" + e);
            }
            "#,
            journal,
            tx,
        ));
        match outcome {
            WorkflowOutcome::Completed { result } => {
                assert_eq!(result, serde_json::json!("caught:replayed failure"));
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    fn parallel_preserves_order_and_nulls_failures() {
        let (tx, rx) = mpsc::unbounded_channel();
        let host = spawn_mock_host(rx, |req| {
            if let WorkflowHostRequest::SpawnAgent { opts, reply } = req {
                if opts.prompt.contains("fail") {
                    let _ = reply.send(Err(HostError::Failed("boom".into())));
                } else {
                    let _ = reply.send(Ok(agent_result(&format!("ok:{}", opts.prompt))));
                }
            }
        });
        let outcome = run_workflow(params(
            r#"
            let meta = #{ name: "t", description: "d" };
            let results = parallel([
                #{ prompt: "a" },
                #{ prompt: "fail-b" },
                #{ prompt: "c" },
            ]);
            let summary = results.map(|r| if r == () { "null" } else { r.output });
            complete(summary);
            "#,
            Journal::new(None),
            tx,
        ));
        drop(host);
        match outcome {
            WorkflowOutcome::Completed { result } => {
                assert_eq!(result, serde_json::json!(["ok:a", "null", "ok:c"]));
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    fn agent_replays_legacy_hash_without_effort() {
        let opts = AgentOpts {
            prompt: "legacy single".into(),
            effort: Some("high".into()),
            ..Default::default()
        };
        let legacy_hash = legacy_spawn_agent_hash(&opts);
        assert_ne!(
            legacy_hash,
            request_hash("spawn_agent", &serde_json::to_value(&opts).unwrap())
        );
        let mut journal = Journal::new(None);
        journal
            .record(
                0,
                "spawn_agent",
                legacy_hash,
                serde_json::to_value(agent_result("legacy result")).unwrap(),
            )
            .unwrap();

        let (tx, mut rx) = mpsc::unbounded_channel();
        let outcome = run_workflow(params(
            r#"
            let meta = #{ name: "t", description: "d" };
            let result = agent("legacy single", #{ effort: "high" });
            complete(result.output);
            "#,
            journal,
            tx,
        ));

        match outcome {
            WorkflowOutcome::Completed { result } => {
                assert_eq!(result, serde_json::json!("legacy result"));
            }
            other => panic!("expected Completed, got {other:?}"),
        }
        assert!(rx.try_recv().is_err(), "replay must not hit the host");
    }

    #[test]
    fn parallel_replays_legacy_hashes_without_effort() {
        let opts = [
            AgentOpts {
                prompt: "legacy first".into(),
                effort: Some("low".into()),
                ..Default::default()
            },
            AgentOpts {
                prompt: "legacy second".into(),
                effort: Some("high".into()),
                ..Default::default()
            },
        ];
        let mut journal = Journal::new(None);
        for (seq, (opts, output)) in opts
            .iter()
            .zip(["first result", "second result"])
            .enumerate()
        {
            journal
                .record(
                    seq as u64,
                    "spawn_agent",
                    legacy_spawn_agent_hash(opts),
                    serde_json::to_value(agent_result(output)).unwrap(),
                )
                .unwrap();
        }

        let (tx, mut rx) = mpsc::unbounded_channel();
        let outcome = run_workflow(params(
            r#"
            let meta = #{ name: "t", description: "d" };
            let results = parallel([
                #{ prompt: "legacy first", effort: "low" },
                #{ prompt: "legacy second", effort: "high" },
            ]);
            complete(results.map(|result| result.output));
            "#,
            journal,
            tx,
        ));

        match outcome {
            WorkflowOutcome::Completed { result } => {
                assert_eq!(result, serde_json::json!(["first result", "second result"]));
            }
            other => panic!("expected Completed, got {other:?}"),
        }
        assert!(rx.try_recv().is_err(), "replay must not hit the host");
    }

    #[test]
    fn new_agent_recording_hash_includes_effort() {
        let dir = tempfile::tempdir().unwrap();
        let journal_path = dir.path().join("journal.jsonl");
        let (tx, rx) = mpsc::unbounded_channel();
        let host = spawn_mock_host(rx, |req| {
            if let WorkflowHostRequest::SpawnAgent { reply, .. } = req {
                let _ = reply.send(Ok(agent_result("recorded")));
            }
        });
        let outcome = run_workflow(params(
            r#"
            let meta = #{ name: "t", description: "d" };
            agent("new recording", #{ effort: "high" });
            complete("ok");
            "#,
            Journal::new(Some(journal_path.clone())),
            tx,
        ));
        drop(host);
        assert!(matches!(outcome, WorkflowOutcome::Completed { .. }));

        let opts = AgentOpts {
            prompt: "new recording".into(),
            effort: Some("high".into()),
            ..Default::default()
        };
        let journal = Journal::load(journal_path).unwrap();
        let current_hash = request_hash("spawn_agent", &serde_json::to_value(&opts).unwrap());
        assert!(
            journal
                .replay(0, "spawn_agent", &current_hash)
                .unwrap()
                .is_some()
        );
        assert!(matches!(
            journal.replay(0, "spawn_agent", &legacy_spawn_agent_hash(&opts)),
            Err(JournalError::Divergence { .. })
        ));
    }

    #[test]
    fn agent_current_hash_mismatch_still_diverges() {
        let opts = AgentOpts {
            prompt: "original prompt".into(),
            effort: Some("high".into()),
            ..Default::default()
        };
        let mut journal = Journal::new(None);
        journal
            .record(
                0,
                "spawn_agent",
                request_hash("spawn_agent", &serde_json::to_value(&opts).unwrap()),
                serde_json::to_value(agent_result("must not replay")).unwrap(),
            )
            .unwrap();

        let (tx, mut rx) = mpsc::unbounded_channel();
        let outcome = run_workflow(params(
            r#"
            let meta = #{ name: "t", description: "d" };
            agent("edited prompt", #{ effort: "high" });
            "#,
            journal,
            tx,
        ));

        match outcome {
            WorkflowOutcome::Failed { error } => {
                assert!(error.contains("divergence"), "got: {error}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        assert!(
            rx.try_recv().is_err(),
            "divergent replay must not hit the host"
        );
    }

    #[test]
    fn journal_replay_skips_host_calls() {
        let script = r#"
            let meta = #{ name: "t", description: "d" };
            let a = agent("first");
            let b = budget();
            complete(#{ out: a.output, spent: b.spent, reserved: b.reserved, remaining: b.remaining });
        "#;

        let dir = tempfile::tempdir().unwrap();
        let journal_path = dir.path().join("journal.jsonl");

        let (tx, rx) = mpsc::unbounded_channel();
        let host = spawn_mock_host(rx, |req| match req {
            WorkflowHostRequest::SpawnAgent { reply, .. } => {
                let _ = reply.send(Ok(agent_result("recorded output")));
            }
            WorkflowHostRequest::BudgetQuery { reply } => {
                let _ = reply.send(Ok(BudgetState {
                    total: Some(1000),
                    spent: 123,
                    reserved: 100,
                    remaining: Some(777),
                }));
            }
            _ => {}
        });
        let first = run_workflow(params(script, Journal::new(Some(journal_path.clone())), tx));
        drop(host);
        let WorkflowOutcome::Completed { result: first } = first else {
            panic!("first run should complete");
        };

        let (tx, rx) = mpsc::unbounded_channel();
        let host = spawn_mock_host(rx, |req| match req {
            WorkflowHostRequest::SpawnAgent { .. } | WorkflowHostRequest::BudgetQuery { .. } => {
                panic!("replay must not hit the host")
            }
            _ => {}
        });
        let second = run_workflow(params(script, Journal::load(journal_path).unwrap(), tx));
        drop(host);
        let WorkflowOutcome::Completed { result: second } = second else {
            panic!("resumed run should complete");
        };
        assert_eq!(first, second);
        assert_eq!(second["spent"], serde_json::json!(123));
        assert_eq!(second["reserved"], serde_json::json!(100));
        assert_eq!(second["remaining"], serde_json::json!(777));
    }

    #[test]
    fn journal_write_failure_is_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let journal_path = dir.path().join("journal.jsonl");
        std::fs::create_dir(&journal_path).unwrap();
        let (tx, rx) = mpsc::unbounded_channel();
        let host = spawn_mock_host(rx, |req| {
            if let WorkflowHostRequest::SpawnAgent { reply, .. } = req {
                let _ = reply.send(Ok(agent_result("unpersisted")));
            }
        });
        let outcome = run_workflow(params(
            r#"
            let meta = #{ name: "t", description: "d" };
            agent("work");
            complete("must not complete");
            "#,
            Journal::new(Some(journal_path)),
            tx,
        ));
        drop(host);
        match outcome {
            WorkflowOutcome::Failed { error } => {
                assert!(error.contains("journal io"), "got: {error}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn journal_divergence_fails_loudly() {
        let dir = tempfile::tempdir().unwrap();
        let journal_path = dir.path().join("journal.jsonl");

        let (tx, rx) = mpsc::unbounded_channel();
        let host = spawn_mock_host(rx, |req| {
            if let WorkflowHostRequest::SpawnAgent { reply, .. } = req {
                let _ = reply.send(Ok(agent_result("v1")));
            }
        });
        let first = run_workflow(params(
            r#"
            let meta = #{ name: "t", description: "d" };
            agent("original prompt");
            complete("ok");
            "#,
            Journal::new(Some(journal_path.clone())),
            tx,
        ));
        drop(host);
        assert!(matches!(first, WorkflowOutcome::Completed { .. }));

        let (tx, _rx) = mpsc::unbounded_channel();
        let second = run_workflow(params(
            r#"
            let meta = #{ name: "t", description: "d" };
            agent("EDITED prompt");
            complete("ok");
            "#,
            Journal::load(journal_path).unwrap(),
            tx,
        ));
        match second {
            WorkflowOutcome::Failed { error } => {
                assert!(error.contains("divergence"), "got: {error}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn phase_carries_replay_flag() {
        let script = r#"
            let meta = #{ name: "t", description: "d" };
            phase("One");
            agent("x");
            phase("Two");
            complete("ok");
        "#;
        let dir = tempfile::tempdir().unwrap();
        let journal_path = dir.path().join("journal.jsonl");

        let (tx, rx) = mpsc::unbounded_channel();
        let host = spawn_mock_host(rx, |req| {
            if let WorkflowHostRequest::SpawnAgent { reply, .. } = req {
                let _ = reply.send(Ok(agent_result("y")));
            }
        });
        let _ = run_workflow(params(script, Journal::new(Some(journal_path.clone())), tx));
        drop(host);

        let (tx, rx) = mpsc::unbounded_channel();
        let phases = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let phases_in_host = phases.clone();
        let host = spawn_mock_host(rx, move |req| {
            if let WorkflowHostRequest::Phase { title, replayed } = req {
                phases_in_host.lock().unwrap().push((title, replayed));
            }
        });
        let _ = run_workflow(params(script, Journal::load(journal_path).unwrap(), tx));
        host.join().unwrap();

        let phases = phases.lock().unwrap();
        assert_eq!(
            phases.as_slice(),
            &[("One".into(), true), ("Two".into(), false)]
        );
    }

    #[test]
    fn fingerprint_is_pure_and_stable() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let outcome = run_workflow(params(
            r#"
            let meta = #{ name: "t", description: "d" };
            complete(fingerprint("abc") == fingerprint("abc"));
            "#,
            Journal::new(None),
            tx,
        ));
        match outcome {
            WorkflowOutcome::Completed { result } => assert_eq!(result, serde_json::json!(true)),
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    fn json_encode_quotes_untrusted_strings() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let outcome = run_workflow(params(
            r#"
            let meta = #{ name: "t", description: "d" };
            complete(json_encode("</tag>\nquoted"));
            "#,
            Journal::new(None),
            tx,
        ));
        match outcome {
            WorkflowOutcome::Completed { result } => {
                assert_eq!(result, serde_json::json!("\"</tag>\\nquoted\""));
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }
}
