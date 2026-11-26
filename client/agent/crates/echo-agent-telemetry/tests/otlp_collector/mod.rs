//! Shared in-process OTLP collector + decode helpers for the external-stream
//! wire tests. Each integration-test binary that needs a collector does
//! `mod otlp_collector;` and uses these.
//!
//! The collector runs on its own thread with its own current-thread runtime.
#![allow(dead_code)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, Once};

/// Install a process-wide test tracing subscriber so construction/export
/// failures surface under `--test_output=errors` without production
/// `eprintln!` side effects.
pub fn init_test_tracing() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
            )
            .with_test_writer()
            .try_init();
    });
}

use prost::Message as _;

use opentelemetry_proto::tonic::collector::logs::v1::logs_service_server::LogsService;
use opentelemetry_proto::tonic::collector::logs::v1::{
    ExportLogsServiceRequest, ExportLogsServiceResponse,
};
use opentelemetry_proto::tonic::collector::metrics::v1::metrics_service_server::MetricsService;
use opentelemetry_proto::tonic::collector::metrics::v1::{
    ExportMetricsServiceRequest, ExportMetricsServiceResponse,
};

/// Delta / Cumulative aggregation-temporality enum values (OTLP metrics v1).
pub const TEMPORALITY_DELTA: i32 = 1;
pub const TEMPORALITY_CUMULATIVE: i32 = 2;

