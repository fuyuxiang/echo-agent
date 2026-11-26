//! TLS policy for the echoagent CLI: OS roots, Mozilla roots, and opt-in extra
//! roots from `ECHO_AGENT_EXTRA_CA_BUNDLE` (fallback: `SSL_CERT_FILE`). A bad bundle
//! is logged and skipped, never failing client construction.
//!
//! Every client pins rustls: feature unification can otherwise select
//! native-tls, whose untyped errors break the certificate classifier.

use std::io::Read;
use std::sync::Arc;
use std::sync::OnceLock;

use rustls::RootCertStore;
use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject;

pub const MAX_EXTRA_CA_BUNDLE_BYTES: u64 = 1024 * 1024;

pub const ENV_ECHO_AGENT_EXTRA_CA_BUNDLE: &str = "ECHO_AGENT_EXTRA_CA_BUNDLE";

pub const ENV_SSL_CERT_FILE: &str = "SSL_CERT_FILE";

/// First install wins; without a default, `ClientConfig::builder()` panics
/// when `ring` and `aws-lc-rs` are both compiled in.
pub fn ensure_default_crypto_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        if rustls::crypto::aws_lc_rs::default_provider()
            .install_default()
            .is_err()
        {
            let supports_p521 = rustls::crypto::CryptoProvider::get_default().is_some_and(|p| {
                p.signature_verification_algorithms
                    .supported_schemes()
                    .contains(&rustls::SignatureScheme::ECDSA_NISTP521_SHA512)
            });
            if !supports_p521 {
                tracing::warn!(
                    "a rustls provider without ECDSA P-521 support was installed first; \
                     some enterprise proxy certificates will not verify"
                );
            }
        }
    });
}

/// Builds a reqwest client with the echoagent TLS policy: the shared roots (OS store,
/// Mozilla bundle, and any extra roots), read once per process instead of on
/// each build. For HTTP/1.1 only, add `http1_only()` in `configure`.
#[allow(clippy::disallowed_methods)] // the approved async build path
pub fn build_reqwest_client(
    configure: impl Fn(reqwest::ClientBuilder) -> reqwest::ClientBuilder,
) -> reqwest::Result<reqwest::Client> {
    ensure_default_crypto_provider();
    let mut builder = configure(reqwest::Client::builder())
        .use_rustls_tls()
        .tls_built_in_native_certs(false)
        .tls_built_in_webpki_certs(true);
    for cert in shared_reqwest_roots() {
        builder = builder.add_root_certificate(cert);
    }
    builder.build()
}

/// [`build_reqwest_client`] for the blocking client type.
#[allow(clippy::disallowed_methods)] // the approved blocking build path
pub fn build_blocking_reqwest_client(
    configure: impl Fn(reqwest::blocking::ClientBuilder) -> reqwest::blocking::ClientBuilder,
) -> reqwest::Result<reqwest::blocking::Client> {
    ensure_default_crypto_provider();
    let mut builder = configure(reqwest::blocking::Client::builder())
        .use_rustls_tls()
        .tls_built_in_native_certs(false)
        .tls_built_in_webpki_certs(true);
    for cert in shared_reqwest_roots() {
        builder = builder.add_root_certificate(cert);
    }
    builder.build()
}

#[cfg(test)]
static NATIVE_ROOT_LOADS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The OS store and extra roots as reqwest certificates, parsed once per
/// process. Mozilla roots come from reqwest's built-in webpki bundle.
fn shared_reqwest_roots() -> impl Iterator<Item = reqwest::Certificate> {
    static ROOTS: OnceLock<Vec<reqwest::Certificate>> = OnceLock::new();
    ROOTS
        .get_or_init(|| {
            cached_native_der()
                .iter()
                .map(|der| &der[..])
                .chain(extra_root_ders().iter().map(|der| &der[..]))
                .filter_map(|der| {
                    reqwest::Certificate::from_der(der)
                        .inspect_err(|error| {
                            tracing::warn!(error = %error, "root rejected by reqwest; skipping")
                        })
                        .ok()
                })
                .collect()
        })
        .iter()
        .cloned()
}

