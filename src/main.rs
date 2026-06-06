//! codex-memoryd binary entrypoint. Initializes structured logging then
//! dispatches the CLI. The `serve` subcommand starts the daemon; all other
//! subcommands run synchronously against the store.

mod cli;

use clap::Parser;
use cli::Cli;
use tracing_subscriber::EnvFilter;

fn main() {
    let cli = Cli::parse();

    // Resolve log level from --log / env / default.
    let level = cli
        .log
        .clone()
        .or_else(|| std::env::var("CODEX_MEMORYD_LOG").ok())
        .unwrap_or_else(|| "info".to_string());

    let filter = EnvFilter::try_new(&level)
        .or_else(|_| EnvFilter::try_new("info"))
        .unwrap_or_else(|_| EnvFilter::new("info"));

    // Logs go to stderr so CLI JSON on stdout stays clean and pipeable.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .try_init();

    let code = cli::run(cli);
    std::process::exit(code);
}
