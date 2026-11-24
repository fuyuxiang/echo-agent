//! Interface-scoped DNS fallback for the public-only managed browser.

#[cfg(target_os = "macos")]
#[path = "macos_dns.rs"]
mod platform;
#[cfg(windows)]
#[path = "windows_dns.rs"]
mod platform;

use std::{
    future::Future,
    io,
    net::{IpAddr, SocketAddr},
    time::Duration,
};

const SYSTEM_LOOKUP_GRACE: Duration = Duration::from_secs(2);
const INTERFACE_LOOKUP_TIMEOUT: Duration = Duration::from_secs(4);

struct InterfaceResolution {
    interface: String,
    addresses: Vec<IpAddr>,
}

pub(super) fn is_public_dns_name(host: &str) -> bool {
    let host = host.trim_end_matches('.');
    let Some((_, suffix)) = host.rsplit_once('.') else {
        return false;
    };
    host.parse::<IpAddr>().is_err()
        && !["local", "localhost", "internal", "lan", "corp", "arpa"]
            .iter()
            .any(|reserved| suffix.eq_ignore_ascii_case(reserved))
}

pub(super) async fn lookup(host: &str, port: u16) -> Option<Vec<SocketAddr>> {
    tokio::time::sleep(SYSTEM_LOOKUP_GRACE).await;
    let resolution = tokio::time::timeout(
        INTERFACE_LOOKUP_TIMEOUT,
        platform::lookup_on_interface(host),
    )
    .await
    .ok()??;
    tracing::info!(
        host,
        interface = resolution.interface,
        "browser DNS used an interface resolver"
    );
    Some(
        resolution
            .addresses
            .into_iter()
            .map(|ip| SocketAddr::new(ip, port))
            .collect(),
    )
}

pub(super) async fn with_fallback(
    system: impl Future<Output = io::Result<Vec<SocketAddr>>>,
    interface: impl Future<Output = Option<Vec<SocketAddr>>>,
) -> io::Result<Vec<SocketAddr>> {
    tokio::pin!(system, interface);
    tokio::select! {
        biased;
        result = &mut system => match result {
            Err(error) if is_temporary_system_error(&error) => interface.await.ok_or(error),
            result => result,
        },
        addresses = &mut interface => match addresses {
            Some(addresses) => Ok(addresses),
            None => system.await,
        },
    }
}

fn is_temporary_system_error(error: &io::Error) -> bool {
    #[cfg(windows)]
    if error.raw_os_error() == Some(windows_sys::Win32::Networking::WinSock::WSATRY_AGAIN) {
        return true;
    }
    error.kind() == io::ErrorKind::TimedOut
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::{pending, ready};

    #[test]
    fn scoped_dns_uses_public_dns_names_only() {
        assert!(is_public_dns_name("www.baidu.com"));
        assert!(is_public_dns_name("example.com."));
        for host in [
            "localhost",
            "printer.LOCAL.",
            "router.home.arpa",
            "example.internal",
            "127.0.0.1",
            "::1",
        ] {
            assert!(!is_public_dns_name(host), "{host}");
        }
    }

    #[tokio::test]
    async fn interface_answers_recover_stalled_and_timed_out_system_queries() {
        let addresses = vec!["192.0.2.12:443".parse().unwrap()];
        assert_eq!(
            with_fallback(pending(), ready(Some(addresses.clone())))
                .await
                .unwrap(),
            addresses
        );
        let timeout = Err(io::ErrorKind::TimedOut.into());
        assert_eq!(
            with_fallback(ready(timeout), ready(Some(addresses.clone())))
                .await
                .unwrap(),
            addresses
        );
    }

    #[tokio::test]
    async fn system_answers_and_non_timeout_errors_take_precedence() {
        let addresses = vec!["192.0.2.12:443".parse().unwrap()];
        assert!(
            with_fallback(ready(Ok(Vec::new())), ready(Some(addresses.clone())))
                .await
                .unwrap()
                .is_empty()
        );
        let missing = Err(io::ErrorKind::NotFound.into());
        assert_eq!(
            with_fallback(ready(missing), ready(Some(addresses)))
                .await
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn winsock_temporary_dns_errors_use_interface_answers() {
        use windows_sys::Win32::Networking::WinSock::WSATRY_AGAIN;

        let addresses = vec!["192.0.2.12:443".parse().unwrap()];
        let retry = Err(io::Error::from_raw_os_error(WSATRY_AGAIN));
        assert_eq!(
            with_fallback(ready(retry), ready(Some(addresses.clone())))
                .await
                .unwrap(),
            addresses
        );
    }
}
