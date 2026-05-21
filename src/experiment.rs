use std::path::PathBuf;

use crate::{config::Config, error::Result};

#[derive(Debug, Clone)]
pub struct ExperimentPaths {
    project_root: PathBuf,
    name: String,
}

#[allow(dead_code)] // path helpers; some are unused in Phase 1 and consumed by later phases
impl ExperimentPaths {
    pub fn new(project_root: PathBuf, name: String) -> Self {
        Self { project_root, name }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn project_root(&self) -> &PathBuf {
        &self.project_root
    }

    pub fn root(&self) -> PathBuf {
        self.project_root.join(".autorize").join(&self.name)
    }

    pub fn config_path(&self) -> PathBuf {
        self.root().join("config.toml")
    }

    pub fn program_path(&self) -> PathBuf {
        self.root().join("program.md")
    }

    pub fn iterations_log(&self) -> PathBuf {
        self.root().join("iterations.jsonl")
    }

    pub fn state_path(&self) -> PathBuf {
        self.root().join("state.json")
    }

    pub fn lock_path(&self) -> PathBuf {
        self.root().join("run.lock")
    }

    pub fn iter_dir(&self, iter: u64) -> PathBuf {
        self.root().join(format!("iter-{iter:04}"))
    }

    pub fn load_config(&self) -> Result<Config> {
        let text = std::fs::read_to_string(self.config_path())?;
        Config::from_toml(&text)
    }

    pub fn load_program(&self) -> Result<String> {
        Ok(std::fs::read_to_string(self.program_path())?)
    }
}
