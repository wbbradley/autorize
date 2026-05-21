use std::{
    fs::{self, File, OpenOptions},
    io::{ErrorKind, Write},
    path::Path,
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Merged,
    Discarded,
    Noop,
    Invalid,
    Killed,
    Denied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CurrentStep {
    Idle,
    AllocateIter,
    CreateWorktree,
    RunSetup,
    BuildPrompt,
    InvokeAgent,
    CaptureDiff,
    RunTeardown,
    Score,
    Decide,
    Merge,
    Discard,
    Cleanup,
    Record,
    CheckDeadline,
    Done,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IterationRecord {
    pub iter: u64,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
    pub outcome: Outcome,
    pub score: Option<f64>,
    pub best_so_far: Option<f64>,
    pub agent_exit: Option<i32>,
    pub agent_killed_by_budget: bool,
    pub diff_lines: u64,
    #[serde(default)]
    pub notes: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateSnapshot {
    pub experiment: String,
    pub branch: String,
    pub base_commit: String,
    pub iter_in_progress: Option<u64>,
    pub current_step: CurrentStep,
    pub best_score: Option<f64>,
    pub best_iter: Option<u64>,
    pub started_at: DateTime<Utc>,
    pub deadline: DateTime<Utc>,
    pub iterations_completed: u64,
    pub consecutive_noops: u32,
}

#[allow(dead_code)] // wired in by Phase 4 iteration + Phase 5 run loop
pub fn write_state(path: &Path, state: &StateSnapshot) -> Result<()> {
    let bytes = serde_json::to_string_pretty(state)?;
    write_atomic(path, bytes.as_bytes())
}

pub fn read_state(path: &Path) -> Result<Option<StateSnapshot>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

#[allow(dead_code)] // wired in by Phase 4 iteration
pub fn append_iteration(path: &Path, rec: &IterationRecord) -> Result<()> {
    let line = serde_json::to_string(rec)?;
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    f.write_all(line.as_bytes())?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    Ok(())
}

pub fn read_iterations(path: &Path) -> Result<Vec<IterationRecord>> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let text = String::from_utf8_lossy(&bytes);
    let mut lines: Vec<&str> = text.split('\n').filter(|l| !l.is_empty()).collect();
    if let Some(last) = lines.last()
        && serde_json::from_str::<IterationRecord>(last).is_err()
    {
        lines.pop();
    }
    lines
        .into_iter()
        .map(|l| serde_json::from_str(l).map_err(Into::into))
        .collect()
}

fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    let result = (|| -> Result<()> {
        let mut f = File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, path)?;
        if let Some(parent) = path.parent() {
            let _ = File::open(parent).and_then(|d| d.sync_all());
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use chrono::TimeZone;
    use tempfile::tempdir;

    use super::*;

    fn sample_state() -> StateSnapshot {
        StateSnapshot {
            experiment: "pi".to_string(),
            branch: "autorize/pi".to_string(),
            base_commit: "abc123".to_string(),
            iter_in_progress: Some(7),
            current_step: CurrentStep::InvokeAgent,
            best_score: Some(std::f64::consts::PI),
            best_iter: Some(5),
            started_at: Utc.with_ymd_and_hms(2026, 5, 20, 8, 0, 0).unwrap(),
            deadline: Utc.with_ymd_and_hms(2026, 5, 20, 12, 0, 0).unwrap(),
            iterations_completed: 6,
            consecutive_noops: 0,
        }
    }

    fn sample_record(iter: u64) -> IterationRecord {
        IterationRecord {
            iter,
            started_at: Utc.with_ymd_and_hms(2026, 5, 20, 8, 0, 0).unwrap(),
            ended_at: Utc.with_ymd_and_hms(2026, 5, 20, 8, 1, 0).unwrap(),
            outcome: Outcome::Merged,
            score: Some(2.5),
            best_so_far: Some(2.5),
            agent_exit: Some(0),
            agent_killed_by_budget: false,
            diff_lines: 4,
            notes: String::new(),
        }
    }

    #[test]
    fn write_atomic_overwrites_destination() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("state.json");
        write_atomic(&p, b"v1").unwrap();
        write_atomic(&p, b"v2").unwrap();
        let read = fs::read(&p).unwrap();
        assert_eq!(read, b"v2");
        let tmp = p.with_extension("json.tmp");
        assert!(!tmp.exists(), "stray tmp file at {tmp:?}");
    }

    #[test]
    fn write_atomic_stray_tmp_doesnt_corrupt_dest() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("state.json");
        write_atomic(&p, b"v1").unwrap();
        // Simulate a torn write that never got renamed.
        let tmp = p.with_extension("json.tmp");
        fs::write(&tmp, b"GARBAGE-half-write").unwrap();
        let read = fs::read(&p).unwrap();
        assert_eq!(read, b"v1");
    }

    #[test]
    fn read_state_missing_returns_none() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("state.json");
        assert!(read_state(&p).unwrap().is_none());
    }

    #[test]
    fn state_round_trips_json() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("state.json");
        let s = sample_state();
        write_state(&p, &s).unwrap();
        let got = read_state(&p).unwrap().unwrap();
        assert_eq!(got.experiment, s.experiment);
        assert_eq!(got.branch, s.branch);
        assert_eq!(got.base_commit, s.base_commit);
        assert_eq!(got.iter_in_progress, s.iter_in_progress);
        assert_eq!(got.current_step, s.current_step);
        assert_eq!(got.best_score, s.best_score);
        assert_eq!(got.best_iter, s.best_iter);
        assert_eq!(got.started_at, s.started_at);
        assert_eq!(got.deadline, s.deadline);
        assert_eq!(got.iterations_completed, s.iterations_completed);
        assert_eq!(got.consecutive_noops, s.consecutive_noops);
    }

    #[test]
    fn append_iteration_100x_then_read_all() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("iterations.jsonl");
        for i in 0..100u64 {
            append_iteration(&p, &sample_record(i)).unwrap();
        }
        let recs = read_iterations(&p).unwrap();
        assert_eq!(recs.len(), 100);
        for (i, r) in recs.iter().enumerate() {
            assert_eq!(r.iter, i as u64);
        }
    }

    #[test]
    fn read_iterations_drops_torn_final_line() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("iterations.jsonl");
        for i in 0..5u64 {
            append_iteration(&p, &sample_record(i)).unwrap();
        }
        // Append a torn half-record with no trailing newline.
        let mut f = OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all(b"{\"iter\":5,\"started_at\":").unwrap();
        f.sync_all().unwrap();
        let recs = read_iterations(&p).unwrap();
        assert_eq!(recs.len(), 5);
    }

    #[test]
    fn read_iterations_drops_torn_final_line_after_newlines() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("iterations.jsonl");
        for i in 0..5u64 {
            append_iteration(&p, &sample_record(i)).unwrap();
        }
        // append the torn line — no trailing newline.
        let mut f = OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all(b"{\"iter\":6,\"started_at\":").unwrap();
        f.sync_all().unwrap();
        let recs = read_iterations(&p).unwrap();
        assert_eq!(recs.len(), 5);
    }

    #[test]
    fn read_iterations_errors_on_corrupt_middle_line() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("iterations.jsonl");
        // 3 good records.
        for i in 0..3u64 {
            append_iteration(&p, &sample_record(i)).unwrap();
        }
        // Inject a bad line (newline-terminated, so it's NOT the last line).
        let mut f = OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all(b"NOT-JSON\n").unwrap();
        f.sync_all().unwrap();
        // 1 more good record on the end.
        append_iteration(&p, &sample_record(99)).unwrap();
        let err = read_iterations(&p).unwrap_err();
        assert!(format!("{err}").contains("json"), "got: {err}");
    }
}
