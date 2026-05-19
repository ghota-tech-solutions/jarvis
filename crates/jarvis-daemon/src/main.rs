use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tracing::info;
use tracing_subscriber::EnvFilter;

mod service;

#[derive(Debug, Parser)]
#[command(
    name = "jarvis-daemon",
    version,
    about = "Jarvis daemon — long-running gRPC server"
)]
struct Cli {
    /// Path to jarvis.toml; defaults to ./jarvis.toml if present.
    #[arg(short, long, env = "JARVIS_CONFIG")]
    config: Option<PathBuf>,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Start the daemon and accept connections.
    Run {
        /// Bind address override (defaults to config's `daemon.addr`).
        #[arg(long, env = "JARVIS_DAEMON_ADDR")]
        addr: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    init_tracing();

    let cli = Cli::parse();
    let cfg = jarvis_config::load(cli.config.as_deref()).context("load config")?;

    match cli.cmd {
        Cmd::Run { addr } => {
            let bind = addr.unwrap_or(cfg.daemon.addr.clone());
            service::run(cfg, bind).await
        }
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_env("JARVIS_LOG")
        .unwrap_or_else(|_| EnvFilter::new("jarvis=info,tower=warn,h2=warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .init();
    info!("tracing initialized");
}