#[derive(Clone, Debug, Default)]
pub struct Collected {
    pub logs: Arc<Mutex<Vec<Vec<u8>>>>,
    pub metrics: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl Collected {
    pub fn logs_len(&self) -> usize {
        self.logs.lock().unwrap().len()
    }
    pub fn metrics_len(&self) -> usize {
        self.metrics.lock().unwrap().len()
    }
    pub fn raw_logs(&self) -> Vec<u8> {
        self.logs.lock().unwrap().concat()
    }
    pub fn raw_metrics(&self) -> Vec<u8> {
        self.metrics.lock().unwrap().concat()
    }
    /// Combined raw bytes of both signals, lossy-decoded to a string — for
    /// canary/leak scans at the HTTP layer.
    pub fn raw_text(&self) -> String {
        let mut bytes = self.raw_logs();
        bytes.extend(self.raw_metrics());
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

#[derive(Clone, Copy, Debug)]
pub enum CollectorProtocol {
    HttpProtobuf,
    Grpc,
}

#[derive(Clone, Debug)]
struct GrpcCollector {
    collected: Collected,
}

#[async_trait::async_trait]
impl LogsService for GrpcCollector {
    async fn export(
        &self,
        request: tonic::Request<ExportLogsServiceRequest>,
    ) -> Result<tonic::Response<ExportLogsServiceResponse>, tonic::Status> {
        let mut body = Vec::new();
        request
            .into_inner()
            .encode(&mut body)
            .expect("encode gRPC logs request");
        self.collected.logs.lock().unwrap().push(body);
        Ok(tonic::Response::new(ExportLogsServiceResponse::default()))
    }
}

#[async_trait::async_trait]
impl MetricsService for GrpcCollector {
    async fn export(
        &self,
        request: tonic::Request<ExportMetricsServiceRequest>,
    ) -> Result<tonic::Response<ExportMetricsServiceResponse>, tonic::Status> {
        let mut body = Vec::new();
        request
            .into_inner()
            .encode(&mut body)
            .expect("encode gRPC metrics request");
        self.collected.metrics.lock().unwrap().push(body);
        Ok(tonic::Response::new(ExportMetricsServiceResponse::default()))
    }
}

/// Start an HTTP/protobuf collector; returns its base URL
/// (`http://127.0.0.1:PORT`).
pub fn start_collector(collected: Collected) -> String {
    start_collector_with_protocol(collected, CollectorProtocol::HttpProtobuf)
}

/// Start the collector for the requested OTLP transport; returns its base URL
/// (`http://127.0.0.1:PORT`).
pub fn start_collector_with_protocol(collected: Collected, protocol: CollectorProtocol) -> String {
    let (addr_tx, addr_rx) = std::sync::mpsc::channel::<SocketAddr>();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("collector runtime");
        rt.block_on(async move {
            match protocol {
                CollectorProtocol::HttpProtobuf => start_http_collector(collected, addr_tx).await,
                CollectorProtocol::Grpc => start_grpc_collector(collected, addr_tx).await,
            }
        });
    });
    let addr = addr_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("collector must start");
    format!("http://{addr}")
}

async fn start_http_collector(collected: Collected, addr_tx: std::sync::mpsc::Sender<SocketAddr>) {
    use axum::{Router, body::Bytes, extract::State, routing::post};
    async fn sink(
        State((store, which)): State<(Collected, &'static str)>,
        body: Bytes,
    ) -> &'static str {
        let target = match which {
            "logs" => &store.logs,
            _ => &store.metrics,
        };
        target.lock().unwrap().push(body.to_vec());
        ""
    }
    let app = Router::new()
        .route(
            "/v1/logs",
            post(sink).with_state((collected.clone(), "logs")),
        )
        .route(
            "/v1/metrics",
            post(sink).with_state((collected.clone(), "metrics")),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind collector");
    addr_tx
        .send(listener.local_addr().expect("collector addr"))
        .expect("send addr");
    axum::serve(listener, app).await.expect("collector serve");
}

async fn start_grpc_collector(collected: Collected, addr_tx: std::sync::mpsc::Sender<SocketAddr>) {
    use opentelemetry_proto::tonic::collector::logs::v1::logs_service_server::LogsServiceServer;
    use opentelemetry_proto::tonic::collector::metrics::v1::metrics_service_server::MetricsServiceServer;

    let incoming = tonic::transport::server::TcpIncoming::bind(
        "127.0.0.1:0".parse().expect("collector bind addr"),
    )
    .expect("bind gRPC collector");
    addr_tx
        .send(incoming.local_addr().expect("collector addr"))
        .expect("send addr");
    let service = GrpcCollector { collected };
    tonic::transport::Server::builder()
        .add_service(LogsServiceServer::new(service.clone()))
        .add_service(MetricsServiceServer::new(service))
        .serve_with_incoming(incoming)
        .await
        .expect("collector serve");
}

pub struct TestTlsMaterial {
    pub ca_cert_pem: String,
    pub server_cert_pem: String,
    pub server_key_pem: String,
    pub client_cert_pem: String,
    pub client_key_pem: String,
}

pub fn generate_tls_material() -> TestTlsMaterial {
    use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair};

    let ca_key = KeyPair::generate().expect("generate CA key");
    let mut ca_params = CertificateParams::new(Vec::new()).expect("CA params");
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca_cert = ca_params.self_signed(&ca_key).expect("self-sign CA");

    let server_key = KeyPair::generate().expect("generate server key");
    let server_params =
        CertificateParams::new(vec!["localhost".to_string(), "127.0.0.1".to_string()])
            .expect("server params");
    let server_cert = server_params
        .signed_by(&server_key, &ca_cert, &ca_key)
        .expect("sign server cert");

    let client_key = KeyPair::generate().expect("generate client key");
    let client_params =
        CertificateParams::new(vec!["echo-agent-client".to_string()]).expect("client params");
    let client_cert = client_params
        .signed_by(&client_key, &ca_cert, &ca_key)
        .expect("sign client cert");

    TestTlsMaterial {
        ca_cert_pem: ca_cert.pem(),
        server_cert_pem: server_cert.pem(),
        server_key_pem: server_key.serialize_pem(),
        client_cert_pem: client_cert.pem(),
        client_key_pem: client_key.serialize_pem(),
    }
}

pub fn start_grpc_tls_collector(
    collected: Collected,
    server_cert_pem: String,
    server_key_pem: String,
) -> String {
    start_grpc_tls_collector_inner(collected, server_cert_pem, server_key_pem, None)
}

pub fn start_grpc_mtls_collector(
    collected: Collected,
    server_cert_pem: String,
    server_key_pem: String,
    client_ca_pem: String,
) -> String {
    start_grpc_tls_collector_inner(
        collected,
        server_cert_pem,
        server_key_pem,
        Some(client_ca_pem),
    )
}

fn start_grpc_tls_collector_inner(
    collected: Collected,
    server_cert_pem: String,
    server_key_pem: String,
    client_ca_pem: Option<String>,
) -> String {
    use opentelemetry_proto::tonic::collector::logs::v1::logs_service_server::LogsServiceServer;
    use opentelemetry_proto::tonic::collector::metrics::v1::metrics_service_server::MetricsServiceServer;

    // The test binary links both ring and aws-lc-rs, so rustls cannot pick a
    // process default on its own; the server-side acceptor needs one pinned.
    // (The production client is unaffected: tonic passes a provider
    // explicitly.)
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let (addr_tx, addr_rx) = std::sync::mpsc::channel::<SocketAddr>();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("collector runtime");
        rt.block_on(async move {
            let incoming = tonic::transport::server::TcpIncoming::bind(
                "127.0.0.1:0".parse().expect("collector bind addr"),
            )
            .expect("bind gRPC TLS collector");
            addr_tx
                .send(incoming.local_addr().expect("collector addr"))
                .expect("send addr");
            let identity = tonic::transport::Identity::from_pem(server_cert_pem, server_key_pem);
            let mut tls = tonic::transport::ServerTlsConfig::new().identity(identity);
            if let Some(ca_pem) = client_ca_pem {
                tls = tls.client_ca_root(tonic::transport::Certificate::from_pem(ca_pem));
            }
            let service = GrpcCollector { collected };
            tonic::transport::Server::builder()
                .tls_config(tls)
                .expect("collector TLS config")
                .add_service(LogsServiceServer::new(service.clone()))
                .add_service(MetricsServiceServer::new(service))
                .serve_with_incoming(incoming)
                .await
                .expect("collector serve");
        });
    });
    let addr = addr_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("collector must start");
    format!("https://localhost:{}", addr.port())
}

