//! `headless-use` binary entry point.

use clap::Parser;

use headless_use::cli::{Cli, Command};

#[tokio::main]
async fn main() {
    init_tracing();
    let cli = Cli::parse();
    let code = match cli.command {
        Command::Doctor => headless_use::cli::commands::doctor().await,
        Command::Launch(args) => headless_use::cli::commands::launch(args).await,
        Command::Serve(args) => headless_use::cli::commands::serve(args).await,
        Command::Run(args) => headless_use::cli::commands::run(args).await,
        Command::InstallBrowser => headless_use::cli::commands::install_browser().await,
        Command::Mcp(args) => headless_use::cli::commands::mcp(args).await,
        Command::Replay(args) => headless_use::cli::commands::replay(args).await,
        Command::View(args) => headless_use::cli::commands::view(args).await,
        Command::Dewiggle(args) => headless_use::cli::commands::dewiggle(args).await,
    };
    std::process::exit(code)
}

fn init_tracing() {
    let filter = std::env::var("RUST_LOG").unwrap_or_else(|_| "warn".into());
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(filter))
        .with_writer(std::io::stderr)
        .init();
}
