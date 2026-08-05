//! codex-memoryd binary entrypoint. Initializes structured logging then
//! dispatches the CLI. The `serve` subcommand starts the daemon; all other
//! subcommands run synchronously against the store.

mod cli;

use clap::Parser;
use cli::Cli;
use tracing_subscriber::EnvFilter;

fn resolve_log_level(cli: &Cli) -> &str {
    cli.log.as_deref().unwrap_or("info")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_log_level_uses_cli_value_when_present() {
        std::env::remove_var("CODEX_MEMORYD_LOG");
        let cli = Cli::parse_from(["codex-memoryd", "--log", "debug", "status"]);
        assert_eq!(resolve_log_level(&cli), "debug");
    }

    #[test]
    fn resolve_log_level_defaults_to_info() {
        std::env::remove_var("CODEX_MEMORYD_LOG");
        let cli = Cli::parse_from(["codex-memoryd", "status"]);
        assert_eq!(resolve_log_level(&cli), "info");
    }
}

fn main() {
    let cli = Cli::parse();

    // Resolve log level from --log / env / default.
    let level = resolve_log_level(&cli);

    let filter = EnvFilter::try_new(level)
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
