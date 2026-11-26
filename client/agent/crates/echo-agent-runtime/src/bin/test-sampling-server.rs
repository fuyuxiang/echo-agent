use anyhow::Result;

/// Usage: cargo run -p echo-agent-runtime --bin test-sampling-server
#[derive(Debug, clap::Parser)]
pub struct Cli {
    #[arg(long, default_value = "127.0.0.1:55345")]
    pub bind_ip_port: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    use clap::Parser;
    let cli = Cli::parse();
    let app = axum::Router::new().route("/chat/completions", axum::routing::post(handler));
    let listener = tokio::net::TcpListener::bind(&cli.bind_ip_port).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn handler(_: axum::http::Request<axum::body::Body>) -> impl axum::response::IntoResponse {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<axum::response::sse::Event, String>>(10);

    tokio::spawn(async move {
        let chunk1 = echo_agent_runtime::sampling::ChatCompletionChunk {
            id: "chat-123".to_string(),
            object: "chat.completion.chunk".to_string(),
            created: 1,
            model: "demo-model".to_string(),
            choices: vec![],
            usage: None,
            system_fingerprint: None,
        };
        match serde_json::to_string(&chunk1) {
            Ok(json1) => {
                let _ = tx
                    .send(Ok(axum::response::sse::Event::default().data(json1)))
                    .await;
            }
            Err(e) => {
                let _ = tx.send(Err(e.to_string())).await;
                return;
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

        // End stream
        let _ = tx
            .send(Ok(axum::response::sse::Event::default().data("[DONE]")))
            .await;
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    axum::response::Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(std::time::Duration::from_secs(15))
            .text("keep-alive"),
    )
}
