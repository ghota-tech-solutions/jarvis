//! jarvis-tui — ratatui front-end for the daemon.

use anyhow::{Context, Result};
use clap::Parser;
use std::time::Duration;
use tonic::transport::{Channel, Endpoint};

mod app;
mod event_loop;
mod theme;
mod ui;

use app::App;

#[derive(Debug, Parser)]
#[command(name = "jarvis-tui", version, about = "Jarvis terminal UI")]
struct Cli {
    /// Daemon endpoint.
    #[arg(long, env = "JARVIS_DAEMON_URL", default_value = "http://127.0.0.1:7777")]
    daemon: String,

    /// Connect timeout in seconds.
    #[arg(long, default_value_t = 3)]
    connect_timeout: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Tracing goes to a file (stdout is owned by ratatui); $JARVIS_TUI_LOG=path overrides.
    init_file_tracing();

    let channel = connect(&cli.daemon, cli.connect_timeout)
        .await
        .context("connect to daemon")?;

    let app = App::new(channel, cli.daemon.clone());
    let result = event_loop::run(app).await;

    // event_loop::run is responsible for restoring the terminal on its way out;
    // double-belt-and-suspenders in case it panicked early.
    let _ = crossterm::terminal::disable_raw_mode();
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::event::DisableMouseCapture,
    );
    result
}

fn init_file_tracing() {
    use tracing_subscriber::EnvFilter;
    let path = std::env::var("JARVIS_TUI_LOG").unwrap_or_else(|_| "jarvis-tui.log".to_string());
    if let Ok(file) = std::fs::File::create(&path) {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")))
            .with_writer(std::sync::Mutex::new(file))
            .with_ansi(false)
            .try_init();
    }
}

async fn connect(endpoint: &str, timeout_secs: u64) -> Result<Channel> {
    let ep = Endpoint::from_shared(endpoint.to_string())
        .context("invalid daemon endpoint")?
        .connect_timeout(Duration::from_secs(timeout_secs))
        .timeout(Duration::from_secs(3600));
    ep.connect()
        .await
        .with_context(|| format!("connect to {endpoint}"))
}
