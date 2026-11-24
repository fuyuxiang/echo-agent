//! Production logging that survives the Windows GUI subsystem's hidden console.

use std::path::{Path, PathBuf};

use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

const LOG_RETENTION_DAYS: u64 = 14;

pub(crate) fn log_dir() -> PathBuf {
    if let Some(home) = crate::paths::redirected_data_home() {
        return home.join("logs");
    }
    dirs::data_local_dir()
        .unwrap_or_else(crate::paths::echo_agent_home_dir)
        .join("EchoAgent")
        .join("logs")
}

fn remove_expired_logs(directory: &Path) {
    let cutoff = std::time::SystemTime::now().checked_sub(std::time::Duration::from_secs(
        60 * 60 * 24 * LOG_RETENTION_DAYS,
    ));
    let Some(cutoff) = cutoff else { return };
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_echoagent_log = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("echoagent.log"));
        if !is_echoagent_log {
            continue;
        }
        let expired = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .is_ok_and(|modified| modified < cutoff);
        if expired {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Install a daily rolling file subscriber. The returned guard must live for
/// the entire process so the non-blocking writer can flush queued records.
pub(crate) fn init() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    init_at(&log_dir())
}

fn init_at(directory: &Path) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    if std::fs::create_dir_all(directory).is_err() {
        return None;
    }
    remove_expired_logs(directory);

    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("echoagent.log")
        .build(directory)
        .map_err(|error| eprintln!("无法初始化 EchoAgent 文件日志：{error}"))
        .ok()?;
    let (file_writer, guard) = tracing_appender::non_blocking(appender);
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new("echoagent=info,echoagent_lib=info,echo_agent_runtime=info,warn")
    });
    let subscriber = tracing_subscriber::registry().with(filter).with(
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_target(true)
            .with_writer(file_writer),
    );

    #[cfg(debug_assertions)]
    let initialized = subscriber
        .with(tracing_subscriber::fmt::layer().with_target(true))
        .try_init();
    #[cfg(not(debug_assertions))]
    let initialized = subscriber.try_init();

    if initialized.is_err() {
        return None;
    }
    Some(guard)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_file_open_failure_does_not_panic() {
        let directory = tempfile::tempdir().unwrap();
        let today = chrono::Utc::now().date_naive();
        for date in [today, today.succ_opt().unwrap()] {
            std::fs::create_dir(directory.path().join(format!("echoagent.log.{date}"))).unwrap();
        }

        assert!(init_at(directory.path()).is_none());
    }
}
