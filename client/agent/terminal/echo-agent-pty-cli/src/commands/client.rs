//! Client commands — send/screen/status/cursor/resize/stop via HTTP.

use anyhow::{Context, Result};
use reqwest::Client;

/// Roots are skipped for plain HTTP targets: reqwest loads the OS store at
/// build time regardless of scheme, and a broken store must not fail the CLI.
fn builder_for(url: &str) -> reqwest::ClientBuilder {
    if url.starts_with("https://") {
        Client::builder()
    } else {
        Client::builder().tls_built_in_root_certs(false)
    }
}

#[allow(clippy::disallowed_methods)] // scheme-aware builder above; loopback skips roots by construction
fn client_for(url: &str) -> Result<Client> {
    builder_for(url)
        .build()
        .context("failed to build HTTP client")
}

/// Send keystrokes to a session.
pub async fn send(url: &str, keys: &str, enter: bool) -> Result<()> {
    let mut keys = keys.to_string();
    if enter {
        keys.push_str("<CR>");
    }

    let client = client_for(url)?;
    let resp = client
        .post(format!("{url}/control/send"))
        .json(&serde_json::json!({"keys": keys}))
        .send()
        .await
        .context("failed to send keys")?;

    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("send failed: {body}");
    }
    Ok(())
}

/// Query screen content.
pub async fn screen(
    url: &str,
    rows: Option<&str>,
    cols: Option<&str>,
    cursor: Option<char>,
    format: &str,
    full: bool,
    line_numbers: bool,
) -> Result<()> {
    let client = client_for(url)?;
    let mut req = client.get(format!("{url}/query/screen"));

    if let Some(r) = rows {
        req = req.query(&[("rows", r)]);
    }
    if let Some(c) = cols {
        req = req.query(&[("cols", c)]);
    }
    if let Some(ch) = cursor {
        req = req.query(&[("cursor", &ch.to_string())]);
    }
    req = req.query(&[("format", format)]);
    if full {
        req = req.query(&[("full", "true")]);
    }

    let resp = req.send().await.context("failed to query screen")?;

    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("screen query failed: {body}");
    }

    let body = resp.text().await?;

    if format == "html" || format == "styled" {
        println!("{body}");
    } else {
        // Parse as JSON and print lines.
        let output: serde_json::Value = serde_json::from_str(&body)?;
        if let Some(lines) = output.get("lines").and_then(|l| l.as_array()) {
            for (i, line) in lines.iter().enumerate() {
                let text = line.as_str().unwrap_or("");
                if line_numbers {
                    println!("{:4} {text}", i + 1);
                } else {
                    println!("{text}");
                }
            }
        }
    }

    Ok(())
}

/// Query cursor position.
pub async fn cursor(url: &str) -> Result<()> {
    let client = client_for(url)?;
    let resp = client
        .get(format!("{url}/query/cursor"))
        .send()
        .await
        .context("failed to query cursor")?;
    let body = resp.text().await?;
    println!("{body}");
    Ok(())
}

/// Query session status.
pub async fn status(url: &str) -> Result<()> {
    let client = client_for(url)?;
    let resp = client
        .get(format!("{url}/query/status"))
        .send()
        .await
        .context("failed to query status")?;
    let body = resp.text().await?;
    println!("{body}");
    Ok(())
}

/// Resize terminal.
pub async fn resize(url: &str, size: &str) -> Result<()> {
    let (cols, rows) = size
        .split_once('x')
        .ok_or_else(|| anyhow::anyhow!("invalid size format, expected COLSxROWS (e.g. 120x40)"))?;
    let cols: u16 = cols.parse().context("invalid cols")?;
    let rows: u16 = rows.parse().context("invalid rows")?;

    let client = client_for(url)?;
    let resp = client
        .post(format!("{url}/control/resize"))
        .json(&serde_json::json!({"cols": cols, "rows": rows}))
        .send()
        .await
        .context("failed to resize")?;

    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("resize failed: {body}");
    }
    println!("Resized to {cols}x{rows}");
    Ok(())
}

/// Long-poll the wait endpoint; prints the outcome JSON and returns whether it matched.
pub async fn wait(
    url: &str,
    text: Option<&str>,
    regex: Option<&str>,
    gone: Option<&str>,
    stable_ms: Option<u64>,
    timeout_secs: u64,
) -> Result<bool> {
    // The HTTP timeout outlasts the wait so the server, not the client, decides the outcome.
    #[allow(clippy::disallowed_methods)]
    // scheme-aware builder above; loopback skips roots by construction
    let client = builder_for(url)
        .timeout(std::time::Duration::from_secs(
            timeout_secs.saturating_add(5),
        ))
        .build()
        .context("failed to build HTTP client")?;

    let mut req = client
        .get(format!("{url}/wait"))
        .query(&[("timeout_ms", timeout_secs.saturating_mul(1000).to_string())]);
    if let Some(t) = text {
        req = req.query(&[("text", t)]);
    }
    if let Some(r) = regex {
        req = req.query(&[("regex", r)]);
    }
    if let Some(g) = gone {
        req = req.query(&[("gone", g)]);
    }
    if let Some(ms) = stable_ms {
        req = req.query(&[("stable_ms", ms.to_string())]);
    }

    let resp = req.send().await.context("failed to call wait")?;
    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("wait failed: {body}");
    }

    let outcome: serde_json::Value = resp.json().await.context("invalid wait response")?;
    println!("{}", serde_json::to_string_pretty(&outcome)?);
    Ok(outcome
        .get("matched")
        .and_then(|m| m.as_bool())
        .unwrap_or(false))
}

/// Stop a session.
pub async fn stop(url: &str) -> Result<()> {
    let client = client_for(url)?;
    let resp = client
        .post(format!("{url}/control/stop"))
        .send()
        .await
        .context("failed to stop session")?;

    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("stop failed: {body}");
    }
    println!("Session stopped");
    Ok(())
}