pub fn start_http_mtls_collector(
    collected: Collected,
    server_cert_pem: String,
    server_key_pem: String,
    client_ca_pem: String,
) -> String {
    use axum::{Router, body::Bytes, extract::State, routing::post};
    use axum_server::tls_rustls::RustlsConfig;
    use rustls::RootCertStore;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
    use rustls::server::WebPkiClientVerifier;
    use std::sync::Arc;

    // Same process-level CryptoProvider pin as gRPC mTLS tests: the binary
    // links both ring and aws-lc-rs, so rustls will not auto-pick. Prefer
    // aws-lc to match the workspace `rustls` feature set and tonic path.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let (addr_tx, addr_rx) = std::sync::mpsc::channel::<SocketAddr>();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("collector runtime");
        rt.block_on(async move {
            async fn sink(
                State((store, which)): State<(Collected, &'static str)>,
                body: Bytes,
            ) -> &'static str {
                let target = match which {
                    "logs" => &store.logs,
                    _ => &store.metrics,
                };
                target.lock().unwrap().push(body.to_vec());
                ""
            }
            let app = Router::new()
                .route(
                    "/v1/logs",
                    post(sink).with_state((collected.clone(), "logs")),
                )
                .route(
                    "/v1/metrics",
                    post(sink).with_state((collected.clone(), "metrics")),
                );

            let certs: Vec<CertificateDer<'static>> =
                CertificateDer::pem_slice_iter(server_cert_pem.as_bytes())
                    .collect::<Result<_, _>>()
                    .expect("parse server cert pem");
            let key = PrivateKeyDer::from_pem_slice(server_key_pem.as_bytes())
                .expect("parse server key pem");

            let mut roots = RootCertStore::empty();
            for ca in CertificateDer::pem_slice_iter(client_ca_pem.as_bytes()) {
                roots
                    .add(ca.expect("parse client CA"))
                    .expect("add client CA");
            }
            let verifier = WebPkiClientVerifier::builder(Arc::new(roots))
                .build()
                .expect("client cert verifier");
            let mut server_config = rustls::ServerConfig::builder()
                .with_client_cert_verifier(verifier)
                .with_single_cert(certs, key)
                .expect("server TLS config");
            server_config.alpn_protocols = vec![b"http/1.1".to_vec()];

            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind HTTP mTLS");
            listener.set_nonblocking(true).expect("nonblocking");
            addr_tx
                .send(listener.local_addr().expect("local addr"))
                .expect("send addr");
            let config = RustlsConfig::from_config(Arc::new(server_config));
            axum_server::from_tcp_rustls(listener, config)
                .expect("tls server")
                .serve(app.into_make_service())
                .await
                .expect("HTTP mTLS collector serve");
        });
    });
    let addr = addr_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("collector must start");
    // Prefer localhost so the server cert SAN (localhost + 127.0.0.1) matches
    // whatever the client/rustls hostname check uses.
    format!("https://localhost:{}", addr.port())
}

pub fn decode_logs(
    collected: &Collected,
) -> Vec<opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest> {
    collected
        .logs
        .lock()
        .unwrap()
        .iter()
        .map(|body| {
            opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest::decode(
                body.as_slice(),
            )
            .expect("valid logs protobuf")
        })
        .collect()
}

pub fn decode_metrics(
    collected: &Collected,
) -> Vec<opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest> {
    collected
        .metrics
        .lock()
        .unwrap()
        .iter()
        .map(|body| {
            opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest::decode(
                body.as_slice(),
            )
            .expect("valid metrics protobuf")
        })
        .collect()
}

/// Poll `check` until it is true or `deadline` elapses.
pub fn wait_until(deadline: std::time::Duration, mut check: impl FnMut() -> bool) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < deadline {
        if check() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    check()
}

// ── Decoding ────────────────────────────────────────────────────────────────

/// Flattened attribute value (the external schema is flat: no arrays/maps).
#[derive(Debug, Clone, PartialEq)]
pub enum AttrVal {
    S(String),
    I(i64),
    B(bool),
    D(f64),
    Other,
}

