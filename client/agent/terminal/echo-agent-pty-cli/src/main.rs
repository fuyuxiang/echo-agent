//! echo-agent-pty CLI — headless PTY controller.

use clap::Parser;

mod cli;
mod commands;
mod registry;

use cli::{Cli, Commands};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Run {
            command,
            width,
            height,
            cwd,
            env,
            port,
            name,
            force,
            timeout,
            linger,
            quiet,
        } => {
            commands::run::run(
                command, width, height, cwd, env, port, name, force, timeout, linger, quiet,
            )
            .await?;
        }
        Commands::Send {
            target,
            keys,
            enter,
        } => {
            let url = target.to_url()?;
            commands::client::send(&url, &keys, enter).await?;
        }
        Commands::Screen {
            target,
            rows,
            cols,
            json: _,
            cursor,
            ansi: _,
            styled,
            html,
            full,
            line_numbers,
        } => {
            let url = target.to_url()?;
            let format = if styled {
                "styled"
            } else if html {
                "html"
            } else {
                "text"
            };
            commands::client::screen(
                &url,
                rows.as_deref(),
                cols.as_deref(),
                cursor,
                format,
                full,
                line_numbers,
            )
            .await?;
        }
        Commands::Status { target } => {
            let url = target.to_url()?;
            commands::client::status(&url).await?;
        }
        Commands::Stop { target } => {
            let url = target.to_url()?;
            commands::client::stop(&url).await?;
        }
        Commands::Resize { target, size } => {
            let url = target.to_url()?;
            commands::client::resize(&url, &size).await?;
        }
        Commands::Cursor { target } => {
            let url = target.to_url()?;
            commands::client::cursor(&url).await?;
        }
        Commands::Wait {
            target,
            text,
            regex,
            gone,
            stable_ms,
            timeout,
        } => {
            // Exit code contract: 0 matched, 1 timeout, 2 usage/connection errors.
            let exit = |code: i32| -> ! { std::process::exit(code) };
            let url = match target.to_url() {
                Ok(url) => url,
                Err(e) => {
                    eprintln!("Error: {e:#}");
                    exit(2);
                }
            };
            match commands::client::wait(
                &url,
                text.as_deref(),
                regex.as_deref(),
                gone.as_deref(),
                stable_ms,
                timeout,
            )
            .await
            {
                Ok(true) => {}
                Ok(false) => exit(1),
                Err(e) => {
                    eprintln!("Error: {e:#}");
                    exit(2);
                }
            }
        }
        Commands::List { json } => {
            let sessions = registry::list_sessions()?;
            if json {
                let mut items = Vec::new();
                for (name, info) in &sessions {
                    items.push(serde_json::json!({
                        "name": name,
                        "port": info.port,
                        "pid": info.pid,
                        "command": info.command,
                        "started_at": info.started_at,
                        // "server_alive", not "alive": /query/status's "alive" means child-alive, which diverges under --linger.
                        "server_alive": registry::server_alive(info.port).await,
                    }));
                }
                println!("{}", serde_json::to_string_pretty(&items)?);
            } else if sessions.is_empty() {
                println!("No active sessions");
            } else {
                println!(
                    "{:<16} {:<8} {:<8} {:<8} COMMAND",
                    "NAME", "PORT", "PID", "SERVER"
                );
                println!("{}", "-".repeat(60));
                for (name, info) in &sessions {
                    let pid = info
                        .pid
                        .map(|p| p.to_string())
                        .unwrap_or_else(|| "?".into());
                    let server = if registry::server_alive(info.port).await {
                        "live"
                    } else {
                        "dead"
                    };
                    println!(
                        "{:<16} {:<8} {:<8} {:<8} {}",
                        name,
                        info.port,
                        pid,
                        server,
                        info.command.join(" ")
                    );
                }
            }
        }
    }

    Ok(())
}
