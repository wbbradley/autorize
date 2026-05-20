use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("config error: {0}")]
    Config(String),
    #[error("experiment {name:?} already exists at {path}")]
    ExperimentExists { name: String, path: PathBuf },
    #[error("invalid experiment name {0:?}: must be [A-Za-z0-9_-]+")]
    InvalidName(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("toml deserialize: {0}")]
    TomlDe(#[from] toml::de::Error),
    #[error("toml serialize: {0}")]
    TomlSer(#[from] toml::ser::Error),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
