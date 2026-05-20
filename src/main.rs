mod cli;
mod config;
mod error;
mod experiment;
mod scoring;
mod templates;
mod worktree;

use clap::Parser;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let cli = cli::Cli::parse();
    cli::dispatch(cli)
}
