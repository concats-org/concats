use clap::Parser;
use concats_cli::{cli::Cli, commands, tui};

#[tokio::main]
async fn main() -> miette::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    tui::install_panic_hook();

    let cli = Cli::parse();
    commands::run(cli).await
}
