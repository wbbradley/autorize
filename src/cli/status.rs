use std::env;

use chrono::Utc;

use crate::{
    error::{Error, Result},
    experiment::ExperimentPaths,
    schedule::Deadline,
    storage::{self, IterationRecord, Outcome, StateSnapshot},
};

#[derive(clap::Args, Debug)]
/// Print a one-shot summary of an experiment's progress: iteration count,
/// best score, last outcome, elapsed wall-clock, and time remaining.
pub struct StatusArgs {
    /// Experiment name.
    pub name: String,
}

pub fn run(args: StatusArgs) -> anyhow::Result<()> {
    let project_root = env::current_dir()?;
    run_with_root(args, project_root)?;
    Ok(())
}

fn run_with_root(args: StatusArgs, project_root: std::path::PathBuf) -> Result<()> {
    let paths = ExperimentPaths::new(project_root, args.name.clone());
    let state = storage::read_state(&paths.state_path())?.ok_or_else(|| {
        Error::Config(format!(
            "no state.json for experiment {:?}; has it been started?",
            args.name
        ))
    })?;
    let records = storage::read_iterations(&paths.iterations_log())?;
    let out = format_summary(&state, &records);
    print!("{out}");
    Ok(())
}

fn format_summary(state: &StateSnapshot, records: &[IterationRecord]) -> String {
    let mut s = String::new();
    let now = Utc::now();
    let elapsed = (now - state.started_at).to_std().unwrap_or_default();
    let remaining = Deadline(state.deadline).remaining(now);
    let last = records.last();
    let last_outcome = last.map(|r| outcome_label(r.outcome)).unwrap_or("(none)");
    s.push_str(&format!("experiment   {}\n", state.experiment));
    s.push_str(&format!("branch       {}\n", state.branch));
    s.push_str(&format!("base_commit  {}\n", state.base_commit));
    s.push_str(&format!(
        "iterations   {} total, {} this run\n",
        state.iterations_completed, state.run_iterations_completed
    ));
    s.push_str(&format!("noop streak  {}\n", state.consecutive_noops));
    s.push_str(&format!("last outcome {last_outcome}\n"));
    if let Some(r) = last
        && !r.notes.is_empty()
    {
        s.push_str(&format!("last reason   {}\n", r.notes));
    }
    match (state.best_iter, state.best_score) {
        (Some(i), Some(sc)) => s.push_str(&format!("best         iter {i}, score {sc:.6}\n")),
        _ => s.push_str("best         (none)\n"),
    }
    s.push_str(&format!(
        "elapsed      {}\n",
        humantime::format_duration(elapsed)
    ));
    s.push_str(&format!(
        "remaining    {}\n",
        humantime::format_duration(remaining)
    ));
    if let Some(ip) = state.iter_in_progress {
        s.push_str(&format!(
            "in progress  iter {ip} at step {:?}\n",
            state.current_step
        ));
    }
    s
}

fn outcome_label(o: Outcome) -> &'static str {
    match o {
        Outcome::Merged => "merged",
        Outcome::Discarded => "discarded",
        Outcome::Noop => "noop",
        Outcome::Invalid => "invalid",
        Outcome::Killed => "killed",
        Outcome::Denied => "denied",
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use tempfile::tempdir;

    use super::*;
    use crate::storage::{CurrentStep, IterationRecord, StateSnapshot};

    fn sample_state() -> StateSnapshot {
        let now = Utc::now();
        StateSnapshot {
            experiment: "pi".to_string(),
            branch: "autorize/pi".to_string(),
            base_commit: "abc1234".to_string(),
            iter_in_progress: None,
            current_step: CurrentStep::Idle,
            best_score: None,
            best_iter: None,
            started_at: now,
            deadline: now + chrono::Duration::seconds(3600),
            iterations_completed: 0,
            run_iterations_completed: 0,
            consecutive_noops: 0,
        }
    }

    fn sample_record(iter: u64, outcome: Outcome) -> IterationRecord {
        IterationRecord {
            iter,
            started_at: Utc.with_ymd_and_hms(2026, 5, 20, 8, 0, 0).unwrap(),
            ended_at: Utc.with_ymd_and_hms(2026, 5, 20, 8, 1, 0).unwrap(),
            outcome,
            score: Some(2.5),
            best_so_far: Some(2.5),
            agent_exit: Some(0),
            agent_killed_by_budget: false,
            diff_lines: 4,
            notes: String::new(),
        }
    }

    #[test]
    fn status_prints_no_iterations() {
        let state = sample_state();
        let out = format_summary(&state, &[]);
        assert!(
            out.contains("iterations   0 total, 0 this run"),
            "got: {out}"
        );
        assert!(out.contains("last outcome (none)"), "got: {out}");
        assert!(out.contains("best         (none)"), "got: {out}");
    }

    #[test]
    fn status_prints_best() {
        let mut state = sample_state();
        state.best_iter = Some(5);
        state.best_score = Some(1.23456);
        state.iterations_completed = 7;
        state.run_iterations_completed = 3;
        let records = vec![sample_record(7, Outcome::Merged)];
        let out = format_summary(&state, &records);
        assert!(out.contains("iter 5"), "got: {out}");
        assert!(out.contains("1.234560"), "got: {out}");
        assert!(
            out.contains("iterations   7 total, 3 this run"),
            "got: {out}"
        );
        assert!(out.contains("last outcome merged"), "got: {out}");
    }

    #[test]
    fn status_prints_last_reason() {
        let mut state = sample_state();
        state.best_iter = Some(2);
        state.best_score = Some(3.15);
        let mut last = sample_record(2, Outcome::Discarded);
        last.notes = "regressed: 3.150000 vs best 3.140000 (min)".to_string();
        let out = format_summary(&state, &[last]);
        assert!(
            out.contains("last reason   regressed: 3.150000 vs best 3.140000 (min)"),
            "got: {out}"
        );
    }

    #[test]
    fn status_omits_reason_when_notes_empty() {
        let state = sample_state();
        // sample_record has empty notes.
        let out = format_summary(&state, &[sample_record(1, Outcome::Merged)]);
        assert!(!out.contains("last reason"), "got: {out}");
    }

    #[test]
    fn status_errors_when_state_missing() {
        let tmp = tempdir().unwrap();
        let err = run_with_root(
            StatusArgs {
                name: "missing".to_string(),
            },
            tmp.path().to_path_buf(),
        )
        .unwrap_err();
        assert!(format!("{err}").contains("state.json"), "got: {err}");
    }
}
