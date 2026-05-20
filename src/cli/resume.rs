#[derive(clap::Args, Debug)]
pub struct ResumeArgs {
    /// Experiment name.
    pub name: String,
}

pub fn run(_args: ResumeArgs) -> anyhow::Result<()> {
    anyhow::bail!("`autorize resume` is not yet implemented (Phase 5)")
}