impl AttrVal {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            AttrVal::S(s) => Some(s.as_str()),
            _ => None,
        }
    }
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            AttrVal::I(i) => Some(*i),
            _ => None,
        }
    }
}

fn anyval(v: &opentelemetry_proto::tonic::common::v1::AnyValue) -> AttrVal {
    use opentelemetry_proto::tonic::common::v1::any_value::Value;
    match &v.value {
        Some(Value::StringValue(s)) => AttrVal::S(s.clone()),
        Some(Value::IntValue(i)) => AttrVal::I(*i),
        Some(Value::BoolValue(b)) => AttrVal::B(*b),
        Some(Value::DoubleValue(d)) => AttrVal::D(*d),
        _ => AttrVal::Other,
    }
}

fn kvs_to_map(
    attrs: &[opentelemetry_proto::tonic::common::v1::KeyValue],
) -> HashMap<String, AttrVal> {
    attrs
        .iter()
        .filter_map(|kv| kv.value.as_ref().map(|v| (kv.key.clone(), anyval(v))))
        .collect()
}

/// One decoded external log record.
#[derive(Debug, Clone)]
pub struct RecordView {
    pub event_name: String,
    pub attrs: HashMap<String, AttrVal>,
    /// `service.name`, `echo_agent_code.schema.version`, … from the owning resource.
    pub resource: HashMap<String, AttrVal>,
    pub scope_name: String,
    pub has_body: bool,
}

pub fn log_records(c: &Collected) -> Vec<RecordView> {
    use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
    let mut out = Vec::new();
    for body in c.logs.lock().unwrap().iter() {
        let req = ExportLogsServiceRequest::decode(body.as_slice()).expect("valid logs protobuf");
        for rl in &req.resource_logs {
            let resource = rl
                .resource
                .as_ref()
                .map(|r| kvs_to_map(&r.attributes))
                .unwrap_or_default();
            for sl in &rl.scope_logs {
                let scope_name = sl
                    .scope
                    .as_ref()
                    .map(|s| s.name.clone())
                    .unwrap_or_default();
                for r in &sl.log_records {
                    out.push(RecordView {
                        event_name: r.event_name.clone(),
                        attrs: kvs_to_map(&r.attributes),
                        resource: resource.clone(),
                        scope_name: scope_name.clone(),
                        has_body: r.body.is_some(),
                    });
                }
            }
        }
    }
    out
}

/// One decoded metric data point (sums only — the external schema is all
/// monotonic counters).
#[derive(Debug, Clone)]
pub struct MetricPoint {
    pub name: String,
    pub temporality: i32,
    pub is_monotonic: bool,
    pub attrs: HashMap<String, AttrVal>,
    pub int_value: i64,
    pub scope_name: String,
}

pub fn metric_points(c: &Collected) -> Vec<MetricPoint> {
    use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
    use opentelemetry_proto::tonic::metrics::v1::metric::Data;
    use opentelemetry_proto::tonic::metrics::v1::number_data_point::Value;
    let mut out = Vec::new();
    for body in c.metrics.lock().unwrap().iter() {
        let req =
            ExportMetricsServiceRequest::decode(body.as_slice()).expect("valid metrics protobuf");
        for rm in &req.resource_metrics {
            for sm in &rm.scope_metrics {
                let scope_name = sm
                    .scope
                    .as_ref()
                    .map(|s| s.name.clone())
                    .unwrap_or_default();
                for metric in &sm.metrics {
                    if let Some(Data::Sum(sum)) = &metric.data {
                        for dp in &sum.data_points {
                            let int_value = match dp.value {
                                Some(Value::AsInt(i)) => i,
                                Some(Value::AsDouble(d)) => d as i64,
                                None => 0,
                            };
                            out.push(MetricPoint {
                                name: metric.name.clone(),
                                temporality: sum.aggregation_temporality,
                                is_monotonic: sum.is_monotonic,
                                attrs: kvs_to_map(&dp.attributes),
                                int_value,
                                scope_name: scope_name.clone(),
                            });
                        }
                    }
                }
            }
        }
    }
    out
}

/// All event names seen across the decoded log records.
pub fn event_names(c: &Collected) -> Vec<String> {
    log_records(c).into_iter().map(|r| r.event_name).collect()
}

/// First record matching `event_name`, if any.
pub fn find_event(c: &Collected, event_name: &str) -> Option<RecordView> {
    log_records(c)
        .into_iter()
        .find(|r| r.event_name == event_name)
}

/// All metric points for a given metric name.
pub fn find_metric(c: &Collected, name: &str) -> Vec<MetricPoint> {
    metric_points(c)
        .into_iter()
        .filter(|m| m.name == name)
        .collect()
}