/// The OS trust store certificates, read once per process.
fn cached_native_der() -> &'static [CertificateDer<'static>] {
    static CERTS: OnceLock<Vec<CertificateDer<'static>>> = OnceLock::new();
    CERTS.get_or_init(|| {
        #[cfg(test)]
        NATIVE_ROOT_LOADS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let native = rustls_native_certs::load_native_certs();
        if !native.errors.is_empty() {
            tracing::warn!(
                native_root_error_count = native.errors.len(),
                "skipping unreadable native root certificates"
            );
        }
        // Keep only certificates rustls accepts, so one unparsable OS root
        // cannot fail every client build.
        native
            .certs
            .into_iter()
            .filter(|der| {
                let mut probe = RootCertStore::empty();
                probe.add(der.clone()).is_ok()
            })
            .collect()
    })
}

/// A rustls config over the process-wide roots: OS store, Mozilla bundle, extra.
fn client_config_with_shared_roots() -> rustls::ClientConfig {
    ensure_default_crypto_provider();
    let mut roots = RootCertStore::empty();
    roots.add_parsable_certificates(cached_native_der().iter().cloned());
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    roots.add_parsable_certificates(extra_root_ders().iter().cloned().map(CertificateDer::from));
    #[expect(clippy::expect_used)]
    rustls::ClientConfig::builder_with_provider(
        rustls::crypto::aws_lc_rs::default_provider().into(),
    )
    .with_safe_default_protocol_versions()
    .expect("aws-lc-rs supports the default protocol versions")
    .with_root_certificates(roots)
    .with_no_client_auth()
}

/// Shared rustls config for TLS outside reqwest (WebSocket, HTTP/1.1 upgrade),
/// pinned to this crate's provider.
pub fn rustls_client_config() -> Arc<rustls::ClientConfig> {
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let mut config = client_config_with_shared_roots();
            config.alpn_protocols = vec![b"http/1.1".to_vec()];
            Arc::new(config)
        })
        .clone()
}

/// The configured extra roots as validated DER, loaded once per process.
pub fn extra_root_ders() -> &'static [Vec<u8>] {
    bundle_snapshot().ders.as_slice()
}

/// The variable that fed the loaded roots, if any.
pub fn configured_bundle_env() -> Option<&'static str> {
    bundle_snapshot().source
}

/// Source and roots, computed together so they cannot diverge.
struct BundleSnapshot {
    source: Option<&'static str>,
    ders: Vec<Vec<u8>>,
}

fn bundle_snapshot() -> &'static BundleSnapshot {
    static SNAPSHOT: OnceLock<BundleSnapshot> = OnceLock::new();
    SNAPSHOT.get_or_init(|| match configured_ca_bundle() {
        Some((source, path)) => BundleSnapshot {
            source: Some(source),
            ders: load_extra_root_ders(source, &path),
        },
        None => BundleSnapshot {
            source: None,
            ders: Vec::new(),
        },
    })
}

/// `ECHO_AGENT_EXTRA_CA_BUNDLE` wins over `SSL_CERT_FILE`; an empty value disables
/// both, since `SSL_CERT_FILE` is often set process-wide (Nix, conda).
fn configured_ca_bundle() -> Option<(&'static str, std::path::PathBuf)> {
    select_bundle(
        std::env::var_os(ENV_ECHO_AGENT_EXTRA_CA_BUNDLE),
        std::env::var_os(ENV_SSL_CERT_FILE),
    )
}

/// `ECHO_AGENT_EXTRA_CA_BUNDLE` wins over `SSL_CERT_FILE`; an empty value disables
/// both, since `SSL_CERT_FILE` is often set process-wide (Nix, conda). Pure so
/// precedence is unit-tested without touching the process environment.
fn select_bundle(
    bundle: Option<std::ffi::OsString>,
    ssl: Option<std::ffi::OsString>,
) -> Option<(&'static str, std::path::PathBuf)> {
    match bundle {
        Some(p) if !p.is_empty() => Some((ENV_ECHO_AGENT_EXTRA_CA_BUNDLE, p.into())),
        Some(_) => None,
        None => match ssl {
            Some(p) if !p.is_empty() => Some((ENV_SSL_CERT_FILE, p.into())),
            _ => None,
        },
    }
}

