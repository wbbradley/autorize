#[derive(clap::Args, Debug)]
pub struct RunArgs {
    /// Experiment name (must already exist under `.autorize/<name>/`).
    pub name: String,
    /// Run even if the working tree has uncommitted changes.
    #[arg(long)]
    pub allow_dirty: bool,
}

pub fn run(_args: RunArgs) -> anyhow::Result<()> {
    anyhow::bail!("`autorize run` is not yet implemented (Phase 5)")
}
