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

use std::{env, fs::OpenOptions};

use clap::Parser;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

fn main() -> anyhow::Result<()> {
    // Held for the lifetime of `main` so the file appender's worker flushes.
    let _log_guard = init_logging();
    // Tear down spawned child process groups on Ctrl-C / SIGTERM / SIGHUP
    // instead of orphaning the agent (it runs in its own session).
    subproc::install_signal_handler();
    let cli = cli::Cli::parse();
    cli::dispatch(cli)
}

/// Initialize tracing with an stderr layer plus — when a `logs/` directory can
/// be created under the current directory — an appending file layer over
/// `logs/autorize.log`. Child-process stdout/stderr is teed into the same file
/// via [`subproc::set_tee_log`], so the central log holds both autorize's own
/// narrative and all subprocess output. Defaults to `info` when `RUST_LOG` is
/// unset (the run narrative is emitted at `info`). Returns the appender's
/// `WorkerGuard`, which must outlive `main` for buffered lines to flush.
fn init_logging() -> Option<WorkerGuard> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let stderr_layer = fmt::layer().with_writer(std::io::stderr);

    let logs_dir = env::current_dir().ok().map(|d| d.join("logs"));
    let file_appender = logs_dir.as_ref().and_then(|dir| {
        std::fs::create_dir_all(dir).ok()?;
        let log_path = dir.join("autorize.log");
        // A second handle on the same file (append mode) is the tee target for
        // child stdio; O_APPEND keeps writes from the appender's worker thread
        // and the synchronous tee from clobbering each other.
        if let Ok(f) = OpenOptions::new().create(true).append(true).open(&log_path) {
            subproc::set_tee_log(f);
        }
        Some(tracing_appender::rolling::never(dir, "autorize.log"))
    });

    match file_appender {
        Some(appender) => {
            let (non_blocking, guard) = tracing_appender::non_blocking(appender);
            tracing_subscriber::registry()
                .with(filter)
                .with(stderr_layer)
                .with(fmt::layer().with_ansi(false).with_writer(non_blocking))
                .init();
            Some(guard)
        }
        None => {
            tracing_subscriber::registry()
                .with(filter)
                .with(stderr_layer)
                .init();
            None
        }
    }
}
