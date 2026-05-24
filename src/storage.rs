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
    /// Model-generated 1-2 sentence summary of what this iteration attempted
    /// and why the score moved, written by the optional `[summarize]` step
    /// after the worker agent exits. Empty when summarization is disabled, the
    /// iteration was a `noop`, or the summarize step failed (best-effort).
    /// `#[serde(default)]` keeps pre-A2 `iterations.jsonl` logs loadable.
    #[serde(default)]
    pub summary: String,
}

/// One line of operator guidance recorded by `autorize tell` (or hand-edited
/// into `guidance.jsonl`). Structured rather than a plain blob so a future
/// consumed/expiry/ack mechanism stays cheap to add; in v1 every entry
/// persists and is shown to the agent every iteration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuidanceEntry {
    /// When the guidance was recorded.
    pub ts: DateTime<Utc>,
    /// Best-effort iteration number the run was on when this was added (from
    /// `state.json`), or `null` when no state existed yet. Informational only —
    /// rendered as a `(since iter N)` hint in the prompt.
    pub added_at_iter: Option<u64>,
    /// The guidance text shown to the agent.
    pub text: String,
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
    /// Lifetime count of records in `iterations.jsonl` (every completed
    /// iteration, including reconciled `killed` records). Never reset.
    pub iterations_completed: u64,
    /// Iterations done in the *current* run; what `max_iterations` is checked
    /// against, and what `autorize run --fresh` resets to 0. A reconciled
    /// `killed` record does NOT bump this (a crash should not burn a budget
    /// slot). Defaults to 0 for, and is migrated by `read_state` from,
    /// pre-existing state files that predate this field.
    #[serde(default)]
    pub run_iterations_completed: u64,
    pub consecutive_noops: u32,
}

pub fn write_state(path: &Path, state: &StateSnapshot) -> Result<()> {
    let bytes = serde_json::to_string_pretty(state)?;
    write_atomic(path, bytes.as_bytes())
}

