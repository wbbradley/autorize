pub mod clean;
pub mod init;
pub mod llms;
pub mod resume;
pub mod run;
pub mod status;
pub mod tell;

#[derive(clap::Parser)]
#[command(name = "autorize", version, about = "Iterative-improvement harness")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(clap::Subcommand)]
pub enum Command {
    /// Scaffold a new experiment under .autorize/<name>/.
    Init(init::InitArgs),
    /// Print an agent-targeted markdown reference covering every config
    /// field, subcommand, and on-disk artifact.
    Llms(llms::LlmsArgs),
    /// Run the experiment loop until deadline.
    Run(run::RunArgs),
    /// Show experiment status from state.json + iterations.jsonl.
    Status(status::StatusArgs),
    /// Append operator guidance, injected into the next iteration's prompt.
    Tell(tell::TellArgs),
    /// Resume an experiment after a crash or stop.
    Resume(resume::ResumeArgs),
    /// Tidy a finished/abandoned experiment: free the tracking branch, drop
    /// stale staged indexes, and prune dead worktree registrations.
    Clean(clean::CleanArgs),
}

pub fn dispatch(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Command::Init(a) => init::run(a),
        Command::Llms(a) => llms::run(a),
        Command::Run(a) => run::run(a),
        Command::Status(a) => status::run(a),
        Command::Tell(a) => tell::run(a),
        Command::Resume(a) => resume::run(a),
        Command::Clean(a) => clean::run(a),
    }
}
