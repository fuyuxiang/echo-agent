//! Standalone voice debug harness: mic → streaming STT → transcript.
//!
//! ```bash
//! export ECHO_AGENT_API_KEY=...
//! cargo run -p echo-agent-voice --bin voice-probe -- --seconds 5
//! ```

use std::path::PathBuf;

use echo_agent_voice::{
    StaticVoiceAuth, VoiceConfig, VoiceProbeOptions, format_probe_report, run_streaming_probe,
};

fn main() -> anyhow::Result<()> {
    // Hidden mic-capture helper intercept (macOS): the capture backend
    // re-execs the current binary — here, voice-probe itself. Runs before any
    // runtime/TLS init so the capture child stays minimal.
    if let Some(code) = echo_agent_voice::maybe_run_capture_subprocess() {
        std::process::exit(code);
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run())
}

async fn run() -> anyhow::Result<()> {
    echo_agent_extra_ca::ensure_default_crypto_provider();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,echo_agent_voice=debug".into()),
        )
        .init();

    let args = parse_args(std::env::args().skip(1).collect());

    let auth = std::env::var("ECHO_AGENT_API_KEY")
        .ok()
        .and_then(StaticVoiceAuth::shared)
        .ok_or_else(|| {
            anyhow::anyhow!("set ECHO_AGENT_API_KEY (standalone probe has no login session)")
        })?;

    let config = load_config(args.config_path.as_deref());

    eprintln!(
        "Voice probe: listening {}s (sample_rate={})",
        args.seconds, config.sample_rate
    );
    eprintln!("Speak now...\n");

    if args.mic_only {
        #[cfg(feature = "audio")]
        {
            let (bytes, chunks) =
                echo_agent_voice::run_mic_only_probe(config.sample_rate, args.seconds)?;
            println!("Mic-only OK: {bytes} bytes in {chunks} chunks");
            if bytes == 0 {
                println!("WARNING: no audio — grant mic access to the terminal");
            }
            return Ok(());
        }
        #[cfg(not(feature = "audio"))]
        anyhow::bail!("built without `audio` feature");
    }

    let report = run_streaming_probe(VoiceProbeOptions {
        config,
        auth,
        capture_secs: args.seconds,
    })
    .await?;

    print!("{}", format_probe_report(&report));
    if report
        .transcript
        .as_ref()
        .is_none_or(|t| t.trim().is_empty())
    {
        std::process::exit(1);
    }
    Ok(())
}

struct Args {
    seconds: u32,
    config_path: Option<PathBuf>,
    mic_only: bool,
}

fn parse_args(argv: Vec<String>) -> Args {
    let mut out = Args {
        seconds: 5,
        config_path: None,
        mic_only: false,
    };
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--seconds" | "-s" => {
                i += 1;
                if i < argv.len() {
                    out.seconds = argv[i].parse().unwrap_or(5);
                }
            }
            "--config" => {
                i += 1;
                if i < argv.len() {
                    out.config_path = Some(PathBuf::from(&argv[i]));
                }
            }
            "--mic-only" => out.mic_only = true,
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            other if !other.starts_with('-') => {}
            _ => eprintln!("unknown arg: {}", argv[i]),
        }
        i += 1;
    }
    out
}

fn load_config(path: Option<&std::path::Path>) -> VoiceConfig {
    // The probe has no shell config stack; env is the resolved fallback
    // (config table still beats it, matching the pager's precedence).
    let env_base = std::env::var("ECHO_AGENT_API_BASE_URL").ok();
    if let Some(path) = path
        && let Ok(raw) = std::fs::read_to_string(path)
        && let Ok(table) = toml::from_str::<toml::Table>(&raw)
    {
        return VoiceConfig::from_config_table(&table, env_base.as_deref());
    }
    if let Ok(home) = std::env::var("ECHO_AGENT_HOME")
        && let Ok(raw) = std::fs::read_to_string(PathBuf::from(home).join("config.toml"))
        && let Ok(table) = toml::from_str::<toml::Table>(&raw)
    {
        return VoiceConfig::from_config_table(&table, env_base.as_deref());
    }
    if let Ok(raw) = std::fs::read_to_string(
        std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_default()
            .join(".echo-agent/config.toml"),
    ) && let Ok(table) = toml::from_str::<toml::Table>(&raw)
    {
        return VoiceConfig::from_config_table(&table, env_base.as_deref());
    }
    VoiceConfig::from_config_table(&toml::Table::new(), env_base.as_deref())
}

fn print_help() {
    eprintln!(
        r#"voice-probe — debug mic + STT outside the pager

Usage:
  voice-probe [--seconds 5] [--mic-only]

Environment:
  ECHO_AGENT_API_KEY     required
  RUST_LOG        optional (default info,echo_agent_voice=debug)

Reads [voice] from ~/.echo-agent/config.toml unless --config PATH is set.
"#
    );
}
