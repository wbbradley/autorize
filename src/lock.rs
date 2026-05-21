use std::{
    fs::{File, OpenOptions},
    io::{Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use nix::fcntl::{Flock, FlockArg};

use crate::error::{Error, Result};

/// RAII guard holding an exclusive flock on the experiment's `run.lock`
/// for the lifetime of the run. Dropping releases the lock.
#[derive(Debug)]
pub struct ExperimentLock {
    _file: Flock<File>,
    #[allow(dead_code)]
    path: PathBuf,
}

impl ExperimentLock {
    /// Acquire an exclusive non-blocking flock on `path`. Creates the
    /// file if it doesn't exist. After locking, truncates and writes the
    /// current pid for diagnostics.
    pub fn acquire(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;

        let mut locked = match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
            Ok(f) => f,
            Err((_file, errno)) => {
                let pid_hint = std::fs::read_to_string(path)
                    .ok()
                    .and_then(|s| s.trim().parse::<i32>().ok());
                let pid_msg = pid_hint
                    .map(|p| format!(" (held by pid {p})"))
                    .unwrap_or_default();
                return Err(Error::Locked {
                    path: path.to_path_buf(),
                    detail: format!("{pid_msg} [{errno}]"),
                });
            }
        };

        let pid = std::process::id();
        {
            let f: &mut File = &mut locked;
            f.set_len(0)?;
            f.seek(SeekFrom::Start(0))?;
            writeln!(f, "{pid}")?;
            f.sync_all()?;
        }

        Ok(Self {
            _file: locked,
            path: path.to_path_buf(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn acquire_creates_file_and_writes_pid() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("run.lock");
        let lock = ExperimentLock::acquire(&p).unwrap();
        let s = fs::read_to_string(&p).unwrap();
        let pid: u32 = s.trim().parse().unwrap();
        assert_eq!(pid, std::process::id());
        drop(lock);
    }

    #[test]
    fn second_acquire_fails_while_held() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("run.lock");
        let _held = ExperimentLock::acquire(&p).unwrap();
        let err = ExperimentLock::acquire(&p).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("lock"), "got: {msg}");
    }

    #[test]
    fn lock_released_after_drop() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("run.lock");
        {
            let _a = ExperimentLock::acquire(&p).unwrap();
        }
        let _b = ExperimentLock::acquire(&p).unwrap();
    }
}