fn load_extra_root_ders(source: &'static str, path: &std::path::Path) -> Vec<Vec<u8>> {
    let bytes = match read_bundle_capped(path) {
        Ok(b) => b,
        Err(BundleReadError::Io(e)) => {
            tracing::warn!(
                source,
                path = %path.display(),
                error = %e,
                "extra CA bundle unreadable; continuing without extra roots"
            );
            return Vec::new();
        }
        Err(BundleReadError::TooLarge) => {
            tracing::warn!(
                source,
                path = %path.display(),
                max_bytes = MAX_EXTRA_CA_BUNDLE_BYTES,
                "extra CA bundle exceeds size cap; continuing without extra roots"
            );
            return Vec::new();
        }
    };

    let outcome = parse_and_validate_pem(&bytes);
    if outcome.no_pem_blocks {
        tracing::warn!(
            source,
            path = %path.display(),
            "extra CA bundle contains no PEM certificate blocks; continuing without extra roots"
        );
        return outcome.accepted;
    }
    if outcome.rejected > 0 {
        tracing::warn!(
            source,
            path = %path.display(),
            accepted = outcome.accepted.len(),
            rejected = outcome.rejected,
            "extra CA bundle: dropped unusable certificate block(s)"
        );
    }
    if outcome.accepted.is_empty() {
        tracing::warn!(
            source,
            path = %path.display(),
            "extra CA bundle produced zero usable certificates; continuing without extra roots"
        );
    } else {
        tracing::info!(
            source,
            path = %path.display(),
            accepted = outcome.accepted.len(),
            "extra CA bundle: loaded extra root certificate(s)"
        );
    }
    outcome.accepted
}

#[derive(Debug)]
enum BundleReadError {
    Io(std::io::Error),
    TooLarge,
}

fn read_bundle_capped(path: &std::path::Path) -> Result<Vec<u8>, BundleReadError> {
    let file = std::fs::File::open(path).map_err(BundleReadError::Io)?;
    let mut buf = Vec::new();
    let n = file
        .take(MAX_EXTRA_CA_BUNDLE_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(BundleReadError::Io)?;
    if (n as u64) > MAX_EXTRA_CA_BUNDLE_BYTES {
        return Err(BundleReadError::TooLarge);
    }
    Ok(buf)
}

#[derive(Debug, Default)]
pub(crate) struct ParseOutcome {
    pub(crate) accepted: Vec<Vec<u8>>,
    /// PEM blocks that failed decode or rustls X.509 validation.
    pub(crate) rejected: usize,
    /// Input (non-empty) contained no PEM certificate blocks at all.
    pub(crate) no_pem_blocks: bool,
}

/// Parses PEM into validated DER, with no environment or cache involved.
pub(crate) fn parse_and_validate_pem(pem: &[u8]) -> ParseOutcome {
    let mut accepted = Vec::new();
    let mut rejected = 0usize;
    let mut saw_block = false;

    let pem = normalize_trusted_certificate_labels(pem);
    // `add` validates each certificate as X.509; the store itself is discarded.
    let mut store = RootCertStore::empty();
    for item in CertificateDer::pem_slice_iter(&pem) {
        saw_block = true;
        match item {
            Ok(der) => {
                let bytes = der.as_ref();
                let cert = first_der_item(bytes).unwrap_or(bytes);
                match store.add(CertificateDer::from(cert.to_vec())) {
                    Ok(()) => accepted.push(cert.to_vec()),
                    Err(_) => rejected += 1,
                }
            }
            Err(_) => rejected += 1,
        }
    }

    ParseOutcome {
        accepted,
        rejected,
        no_pem_blocks: !saw_block,
    }
}

/// Relabel OpenSSL `TRUSTED CERTIFICATE` blocks the PEM parser skips (rustls/pemfile#52).
fn normalize_trusted_certificate_labels(pem: &[u8]) -> Vec<u8> {
    String::from_utf8_lossy(pem)
        .replace("BEGIN TRUSTED CERTIFICATE", "BEGIN CERTIFICATE")
        .replace("END TRUSTED CERTIFICATE", "END CERTIFICATE")
        .into_bytes()
}

/// The first DER object as a prefix of `der` (drops trailing bytes), or `None`
/// if the header is malformed, over-long, or an unsupported length form.
fn first_der_item(der: &[u8]) -> Option<&[u8]> {
    if der.first() != Some(&0x30) {
        return None;
    }
    let (len, header) = match *der.get(1)? {
        n @ 0x00..=0x7f => (usize::from(n), 2usize),
        n @ 0x81..=0x84 => {
            let octets = usize::from(n - 0x80);
            let mut len = 0usize;
            for i in 0..octets {
                len = (len << 8) | usize::from(*der.get(2 + i)?);
            }
            (len, 2 + octets)
        }
        _ => return None,
    };
    der.get(..header.checked_add(len)?)
}

#[allow(clippy::disallowed_methods)] // tests exercise the adapter's build seam directly
#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
