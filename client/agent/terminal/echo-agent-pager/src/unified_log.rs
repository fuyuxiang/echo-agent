//! Unified log forwarding for the pager.
//!
//! Buffers log entries in memory and flushes them to the shell via
//! `echo.agent/log` ACP notifications. Call [`init`] once at startup with
//! the ACP sender, then use [`info`], [`warn`], [`error`], [`debug`]
//! from anywhere.

use std::sync::{Mutex, OnceLock};

use agent_client_protocol as acp;
use echo_agent_acp::AcpAgentTx;
use echo_agent_telemetry::unified_log::{
    ClientLogEntry, LOG_METHOD, LogLevel, LogNotificationParams, LogSource,
};
use tokio::runtime::Handle;

static ACP_TX: OnceLock<AcpAgentTx> = OnceLock::new();
static BUFFER: Mutex<Vec<ClientLogEntry>> = Mutex::new(Vec::new());

/// Initialize the unified log forwarder with the ACP sender.
///
/// Must be called once after the ACP connection is established.
/// Spawns a background task that flushes buffered entries every few
/// seconds so events are delivered promptly without manual flush calls.
/// Entries buffered before this call will be picked up on the first tick.
pub fn init(tx: AcpAgentTx) {
    let _ = ACP_TX.set(tx);
    tokio::spawn(async {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            flush();
        }
    });
}

fn now_ts() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn make_entry(
    lvl: LogLevel,
    msg: &str,
    sid: Option<&str>,
    ctx: Option<serde_json::Value>,
) -> ClientLogEntry {
    ClientLogEntry {
        ts: now_ts(),
        pid: Some(std::process::id()),
        ver: Some(echo_agent_version::VERSION.to_owned()),
        lvl,
        sid: sid.map(Into::into),
        msg: msg.into(),
        ctx,
    }
}

/// Write an info entry straight to `unified.jsonl`, bypassing the ACP
/// forwarder. The forwarder only gets a sender after a successful connect
/// and a flush on a failed startup destroys buffered entries — so pre-connect
/// facts that must survive a failed startup go through here, with the same
/// pager source and pid stamping as forwarded entries.
pub fn write_direct_info(msg: &str, ctx: Option<serde_json::Value>) {
    echo_agent_telemetry::unified_log::ingest_client_entries(
        LogSource::EchoAgentPager,
        &[make_entry(LogLevel::Info, msg, None, ctx)],
    );
}

fn push_entry(lvl: LogLevel, msg: &str, sid: Option<&str>, ctx: Option<serde_json::Value>) {
    let entry = make_entry(lvl, msg, sid, ctx);
    if let Ok(mut buf) = BUFFER.lock() {
        buf.push(entry);
        // Auto-flush when we have a reasonable batch, but only if the ACP
        // sender is ready -- otherwise keep buffering until an explicit flush().
        if buf.len() >= 16 && ACP_TX.get().is_some() {
            let entries: Vec<ClientLogEntry> = buf.drain(..).collect();
            drop(buf);
            send_entries(entries);
        }
    }
}

fn build_notification(entries: Vec<ClientLogEntry>) -> Option<acp::ExtNotification> {
    if entries.is_empty() {
        return None;
    }
    let params = LogNotificationParams {
        src: LogSource::EchoAgentPager,
        entries,
    };
    let raw = serde_json::value::to_raw_value(&params).ok()?;
    Some(acp::ExtNotification::new(LOG_METHOD, raw.into()))
}

fn send_entries(entries: Vec<ClientLogEntry>) {
    let Some(tx) = ACP_TX.get() else { return };
    let Some(notification) = build_notification(entries) else {
        return;
    };
    // Guard against panic if called from a non-tokio thread (e.g., a
    // tracing::Layer callback on a blocking thread).
    let Ok(handle) = Handle::try_current() else {
        return;
    };
    let tx = tx.clone();
    handle.spawn(async move {
        let _ = echo_agent_acp::acp_send(notification, &tx).await;
    });
}

/// Flush any buffered entries to the shell (fire-and-forget).
pub fn flush() {
    let entries = {
        let Ok(mut buf) = BUFFER.lock() else { return };
        if buf.is_empty() {
            return;
        }
        buf.drain(..).collect::<Vec<_>>()
    };
    send_entries(entries);
}

/// Flush buffered entries and await delivery.
///
/// Use this before process exit to ensure entries are delivered
/// before the agent shuts down.
pub async fn flush_blocking() {
    let entries = {
        let Ok(mut buf) = BUFFER.lock() else { return };
        if buf.is_empty() {
            return;
        }
        buf.drain(..).collect::<Vec<_>>()
    };
    let Some(tx) = ACP_TX.get() else { return };
    let Some(notification) = build_notification(entries) else {
        return;
    };
    let _ = echo_agent_acp::acp_send(notification, tx).await;
}

/// Log an info-level entry.
pub fn info(msg: &str, sid: Option<&str>, ctx: Option<serde_json::Value>) {
    push_entry(LogLevel::Info, msg, sid, ctx);
}

/// Log a warn-level entry.
pub fn warn(msg: &str, sid: Option<&str>, ctx: Option<serde_json::Value>) {
    push_entry(LogLevel::Warn, msg, sid, ctx);
}

/// Log an error-level entry.
pub fn error(msg: &str, sid: Option<&str>, ctx: Option<serde_json::Value>) {
    push_entry(LogLevel::Error, msg, sid, ctx);
}

/// Log a debug-level entry.
pub fn debug(msg: &str, sid: Option<&str>, ctx: Option<serde_json::Value>) {
    push_entry(LogLevel::Debug, msg, sid, ctx);
}