pub fn read_state(path: &Path) -> Result<Option<StateSnapshot>> {
    match fs::read(path) {
        Ok(bytes) => {
            let mut v: serde_json::Value = serde_json::from_slice(&bytes)?;
            // Migrate state files that predate the per-run iteration counter:
            // attribute all prior iterations to the current run so a non-fresh
            // re-run behaves exactly as before (it stops at the same cap).
            if let Some(obj) = v.as_object_mut()
                && !obj.contains_key("run_iterations_completed")
            {
                let lifetime = obj
                    .get("iterations_completed")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!(0));
                obj.insert("run_iterations_completed".to_string(), lifetime);
            }
            Ok(Some(serde_json::from_value(v)?))
        }
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn append_iteration(path: &Path, rec: &IterationRecord) -> Result<()> {
    let line = serde_json::to_string(rec)?;
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    f.write_all(line.as_bytes())?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    tracing::info!("appended record to {}", path.display());
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

/// Atomically rewrite the entire `iterations.jsonl` from `recs`. The
/// append-only [`append_iteration`] path can't express a mutation of a
/// mid-file record (e.g. backfilling a summary), so this serializes every
/// record to one jsonl buffer and lands it via the same tmp + fsync + rename +
/// dir-fsync dance as [`write_state`] — no torn file. Safe because `autorize
/// run` holds the experiment lock, so there is no concurrent writer.
pub fn rewrite_iterations(path: &Path, recs: &[IterationRecord]) -> Result<()> {
    let mut buf = String::new();
    for rec in recs {
        buf.push_str(&serde_json::to_string(rec)?);
        buf.push('\n');
    }
    write_atomic(path, buf.as_bytes())
}

/// Append one operator-guidance entry to `guidance.jsonl`. Mirrors
/// [`append_iteration`]: `O_APPEND` + per-line `fsync` so a concurrent
/// `autorize tell` (running while `autorize run` loops) lands atomically.
pub fn append_guidance(path: &Path, entry: &GuidanceEntry) -> Result<()> {
    let line = serde_json::to_string(entry)?;
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    f.write_all(line.as_bytes())?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    tracing::info!("appended guidance to {}", path.display());
    Ok(())
}

/// Read all operator-guidance entries. Mirrors [`read_iterations`]: a missing
/// file yields an empty `Vec`, and a torn last line (e.g. a `tell` that landed
/// mid-write) is dropped. The file is documented as hand-editable.
pub fn read_guidance(path: &Path) -> Result<Vec<GuidanceEntry>> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let text = String::from_utf8_lossy(&bytes);
    let mut lines: Vec<&str> = text.split('\n').filter(|l| !l.is_empty()).collect();
    if let Some(last) = lines.last()
        && serde_json::from_str::<GuidanceEntry>(last).is_err()
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
        tracing::info!("wrote {} (atomic)", path.display());
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
            run_iterations_completed: 6,
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
            summary: String::new(),
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
        assert_eq!(got.run_iterations_completed, s.run_iterations_completed);
        assert_eq!(got.consecutive_noops, s.consecutive_noops);
    }

    #[test]
    fn read_state_migrates_missing_run_counter() {
        // A state.json predating `run_iterations_completed` must load with the
        // per-run counter seeded from the lifetime count, so a non-fresh re-run
        // stops at the same cap it would have before this field existed.
        let dir = tempdir().unwrap();
        let p = dir.path().join("state.json");
        let legacy = r#"{
  "experiment": "pi",
  "branch": "autorize/pi",
  "base_commit": "abc123",
  "iter_in_progress": null,
  "current_step": "Idle",
  "best_score": 3.14,
  "best_iter": 5,
  "started_at": "2026-05-20T08:00:00Z",
  "deadline": "2026-05-20T12:00:00Z",
  "iterations_completed": 6,
  "consecutive_noops": 0
}"#;
        fs::write(&p, legacy).unwrap();
        let got = read_state(&p).unwrap().unwrap();
        assert_eq!(got.iterations_completed, 6);
        assert_eq!(got.run_iterations_completed, 6);
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
    fn rewrite_iterations_round_trips() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("iterations.jsonl");
        // Seed via the append path, then rewrite with a mutated middle record
        // (what backfill does when it fills in a summary).
        for i in 0..3u64 {
            append_iteration(&p, &sample_record(i)).unwrap();
        }
        let mut recs = read_iterations(&p).unwrap();
        recs[1].summary = "backfilled summary".to_string();
        rewrite_iterations(&p, &recs).unwrap();

        let got = read_iterations(&p).unwrap();
        assert_eq!(got.len(), 3);
        for (i, r) in got.iter().enumerate() {
            assert_eq!(r.iter, i as u64);
        }
        assert_eq!(got[0].summary, "");
        assert_eq!(got[1].summary, "backfilled summary");
        assert_eq!(got[2].summary, "");
        // No stray tmp left behind by the atomic rename.
        assert!(
            !p.with_extension("json.tmp").exists(),
            "stray tmp after rewrite"
        );
    }

    #[test]
    fn rewrite_iterations_stray_tmp_doesnt_corrupt_dest() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("iterations.jsonl");
        append_iteration(&p, &sample_record(0)).unwrap();
        rewrite_iterations(&p, &read_iterations(&p).unwrap()).unwrap();
        // Simulate a torn write that never got renamed; the real file must be
        // untouched and still read back cleanly.
        let tmp = p.with_extension("json.tmp");
        fs::write(&tmp, b"GARBAGE-half-write").unwrap();
        let recs = read_iterations(&p).unwrap();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].iter, 0);
    }

    #[test]
    fn rewrite_iterations_empty_truncates() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("iterations.jsonl");
        append_iteration(&p, &sample_record(0)).unwrap();
        rewrite_iterations(&p, &[]).unwrap();
        let recs = read_iterations(&p).unwrap();
        assert!(
            recs.is_empty(),
            "expected empty after rewrite, got {recs:?}"
        );
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
    fn worst_sentinel_round_trips_through_json() {
        // The finite sentinels used by `fail_mode = "worst"` (f64::MAX for
        // direction = min, f64::MIN for direction = max) must survive a
        // JSON round-trip — serde_json serializes non-finite f64 as `null`,
        // which Option<f64> would then read back as None, silently breaking
        // the next iteration's improvement comparison.
        for sentinel in [f64::MAX, f64::MIN] {
            let mut rec = sample_record(1);
            rec.score = Some(sentinel);
            rec.best_so_far = Some(sentinel);
            let line = serde_json::to_string(&rec).unwrap();
            assert!(
                !line.contains("null"),
                "sentinel {sentinel} serialized as null: {line}"
            );
            let back: IterationRecord = serde_json::from_str(&line).unwrap();
            assert_eq!(back.score, Some(sentinel));
            assert_eq!(back.best_so_far, Some(sentinel));
        }

        let mut state = sample_state();
        state.best_score = Some(f64::MAX);
        let text = serde_json::to_string(&state).unwrap();
        assert!(!text.contains("\"best_score\":null"), "got: {text}");
        let back: StateSnapshot = serde_json::from_str(&text).unwrap();
        assert_eq!(back.best_score, Some(f64::MAX));
    }

    #[test]
    fn pre_a2_record_without_summary_loads_with_empty_summary() {
        // A line written before the `summary` field existed must still parse,
        // defaulting `summary` to "".
        let dir = tempdir().unwrap();
        let p = dir.path().join("iterations.jsonl");
        let legacy = r#"{"iter":1,"started_at":"2026-05-20T08:00:00Z","ended_at":"2026-05-20T08:01:00Z","outcome":"merged","score":2.5,"best_so_far":2.5,"agent_exit":0,"agent_killed_by_budget":false,"diff_lines":4,"notes":"first valid score: 2.500000"}"#;
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&p)
            .unwrap();
        f.write_all(legacy.as_bytes()).unwrap();
        f.write_all(b"\n").unwrap();
        f.sync_all().unwrap();
        let recs = read_iterations(&p).unwrap();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].summary, "");
        assert_eq!(recs[0].notes, "first valid score: 2.500000");
    }

    fn sample_guidance(text: &str, at: Option<u64>) -> GuidanceEntry {
        GuidanceEntry {
            ts: Utc.with_ymd_and_hms(2026, 5, 20, 8, 0, 0).unwrap(),
            added_at_iter: at,
            text: text.to_string(),
        }
    }

    #[test]
    fn guidance_round_trips() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("guidance.jsonl");
        append_guidance(&p, &sample_guidance("try a spigot algorithm", Some(6))).unwrap();
        append_guidance(&p, &sample_guidance("stop tuning the series", None)).unwrap();
        let got = read_guidance(&p).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].text, "try a spigot algorithm");
        assert_eq!(got[0].added_at_iter, Some(6));
        assert_eq!(got[1].text, "stop tuning the series");
        assert_eq!(got[1].added_at_iter, None);
    }

    #[test]
    fn read_guidance_missing_returns_empty() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("guidance.jsonl");
        assert!(read_guidance(&p).unwrap().is_empty());
    }

    #[test]
    fn read_guidance_drops_torn_final_line() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("guidance.jsonl");
        append_guidance(&p, &sample_guidance("good", Some(1))).unwrap();
        // A torn half-record with no trailing newline (a `tell` killed mid-write).
        let mut f = OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all(b"{\"ts\":\"2026-05-20T08:00:00Z\",\"added_at_iter\":")
            .unwrap();
        f.sync_all().unwrap();
        let got = read_guidance(&p).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].text, "good");
    }

    #[test]
    fn read_guidance_accepts_hand_written_line() {
        // A human can append a line by hand; whitespace-only/blank lines are
        // skipped and a well-formed object is read.
        let dir = tempdir().unwrap();
        let p = dir.path().join("guidance.jsonl");
        let hand = "\n{\"ts\":\"2026-05-20T08:00:00Z\",\"added_at_iter\":null,\"text\":\"hand edited\"}\n\n";
        fs::write(&p, hand).unwrap();
        let got = read_guidance(&p).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].text, "hand edited");
        assert_eq!(got[0].added_at_iter, None);
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
