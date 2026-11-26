//! `ToolCallContext` extension store.

use std::sync::Arc;

use echo_agent_tool_protocol::ToolCallId;
use echo_agent_tool_runtime::{BehaviorVersion, Cwd, ToolCallContext, TraceContext};

#[derive(Debug, PartialEq)]
struct Config {
    base_url: String,
    timeout_ms: u32,
}

#[derive(Debug, PartialEq)]
struct AuthToken(String);

#[derive(Debug)]
struct Counter(std::sync::atomic::AtomicUsize);

impl Counter {
    fn bump(&self) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    fn get(&self) -> usize {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[test]
fn insert_then_get_returns_arc_of_same_value() {
    let mut ctx = ToolCallContext::default();
    ctx.extensions.insert(Config {
        base_url: "https://example".into(),
        timeout_ms: 5_000,
    });
    let cfg = ctx
        .extensions
        .get::<Config>()
        .expect("config must be present");
    assert_eq!(cfg.base_url, "https://example");
    assert_eq!(cfg.timeout_ms, 5_000);
}

#[test]
fn distinct_types_coexist() {
    let mut ctx = ToolCallContext::default();
    ctx.extensions.insert(Config {
        base_url: "u".into(),
        timeout_ms: 1,
    });
    ctx.extensions.insert(AuthToken("token".into()));
    assert!(ctx.extensions.contains::<Config>());
    assert!(ctx.extensions.contains::<AuthToken>());
    assert_eq!(ctx.extensions.len(), 2);
    assert_eq!(ctx.extensions.get::<AuthToken>().unwrap().0, "token");
}

#[test]
fn reinsert_same_type_replaces_value() {
    let mut ctx = ToolCallContext::default();
    ctx.extensions.insert(AuthToken("first".into()));
    ctx.extensions.insert(AuthToken("second".into()));
    assert_eq!(ctx.extensions.get::<AuthToken>().unwrap().0, "second");
    assert_eq!(ctx.extensions.len(), 1);
}

#[test]
fn missing_type_returns_none() {
    let ctx = ToolCallContext::default();
    assert!(ctx.extensions.get::<Config>().is_none());
    assert!(!ctx.extensions.contains::<Config>());
    assert_eq!(ctx.extensions.len(), 0);
}

#[test]
fn remove_returns_value_then_none() {
    let mut ctx = ToolCallContext::default();
    ctx.extensions.insert(Config {
        base_url: "u".into(),
        timeout_ms: 1,
    });
    let removed = ctx.extensions.remove::<Config>().expect("first remove");
    assert_eq!(removed.base_url, "u");
    assert!(ctx.extensions.remove::<Config>().is_none());
    assert!(!ctx.extensions.contains::<Config>());
}

#[test]
fn insert_arc_shares_allocation() {
    // Inserting an existing Arc means the stored value and the original
    // share strong-count.
    let arc = Arc::new(Config {
        base_url: "shared".into(),
        timeout_ms: 9,
    });
    let mut ctx = ToolCallContext::default();
    ctx.extensions.insert_arc(arc.clone());
    let from_ctx = ctx.extensions.get::<Config>().unwrap();
    // Strong-count on the original Arc should reflect at least:
    // - the original `arc` binding
    // - the value stored in the extension map
    // - the clone returned from `get`
    assert!(Arc::strong_count(&arc) >= 3);
    assert_eq!(*from_ctx, *arc);
}

#[test]
fn new_binds_to_specific_call_id() {
    let id = ToolCallId::new("call-123").unwrap();
    let ctx = ToolCallContext::new(id.clone());
    assert_eq!(ctx.call_id, id);
    assert_eq!(ctx.extensions.len(), 0);
}

#[tokio::test]
async fn context_can_cross_await_with_held_extension() {
    let mut ctx = ToolCallContext::default();
    ctx.extensions.insert(Counter(0.into()));
    let counter = ctx.extensions.get::<Counter>().unwrap();
    counter.bump();
    tokio::task::yield_now().await;
    counter.bump();
    assert_eq!(counter.get(), 2);
}

#[tokio::test]
async fn context_is_send_across_spawn() {
    let mut ctx = ToolCallContext::default();
    ctx.extensions.insert(AuthToken("for-task".into()));
    let handle =
        tokio::spawn(async move { ctx.extensions.get::<AuthToken>().map(|t| t.0.clone()) });
    let value = handle.await.unwrap();
    assert_eq!(value.as_deref(), Some("for-task"));
}

#[test]
fn default_constructor_yields_fresh_call_id() {
    let a = ToolCallContext::default();
    let b = ToolCallContext::default();
    assert_ne!(a.call_id, b.call_id, "default ids should be unique");
    assert_eq!(a.extensions.len(), 0);
    assert_eq!(b.extensions.len(), 0);
}

#[test]
fn clone_preserves_call_id_and_extensions() {
    let mut ctx = ToolCallContext::new(ToolCallId::new("call-clone").unwrap());
    ctx.extensions.insert(AuthToken("shared".into()));

    let copy = ctx.clone();
    assert_eq!(copy.call_id, ctx.call_id);
    assert_eq!(copy.extensions.len(), 1);

    // Both clones see the same Arc-backed extension value.
    let from_orig = ctx.extensions.get::<AuthToken>().unwrap();
    let from_copy = copy.extensions.get::<AuthToken>().unwrap();
    assert_eq!(from_orig.0, from_copy.0);
    // The Arc allocation is shared; mutating via one path is impossible
    // (extensions are immutable through `get`), but strong-count rises
    // because of the clone.
    assert!(Arc::strong_count(&from_orig) >= 3);
}

#[test]
fn clone_extension_map_is_independent_after_remove() {
    let mut ctx = ToolCallContext::default();
    ctx.extensions.insert(AuthToken("a".into()));
    let mut copy = ctx.clone();
    copy.extensions.remove::<AuthToken>();
    assert_eq!(copy.extensions.len(), 0);
    assert_eq!(
        ctx.extensions.len(),
        1,
        "removing from the clone must not affect the original"
    );
}

// ---------------------------------------------------------------------------
// Per-concept client/SDK-side extensions.
//
// These exist as separate extensions (one per concept) rather than a
// single bundle. The tests below pin three contracts:
//
//   1. Each extension round-trips through the typed-extension store
//      independently of the others.
//   2. A dispatcher with only some of the concepts can install them
//      individually — installing `Cwd` MUST NOT make `BehaviorVersion`
//      look "present" with a default value, and vice versa.
//   3. Absence of every well-known extension is the legitimate "backend
//      dispatcher" shape; tools that require one MUST treat absence as
//      a hard error rather than fall back to a process-wide default.
// ---------------------------------------------------------------------------

#[test]
fn each_well_known_extension_round_trips_independently() {
    let mut ctx = ToolCallContext::default();
    ctx.extensions.insert(Cwd(std::path::PathBuf::from("/tmp")));
    ctx.extensions.insert(BehaviorVersion("v1.0".into()));
    ctx.extensions
        .insert(TraceContext("traceparent: 00-...-00".into()));

    assert_eq!(
        ctx.extensions.get::<Cwd>().unwrap().0,
        std::path::PathBuf::from("/tmp")
    );
    assert_eq!(ctx.extensions.get::<BehaviorVersion>().unwrap().0, "v1.0");
    assert!(
        ctx.extensions
            .get::<TraceContext>()
            .unwrap()
            .0
            .contains("traceparent")
    );
    assert_eq!(ctx.extensions.len(), 3);
}

#[test]
fn dispatcher_can_install_only_what_it_has() {
    // A dispatcher that knows the cwd but not the trace context installs
    // only `Cwd`. The other extensions stay absent (not "default"),
    // which is the discriminator a tool can rely on.
    let mut ctx = ToolCallContext::default();
    ctx.extensions
        .insert(Cwd(std::path::PathBuf::from("/work")));

    assert!(ctx.extensions.contains::<Cwd>());
    assert!(!ctx.extensions.contains::<BehaviorVersion>());
    assert!(!ctx.extensions.contains::<TraceContext>());
    assert_eq!(ctx.extensions.len(), 1);

    // Adding `TraceContext` later does not implicitly conjure a
    // `BehaviorVersion` — extensions are independent.
    ctx.extensions.insert(TraceContext("tp".into()));
    assert!(ctx.extensions.contains::<TraceContext>());
    assert!(!ctx.extensions.contains::<BehaviorVersion>());
    assert_eq!(ctx.extensions.len(), 2);
}

#[test]
fn absence_signals_backend_or_other_mode() {
    // A backend dispatcher installs none of the client-side extensions.
    // Tools that require any of them must treat absence as a hard error
    // — this test pins the contract.
    let ctx = ToolCallContext::default();
    assert!(ctx.extensions.get::<Cwd>().is_none());
    assert!(ctx.extensions.get::<BehaviorVersion>().is_none());
    assert!(ctx.extensions.get::<TraceContext>().is_none());
    assert!(!ctx.extensions.contains::<Cwd>());
    assert!(!ctx.extensions.contains::<BehaviorVersion>());
    assert!(!ctx.extensions.contains::<TraceContext>());
}

#[test]
fn well_known_extensions_clone_preserves_inner_value() {
    let cwd = Cwd(std::path::PathBuf::from("/etc"));
    let behavior = BehaviorVersion("v0".into());
    let trace = TraceContext("tp".into());

    assert_eq!(cwd.clone().0, cwd.0);
    assert_eq!(behavior.clone().0, behavior.0);
    assert_eq!(trace.clone().0, trace.0);
}
