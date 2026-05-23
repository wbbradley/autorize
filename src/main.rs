mod agent;
mod cli;
mod config;
mod error;
mod experiment;
mod iteration;
mod lock;
mod prompt;
mod schedule;
mod scoring;
mod storage;
mod subproc;
mod templates;
mod worktree;

use clap::Parser;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    // Tear down spawned child process groups on Ctrl-C / SIGTERM / SIGHUP
    // instead of orphaning the agent (it runs in its own session).
    subproc::install_signal_handler();
    let cli = cli::Cli::parse();
    cli::dispatch(cli)
}
