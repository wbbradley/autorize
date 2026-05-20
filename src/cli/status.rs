#[derive(clap::Args, Debug)]
pub struct StatusArgs {
    /// Experiment name.
    pub name: String,
}

pub fn run(_args: StatusArgs) -> anyhow::Result<()> {
    anyhow::bail!("`autorize status` is not yet implemented (Phase 5)")
}
