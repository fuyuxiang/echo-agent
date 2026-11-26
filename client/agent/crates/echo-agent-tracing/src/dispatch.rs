use tracing::subscriber::NoSubscriber;

/// Returns `true` when a `tracing` dispatcher (subscriber) is active in the
/// current context — either the thread-scoped default
/// (`tracing::subscriber::with_default` / `set_default`) or the global one.
///
/// When this returns `false`, spans have no consumer. Worse than useless: when
/// `tracing` is compiled with its `log` compatibility feature, every span
/// creation and every later `Span::record(...)` is downgraded to a `log`
/// record at the span's level. In processes that only configure a `log`
/// logger — e.g. integration tests or fastrace-only binaries — that prints
/// noise like:
///
/// ```text
/// I grpc; otel.name="POST_/pkg.Service/Method" ...
/// I grpc; status_code=200 OK
/// I grpc; trace_id=00000000000000000000000000000000
/// ```
///
/// Request-span factories (gRPC/HTTP server and client middleware) call this
/// and return `Span::none()` when no dispatcher is active, so the span is
/// neither built nor downgraded to log spam.
pub fn dispatcher_active() -> bool {
    tracing::dispatcher::get_default(|dispatch| !dispatch.is::<NoSubscriber>())
}

#[cfg(test)]
mod tests {
    use super::*;

    // NOTE: relies on no test in this binary installing a *global* subscriber
    // (`OtelTestEnv` and friends use thread-scoped `set_default` guards).
    #[test]
    fn without_dispatcher_inactive() {
        assert!(!dispatcher_active());
    }

    #[test]
    fn scoped_dispatcher_active() {
        tracing::subscriber::with_default(tracing_subscriber::registry(), || {
            assert!(dispatcher_active());
        });
        assert!(!dispatcher_active());
    }
}
