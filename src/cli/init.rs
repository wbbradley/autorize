use std::{env, fs, path::PathBuf};

use crate::{
    config::Config,
    error::{Error, Result},
    experiment::ExperimentPaths,
    templates,
};

#[derive(clap::Args, Debug)]
pub struct InitArgs {
    /// Experiment name. Must match `[A-Za-z0-9_-]+`.
    pub name: String,
}

pub fn run(args: InitArgs) -> anyhow::Result<()> {
    let root = env::current_dir()?;
    run_with_root(args, root)?;
    Ok(())
}

pub fn run_with_root(args: InitArgs, project_root: PathBuf) -> Result<Config> {
    if !is_valid_name(&args.name) {
        return Err(Error::InvalidName(args.name));
    }

    let paths = ExperimentPaths::new(project_root, args.name.clone());
    let root = paths.root();
    if root.exists() {
        return Err(Error::ExperimentExists {
            name: args.name,
            path: root,
        });
    }

    tracing::info!("mkdir -p {}", root.display());
    fs::create_dir_all(&root)?;
    let config_text = templates::render_config(&args.name);
    let program_text = templates::render_program(&args.name);
    tracing::info!("write {}", paths.config_path().display());
    fs::write(paths.config_path(), &config_text)?;
    tracing::info!("write {}", paths.program_path().display());
    fs::write(paths.program_path(), &program_text)?;

    let cfg = Config::from_toml(&config_text)?;

    tracing::info!("created experiment {:?} at {}", args.name, root.display());
    tracing::info!("  - {}", paths.config_path().display());
    tracing::info!("  - {}", paths.program_path().display());
    tracing::info!("edit program.md with your agent instructions, then run:");
    tracing::info!("  autorize run {}", args.name);

    Ok(cfg)
}

fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_files_in_fresh_dir() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = run_with_root(
            InitArgs {
                name: "pi".to_string(),
            },
            dir.path().to_path_buf(),
        )
        .unwrap();
        assert_eq!(cfg.experiment.name, "pi");
        let root = dir.path().join(".autorize").join("pi");
        assert!(root.join("config.toml").exists());
        assert!(root.join("program.md").exists());

        let body = fs::read_to_string(root.join("config.toml")).unwrap();
        Config::from_toml(&body).expect("round-trips");
    }

    #[test]
    fn refuses_existing_experiment_dir() {
        let dir = tempfile::tempdir().unwrap();
        let pre = dir.path().join(".autorize").join("pi");
        fs::create_dir_all(&pre).unwrap();
        let err = run_with_root(
            InitArgs {
                name: "pi".to_string(),
            },
            dir.path().to_path_buf(),
        )
        .unwrap_err();
        assert!(
            matches!(err, Error::ExperimentExists { .. }),
            "got: {err:?}"
        );
    }

    #[test]
    fn refuses_bad_name() {
        let dir = tempfile::tempdir().unwrap();
        let err = run_with_root(
            InitArgs {
                name: "../etc".to_string(),
            },
            dir.path().to_path_buf(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::InvalidName(_)), "got: {err:?}");
    }
}
