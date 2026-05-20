use std::path::PathBuf;

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

    pub fn iter_dir(&self, iter: u64) -> PathBuf {
        self.root().join(format!("iter-{iter:04}"))
    }
}
