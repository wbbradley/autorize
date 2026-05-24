use std::{env, fs, path::PathBuf};

use chrono::{Local, Utc};
use regex::Regex;
use tracing::info;

use crate::{
    config::{Config, Direction},
    error::{Error, Result},
    experiment::ExperimentPaths,
    iteration::{self, IterationInputs},
    lock::ExperimentLock,
    schedule::{self, Deadline},
    storage::{self, CurrentStep, IterationRecord, Outcome, StateSnapshot},
    worktree::Git,
};

#[derive(clap::Args, Debug)]
/// Run the iterative-improvement loop until the deadline, iteration cap,
/// or consecutive-noop cap fires.
///
/// On first invocation autorize creates the `autorize/<name>` tracking branch
/// at HEAD and persists `state.json`. Subsequent `autorize run` invocations
/// continue from the saved state (use `autorize resume` to recover after a
/// mid-iteration crash).
pub struct RunArgs {
    /// Experiment name (must already exist under `.autorize/<name>/`).
    pub name: String,
    /// Proceed even if the working tree has uncommitted changes
    /// (excluding `.autorize/` which is always ignored).
    #[arg(long)]
    pub allow_dirty: bool,
    /// Start another run on a finished experiment, building on the prior best.
    /// Recomputes the deadline from `schedule`, and resets the per-run
    /// iteration budget (`max_iterations`) and the consecutive-noop streak,
    /// while preserving `best_score`/`best_iter`, the `autorize/<name>` branch
    /// tip, and the full `iterations.jsonl` history. A no-op on a never-run
    /// experiment; refused (use `autorize resume`) if an iteration is in
    /// progress.
    #[arg(long)]
    pub fresh: bool,
}

pub fn run(args: RunArgs) -> anyhow::Result<()> {
    let project_root = env::current_dir()?;
    run_loop(args.name, args.allow_dirty, project_root, false, args.fresh)?;
    Ok(())
}

/// Shared body used by both `autorize run` and `autorize resume`. When
/// `recover_iter` is true, an in-progress iteration found in state.json
/// is recorded as `killed` and the loop continues; otherwise the loop
/// refuses and points the user at `autorize resume`.
///
/// When `fresh` is true (only `autorize run --fresh`; `resume` passes false)
/// and a clean (non-in-progress) state already exists, the run-level stop
/// conditions are reset before the loop: the deadline is recomputed from
/// `schedule`, and `consecutive_noops` / `run_iterations_completed` /
/// `started_at` are reset, while best score, branch tip, and history are
/// preserved. `fresh` is a no-op when there is no existing state.
pub(crate) fn run_loop(
    name: String,
    allow_dirty: bool,
    project_root: PathBuf,
    recover_iter: bool,
    fresh: bool,
) -> Result<()> {
    let paths = ExperimentPaths::new(project_root, name.clone());
    if !paths.root().exists() {
        return Err(Error::Config(format!(
            "experiment {name:?} not found at {:?}; run `autorize init {name}` first",
            paths.root()
        )));
    }

    let _lock = ExperimentLock::acquire(&paths.lock_path())?;

    let cfg = paths.load_config()?;
    let program_md = paths.load_program()?;
    let git = Git::new(paths.project_root().clone());

    if !git.is_inside_repo()? {
        return Err(Error::Git(
            "not a git repository (cd into one or `git init`)".to_string(),
        ));
    }
    // `.autorize/` is the harness's own bookkeeping and `logs/` is the central
    // run log it creates on startup; neither should trip the dirty-tree guard.
    if !allow_dirty && !git.is_clean_excluding(&[".autorize/", "logs/"])? {
        return Err(Error::Git(
            "working tree has uncommitted changes; pass --allow-dirty to override".to_string(),
        ));
    }

    let branch = format!("autorize/{name}");

    let mut state = match storage::read_state(&paths.state_path())? {
        None => {
            if recover_iter {
                return Err(Error::Config(format!(
                    "no state.json for experiment {name:?}; nothing to resume — run `autorize run {name}` first"
                )));
            }
            let deadline = schedule::compute_deadline(&cfg.schedule, Utc::now(), Local::now())?;
            let base_commit = git.head_sha()?;
            if !git.branch_exists(&branch)? {
                git.create_branch_at(&branch, &base_commit)?;
            }
            let now = Utc::now();
            let state = StateSnapshot {
                experiment: name.clone(),
                branch: branch.clone(),
                base_commit,
                iter_in_progress: None,
                current_step: CurrentStep::Idle,
                best_score: None,
                best_iter: None,
                started_at: now,
                deadline: deadline.at(),
                iterations_completed: 0,
                run_iterations_completed: 0,
                consecutive_noops: 0,
            };
            info!("mkdir -p {}", paths.root().display());
            fs::create_dir_all(paths.root())?;
            storage::write_state(&paths.state_path(), &state)?;
            state
        }
        Some(s) => {
            if git.resolve_ref(&s.base_commit)?.is_none() {
                return Err(Error::Git(format!(
                    "base_commit {} unreachable; aborting",
                    s.base_commit
                )));
            }
            let mut s = s;
            if s.iter_in_progress.is_some() {
                if !recover_iter {
                    return Err(Error::Config(
                        "in-progress iteration found; use `autorize resume`".to_string(),
                    ));
                }
                reconcile_in_progress(&paths, &mut s, &git, &cfg)?;
            }
            // `--fresh` only ever reaches here on a clean (non-in-progress)
            // state, since the in-progress branch above errors out for a plain
            // `run` (recover_iter is false). `resume` always passes fresh=false.
            if fresh {
                apply_fresh_reset(&paths, &mut s, &cfg)?;
            }
            s
        }
    };

    let deadline = Deadline(state.deadline);

    let mut records = storage::read_iterations(&paths.iterations_log())?;

    // When `[summarize]` is enabled, generate summaries for any records still
    // missing one (written before the step was enabled, or whose summarize
    // step failed). Best-effort: a failure here must not sink an otherwise
    // healthy run, and mutating `records` in place means the first prompt's
    // recent-iterations slice immediately reflects the backfilled summaries.
    if let Err(e) = iteration::backfill_missing_summaries(&cfg, &paths, &mut records) {
        tracing::warn!("backfill of missing summaries failed ({e}); continuing");
    }

    loop {
        let now = Utc::now();
        if deadline.is_expired(now) {
            info!("deadline reached at {now}; stopping.");
            break;
        }
        if cfg.iteration.max_iterations > 0
            && state.run_iterations_completed >= cfg.iteration.max_iterations
        {
            info!(
                "reached max_iterations={}; stopping.",
                cfg.iteration.max_iterations
            );
            break;
        }
        if state.consecutive_noops >= cfg.iteration.max_consecutive_noops {
            info!(
                "reached max_consecutive_noops={}; stopping.",
                cfg.iteration.max_consecutive_noops
            );
            break;
        }

        let next_iter = next_iter_number(&state, &records);
        let recent = recent_slice(&records, 10);
        let best = match (state.best_score, state.best_iter) {
            (Some(s), Some(i)) => Some((s, i)),
            _ => None,
        };
        // Re-read operator guidance every iteration so a concurrent
        // `autorize tell` (or a hand-edit of guidance.jsonl) is picked up. A
        // read error (e.g. a malformed hand-edit) is non-fatal: an expensive
        // run must not die over auxiliary steering input.
        let guidance = storage::read_guidance(&paths.guidance_path()).unwrap_or_else(|e| {
            tracing::warn!("failed to read guidance.jsonl ({e}); ignoring this iteration");
            Vec::new()
        });

        let inputs = IterationInputs {
            cfg: &cfg,
            paths: &paths,
            git: &git,
            branch: &branch,
            iter: next_iter,
            best,
            recent: &recent,
            program_md: &program_md,
            guidance: &guidance,
        };

        let rec = iteration::run_iteration(&inputs, &mut state)?;
        let score_str = rec
            .score
            .map(|s| format!("{s:.6}"))
            .unwrap_or_else(|| "(none)".to_string());
        let best_str = state
            .best_score
            .map(|s| format!("{s:.6}"))
            .unwrap_or_else(|| "(none)".to_string());
        info!(
            "iter {}: {} score={} best={}",
            rec.iter,
            outcome_label(rec.outcome),
            score_str,
            best_str
        );
        records.push(rec);
    }

    print_final_summary(&state);
    Ok(())
}

fn next_iter_number(state: &StateSnapshot, records: &[IterationRecord]) -> u64 {
    let from_records = records.iter().map(|r| r.iter).max().unwrap_or(0);
    let from_state = state.iter_in_progress.unwrap_or(0);
    from_records.max(from_state) + 1
}

fn recent_slice(records: &[IterationRecord], n: usize) -> Vec<IterationRecord> {
    let len = records.len();
    let start = len.saturating_sub(n);
    records[start..].to_vec()
}

/// Reset the run-level stop conditions for `autorize run --fresh`, preserving
/// all prior progress (best score, branch tip, history, lifetime count). Errors
/// if the recomputed deadline is already in the past (only a hard past RFC3339
/// `schedule.deadline` trips this — durations / `total_budget` recompute fine),
/// rather than entering a loop that exits immediately. Persists the mutated
/// state so a crash right after `--fresh` does not re-trigger the old deadline.
fn apply_fresh_reset(
    paths: &ExperimentPaths,
    state: &mut StateSnapshot,
    cfg: &Config,
) -> Result<()> {
    let now = Utc::now();
    let deadline = schedule::compute_deadline(&cfg.schedule, now, Local::now())?;
    if deadline.is_expired(now) {
        return Err(Error::Schedule(format!(
            "schedule.deadline {:?} is in the past; edit config.toml or switch to \
             total_budget before `--fresh`",
            cfg.schedule.deadline.as_deref().unwrap_or("")
        )));
    }
    state.deadline = deadline.at();
    state.consecutive_noops = 0;
    state.run_iterations_completed = 0;
    state.started_at = now;
    info!(
        "--fresh: reset run budget and deadline (now {now}); preserving best={}",
        match (state.best_iter, state.best_score) {
            (Some(i), Some(s)) => format!("iter {i} score {s:.6}"),
            _ => "(none)".to_string(),
        }
    );
    storage::write_state(&paths.state_path(), state)?;
    Ok(())
}

/// Resume-time reconciliation for an in-progress iteration. Picks one of
/// three branches based on what's actually on disk and on the tracking
/// branch:
///
/// - Case A — `iterations.jsonl` already holds a record for this iter:
///   replay it into `state` (no new record written).
/// - Case B — the tracking branch tip is `autorize iter <N>: score <S>`
///   with `N == iter_in_progress`: the merge landed but the record didn't.
///   Synthesize a `Merged` record with the parsed score and apply it.
/// - Case C — neither holds: behave as before, recording a `killed`.
fn reconcile_in_progress(
    paths: &ExperimentPaths,
    state: &mut StateSnapshot,
    git: &Git,
    cfg: &Config,
) -> Result<()> {
    let iter = state.iter_in_progress.ok_or_else(|| {
        Error::Config("reconcile_in_progress called without in-progress iter".into())
    })?;

    // Idempotent; the worktree may or may not exist depending on where the
    // crash hit.
    let wt = paths.iter_dir(iter).join("wt");
    let _ = git.worktree_remove(&wt);

    let existing = storage::read_iterations(&paths.iterations_log())?;
    if let Some(rec) = existing.into_iter().find(|r| r.iter == iter) {
        return replay_existing_record(paths, state, &rec, cfg);
    }

    let subject = git.log_subject(&state.branch)?;
    if let Some(score) = parse_merge_subject(&subject, iter) {
        return synthesize_merged_record(paths, state, iter, score, cfg);
    }

    record_killed(paths, state, iter)
}

fn replay_existing_record(
    paths: &ExperimentPaths,
    state: &mut StateSnapshot,
    rec: &IterationRecord,
    cfg: &Config,
) -> Result<()> {
    if rec.outcome == Outcome::Merged
        && let Some(s) = rec.score
    {
        apply_score_to_best(state, rec.iter, s, cfg.objective.direction);
    }
    state.iter_in_progress = None;
    state.current_step = CurrentStep::Idle;
    state.iterations_completed += 1;
    // A reconciled `killed` record does not count toward the per-run budget
    // (a crash should not burn a `max_iterations` slot); any other replayed
    // outcome was a real completed iteration in this run.
    if rec.outcome != Outcome::Killed {
        state.run_iterations_completed += 1;
    }
    if rec.outcome == Outcome::Noop {
        state.consecutive_noops += 1;
    } else {
        state.consecutive_noops = 0;
    }
    storage::write_state(&paths.state_path(), state)?;
    Ok(())
}

fn synthesize_merged_record(
    paths: &ExperimentPaths,
    state: &mut StateSnapshot,
    iter: u64,
    score: f64,
    cfg: &Config,
) -> Result<()> {
    apply_score_to_best(state, iter, score, cfg.objective.direction);

    let now = Utc::now();
    let rec = IterationRecord {
        iter,
        started_at: now,
        ended_at: now,
        outcome: Outcome::Merged,
        score: Some(score),
        best_so_far: Some(score),
        agent_exit: None,
        agent_killed_by_budget: false,
        diff_lines: 0,
        notes: "reconciled from branch tip after crash".to_string(),
        summary: String::new(),
    };
    storage::append_iteration(&paths.iterations_log(), &rec)?;

    state.iter_in_progress = None;
    state.current_step = CurrentStep::Idle;
    state.iterations_completed += 1;
    state.run_iterations_completed += 1;
    state.consecutive_noops = 0;
    storage::write_state(&paths.state_path(), state)?;
    Ok(())
}

fn record_killed(paths: &ExperimentPaths, state: &mut StateSnapshot, iter: u64) -> Result<()> {
    let now = Utc::now();
    let rec = IterationRecord {
        iter,
        started_at: now,
        ended_at: now,
        outcome: Outcome::Killed,
        score: None,
        best_so_far: state.best_score,
        agent_exit: None,
        agent_killed_by_budget: false,
        diff_lines: 0,
        notes: "resumed after crash".to_string(),
        summary: String::new(),
    };
    storage::append_iteration(&paths.iterations_log(), &rec)?;

    state.iter_in_progress = None;
    state.current_step = CurrentStep::Idle;
    // A `killed` record is a jsonl record (so it bumps the lifetime count) but
    // it represents an abandoned, crashed iteration — it must NOT consume a
    // per-run `max_iterations` slot, so `run_iterations_completed` is untouched.
    state.iterations_completed += 1;
    storage::write_state(&paths.state_path(), state)?;
    Ok(())
}

fn apply_score_to_best(state: &mut StateSnapshot, iter: u64, score: f64, direction: Direction) {
    let improved = match (state.best_score, direction) {
        (None, _) => true,
        (Some(b), Direction::Min) => score < b,
        (Some(b), Direction::Max) => score > b,
    };
    if improved {
        state.best_score = Some(score);
        state.best_iter = Some(iter);
    }
}

/// Parse the score out of a commit subject of the form
/// `autorize iter <N>: score <S>`, returning `Some(S)` iff `<N>` equals
/// `expected_iter` and `<S>` is a parseable f64.
fn parse_merge_subject(subject: &str, expected_iter: u64) -> Option<f64> {
    let re = Regex::new(r"^autorize iter (\d+): score (\S+)$").ok()?;
    let caps = re.captures(subject.trim())?;
    let parsed_iter: u64 = caps.get(1)?.as_str().parse().ok()?;
    if parsed_iter != expected_iter {
        return None;
    }
    caps.get(2)?.as_str().parse::<f64>().ok()
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

fn print_final_summary(state: &StateSnapshot) {
    let best = match (state.best_iter, state.best_score) {
        (Some(i), Some(s)) => format!("iter {i}, score {s:.6}"),
        _ => "(none)".to_string(),
    };
    info!(
        "run complete: experiment={} iterations={} best={best}",
        state.experiment, state.iterations_completed
    );
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        fs,
        os::unix::fs::PermissionsExt,
        path::Path,
        process::Command,
        time::Duration,
    };

    use chrono::Utc;
    use tempfile::{TempDir, tempdir};

    use super::*;
    use crate::{
        config::{
            Agent,
            AgentStdin,
            Boundaries,
            Config,
            Direction,
            Experiment,
            FailMode,
            Iteration,
            Objective,
            ParseSpec,
            Schedule,
            Setup,
            Summarize,
            Teardown,
        },
        storage::CurrentStep,
    };

    fn run_cmd(args: &[&str], cwd: &Path) {
        let st = Command::new(args[0])
            .args(&args[1..])
            .current_dir(cwd)
            .status()
            .unwrap_or_else(|e| panic!("spawning {args:?} failed: {e}"));
        assert!(st.success(), "command {args:?} failed: {st:?}");
    }

    const SCORE_SH: &str = r#"#!/bin/sh
v=$(cat value.txt)
awk -v x="$v" 'BEGIN { pi=3.141592653589793; d=x-pi; if (d<0) d=-d; printf "%f\n", d }'
"#;

    fn init_test_repo() -> TempDir {
        let tmp = tempdir().unwrap();
        let p = tmp.path();
        run_cmd(&["git", "init", "-q", "-b", "main"], p);
        run_cmd(&["git", "config", "user.email", "test@example.com"], p);
        run_cmd(&["git", "config", "user.name", "Test"], p);

        fs::write(p.join("value.txt"), "3.0\n").unwrap();
        let score_path = p.join("score.sh");
        fs::write(&score_path, SCORE_SH).unwrap();
        let mut perms = fs::metadata(&score_path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&score_path, perms).unwrap();

        run_cmd(&["git", "add", "."], p);
        run_cmd(&["git", "commit", "-qm", "init"], p);
        tmp
    }

    fn write_experiment(
        tmp: &Path,
        name: &str,
        agent_cmd: &str,
        objective_cmd: &str,
        total_budget: Duration,
        max_iterations: u64,
    ) -> PathBuf {
        let root = tmp.join(".autorize").join(name);
        fs::create_dir_all(&root).unwrap();
        let cfg = make_config(agent_cmd, objective_cmd, total_budget, max_iterations);
        let cfg_toml = toml::to_string(&cfg).unwrap();
        fs::write(root.join("config.toml"), cfg_toml).unwrap();
        fs::write(root.join("program.md"), "# test program\n").unwrap();
        root
    }

    fn make_config(
        agent_cmd: &str,
        objective_cmd: &str,
        total_budget: Duration,
        max_iterations: u64,
    ) -> Config {
        Config {
            experiment: Experiment {
                name: "test".to_string(),
                description: String::new(),
            },
            objective: Objective {
                command: objective_cmd.to_string(),
                direction: Direction::Min,
                parse: ParseSpec::Float,
                timeout: Duration::from_secs(30),
                fail_mode: FailMode::Invalid,
            },
            boundaries: Boundaries {
                allow_paths: vec![],
                deny_paths: vec![],
            },
            setup: Setup::default(),
            teardown: Teardown::default(),
            iteration: Iteration {
                budget: Duration::from_secs(30),
                max_iterations,
                keep_worktrees: false,
                max_consecutive_noops: 5,
            },
            schedule: Schedule {
                total_budget: Some(total_budget),
                deadline: None,
            },
            agent: Agent {
                command: agent_cmd.to_string(),
                workdir_var: "AUTORIZE_WORKDIR".to_string(),
                env: BTreeMap::new(),
                stdin: AgentStdin::Prompt,
            },
            summarize: Summarize::default(),
        }
    }

    fn seed_state(
        root: &Path,
        name: &str,
        base_commit: &str,
        iter_in_progress: Option<u64>,
        iterations_completed: u64,
    ) {
        let now = Utc::now();
        let state = StateSnapshot {
            experiment: name.to_string(),
            branch: format!("autorize/{name}"),
            base_commit: base_commit.to_string(),
            iter_in_progress,
            current_step: if iter_in_progress.is_some() {
                CurrentStep::InvokeAgent
            } else {
                CurrentStep::Idle
            },
            best_score: None,
            best_iter: None,
            started_at: now,
            deadline: now + chrono::Duration::seconds(3600),
            iterations_completed,
            run_iterations_completed: iterations_completed,
            consecutive_noops: 0,
        };
        storage::write_state(&root.join("state.json"), &state).unwrap();
    }

    #[test]
    fn refuses_dirty_repo() {
        let tmp = init_test_repo();
        write_experiment(
            tmp.path(),
            "test",
            "true",
            "bash score.sh",
            Duration::from_secs(60),
            1,
        );
        // Make the tree dirty outside .autorize/
        fs::write(tmp.path().join("stray.txt"), "x\n").unwrap();

        let err = run_loop(
            "test".to_string(),
            false,
            tmp.path().to_path_buf(),
            false,
            false,
        )
        .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("uncommitted"), "got: {msg}");
    }

    #[test]
    fn allows_dirty_with_flag() {
        let tmp = init_test_repo();
        write_experiment(
            tmp.path(),
            "test",
            "true",
            "bash score.sh",
            Duration::from_secs(60),
            1,
        );
        fs::write(tmp.path().join("stray.txt"), "x\n").unwrap();

        // Should not error on dirty when allow_dirty=true.
        run_loop(
            "test".to_string(),
            true,
            tmp.path().to_path_buf(),
            false,
            false,
        )
        .unwrap();
    }

    #[test]
    fn tolerates_dirty_autorize_dir() {
        let tmp = init_test_repo();
        write_experiment(
            tmp.path(),
            "test",
            "true",
            "bash score.sh",
            Duration::from_secs(60),
            1,
        );
        // Untracked file inside .autorize/ should not flag the tree dirty.
        fs::write(
            tmp.path()
                .join(".autorize")
                .join("test")
                .join("scratch.txt"),
            "x\n",
        )
        .unwrap();
        run_loop(
            "test".to_string(),
            false,
            tmp.path().to_path_buf(),
            false,
            false,
        )
        .unwrap();
    }

    #[test]
    fn refuses_unreachable_base_commit() {
        let tmp = init_test_repo();
        let root = write_experiment(
            tmp.path(),
            "test",
            "true",
            "bash score.sh",
            Duration::from_secs(60),
            1,
        );
        seed_state(
            &root,
            "test",
            "0000000000000000000000000000000000000000",
            None,
            0,
        );

        let err = run_loop(
            "test".to_string(),
            false,
            tmp.path().to_path_buf(),
            false,
            false,
        )
        .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("unreachable"), "got: {msg}");
    }

    #[test]
    fn refuses_in_progress_without_resume() {
        let tmp = init_test_repo();
        let root = write_experiment(
            tmp.path(),
            "test",
            "true",
            "bash score.sh",
            Duration::from_secs(60),
            1,
        );
        let git = Git::new(tmp.path().to_path_buf());
        let sha = git.head_sha().unwrap();
        seed_state(&root, "test", &sha, Some(3), 2);

        let err = run_loop(
            "test".to_string(),
            false,
            tmp.path().to_path_buf(),
            false,
            false,
        )
        .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("resume"), "got: {msg}");
    }

    #[test]
    fn fresh_run_creates_branch_and_state() {
        let tmp = init_test_repo();
        write_experiment(
            tmp.path(),
            "test",
            "true",
            "bash score.sh",
            Duration::from_millis(50),
            0,
        );
        run_loop(
            "test".to_string(),
            false,
            tmp.path().to_path_buf(),
            false,
            false,
        )
        .unwrap();

        let state_path = tmp.path().join(".autorize").join("test").join("state.json");
        assert!(state_path.exists(), "state.json should exist");
        let git = Git::new(tmp.path().to_path_buf());
        assert!(git.branch_exists("autorize/test").unwrap());
    }

    #[test]
    fn refuses_concurrent_run() {
        let tmp = init_test_repo();
        let root = write_experiment(
            tmp.path(),
            "test",
            "true",
            "bash score.sh",
            Duration::from_secs(60),
            1,
        );
        let lock_path = root.join("run.lock");
        let _held = crate::lock::ExperimentLock::acquire(&lock_path).unwrap();

        let err = run_loop(
            "test".to_string(),
            false,
            tmp.path().to_path_buf(),
            false,
            false,
        )
        .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("lock"), "got: {msg}");
    }

    #[test]
    fn lock_released_after_successful_run() {
        let tmp = init_test_repo();
        write_experiment(
            tmp.path(),
            "test",
            "true",
            "bash score.sh",
            Duration::from_millis(50),
            0,
        );
        run_loop(
            "test".to_string(),
            false,
            tmp.path().to_path_buf(),
            false,
            false,
        )
        .unwrap();
        let lock_path = tmp.path().join(".autorize/test/run.lock");
        let _l = crate::lock::ExperimentLock::acquire(&lock_path).unwrap();
    }

    #[test]
    fn parse_merge_subject_matches_with_expected_iter() {
        let got = super::parse_merge_subject("autorize iter 7: score 0.123456", 7);
        assert_eq!(got, Some(0.123456));
    }

    #[test]
    fn parse_merge_subject_rejects_mismatched_iter() {
        assert!(super::parse_merge_subject("autorize iter 7: score 0.5", 8).is_none());
        // Substring `7` inside a longer iter number must not partially match.
        assert!(super::parse_merge_subject("autorize iter 77: score 0.5", 7).is_none());
    }

    #[test]
    fn parse_merge_subject_rejects_unrelated_subjects() {
        for s in [
            "iter 1: score 0.5",
            "autorize iter 1 score 0.5",
            "autorize: iter 1: score 0.5",
            "fix: something",
            "",
            "autorize iter 1: score not-a-number",
        ] {
            assert!(
                super::parse_merge_subject(s, 1).is_none(),
                "should reject {s:?}"
            );
        }
    }

    #[test]
    fn parse_merge_subject_round_trips_sentinel_floats() {
        for sentinel in [f64::MAX, f64::MIN] {
            let subj = format!("autorize iter 1: score {sentinel}");
            let got = super::parse_merge_subject(&subj, 1)
                .unwrap_or_else(|| panic!("failed to parse {subj:?}"));
            assert_eq!(got, sentinel);
        }
    }

    #[test]
    fn respects_max_iterations() {
        let tmp = init_test_repo();
        write_experiment(
            tmp.path(),
            "test",
            "echo improvement > value.txt",
            "bash score.sh",
            Duration::from_secs(60),
            2,
        );
        run_loop(
            "test".to_string(),
            false,
            tmp.path().to_path_buf(),
            false,
            false,
        )
        .unwrap();
        let log = tmp
            .path()
            .join(".autorize")
            .join("test")
            .join("iterations.jsonl");
        let recs = storage::read_iterations(&log).unwrap();
        assert_eq!(recs.len(), 2, "expected exactly 2 records, got {recs:?}");
    }

    #[test]
    fn keep_worktrees_does_not_block_second_iteration() {
        // Regression: with `keep_worktrees = true`, iter 1's worktree used to
        // keep the tracking branch checked out, so `git worktree add` for iter
        // 2 failed ("branch already used by worktree"). Detached worktrees fix
        // it. Both iterations must run.
        let tmp = init_test_repo();
        let root = write_experiment(
            tmp.path(),
            "test",
            "echo 3.14 > value.txt",
            "bash score.sh",
            Duration::from_secs(60),
            2,
        );
        let cfg_path = root.join("config.toml");
        let mut cfg: Config = toml::from_str(&fs::read_to_string(&cfg_path).unwrap()).unwrap();
        cfg.iteration.keep_worktrees = true;
        fs::write(&cfg_path, toml::to_string(&cfg).unwrap()).unwrap();

        run_loop(
            "test".to_string(),
            false,
            tmp.path().to_path_buf(),
            false,
            false,
        )
        .unwrap();

        let recs = storage::read_iterations(&root.join("iterations.jsonl")).unwrap();
        assert_eq!(recs.len(), 2, "both iterations should run, got {recs:?}");
        assert_eq!(recs[0].outcome, Outcome::Merged);
        // Both worktrees are kept on disk (keep_worktrees = true).
        assert!(root.join("iter-0001").join("wt").is_dir());
        assert!(root.join("iter-0002").join("wt").is_dir());
    }

    #[test]
    fn fresh_reruns_after_completion() {
        // (a) + (c): a completed `max_iterations` experiment re-runs another N
        // iterations under `--fresh` (and exits immediately without it), with
        // iter numbers continuing strictly past the prior max and the lifetime
        // counter climbing while the per-run counter resets.
        let tmp = init_test_repo();
        let root = write_experiment(
            tmp.path(),
            "test",
            "echo improvement > value.txt",
            "bash score.sh",
            Duration::from_secs(60),
            2,
        );

        // First run: max_iterations = 2 → exactly 2 records.
        run_loop(
            "test".to_string(),
            false,
            tmp.path().to_path_buf(),
            false,
            false,
        )
        .unwrap();
        let recs = storage::read_iterations(&root.join("iterations.jsonl")).unwrap();
        assert_eq!(recs.len(), 2, "first run should do 2 iters, got {recs:?}");
        let after_first = storage::read_state(&root.join("state.json"))
            .unwrap()
            .unwrap();
        assert_eq!(after_first.run_iterations_completed, 2);
        assert_eq!(after_first.iterations_completed, 2);

        // Re-run WITHOUT --fresh: the stop condition is already hit → no new work.
        run_loop(
            "test".to_string(),
            false,
            tmp.path().to_path_buf(),
            false,
            false,
        )
        .unwrap();
        let recs = storage::read_iterations(&root.join("iterations.jsonl")).unwrap();
        assert_eq!(
            recs.len(),
            2,
            "non-fresh re-run must not add records, got {recs:?}"
        );

        // Re-run WITH --fresh: another (up to) 2 iterations, continuing numbering.
        run_loop(
            "test".to_string(),
            false,
            tmp.path().to_path_buf(),
            false,
            true,
        )
        .unwrap();
        let recs = storage::read_iterations(&root.join("iterations.jsonl")).unwrap();
        assert_eq!(
            recs.len(),
            4,
            "fresh re-run should add 2 more, got {recs:?}"
        );
        for (idx, rec) in recs.iter().enumerate() {
            assert_eq!(
                rec.iter,
                idx as u64 + 1,
                "iter numbers must be 1..=N strict; rec={rec:?}"
            );
        }
        let after_fresh = storage::read_state(&root.join("state.json"))
            .unwrap()
            .unwrap();
        assert_eq!(
            after_fresh.iterations_completed, 4,
            "lifetime count keeps climbing"
        );
        assert_eq!(
            after_fresh.run_iterations_completed, 2,
            "per-run count reset on --fresh then re-grew to the cap"
        );
    }

    #[test]
    fn fresh_preserves_best_and_discards_regression() {
        // (b) + (d): --fresh keeps the prior best, the first new iteration
        // compares against it (a regression is `discarded`, not `merged`), and
        // --fresh itself never moves the tracking-branch tip.
        let tmp = init_test_repo();
        let root = write_experiment(
            tmp.path(),
            "test",
            "echo 3.14 > value.txt",
            "bash score.sh",
            Duration::from_secs(60),
            1,
        );

        run_loop(
            "test".to_string(),
            false,
            tmp.path().to_path_buf(),
            false,
            false,
        )
        .unwrap();
        let after_first = storage::read_state(&root.join("state.json"))
            .unwrap()
            .unwrap();
        let best_score = after_first.best_score.expect("first run should set best");
        assert_eq!(after_first.best_iter, Some(1));

        let git = Git::new(tmp.path().to_path_buf());
        let tip_before = git.resolve_ref("autorize/test").unwrap().unwrap();

        // Swap in an agent whose change scores WORSE than the prior best.
        let cfg_path = root.join("config.toml");
        let mut cfg: Config = toml::from_str(&fs::read_to_string(&cfg_path).unwrap()).unwrap();
        cfg.agent.command = "echo 2.0 > value.txt".to_string();
        fs::write(&cfg_path, toml::to_string(&cfg).unwrap()).unwrap();

        run_loop(
            "test".to_string(),
            false,
            tmp.path().to_path_buf(),
            false,
            true,
        )
        .unwrap();

        let recs = storage::read_iterations(&root.join("iterations.jsonl")).unwrap();
        assert_eq!(
            recs.len(),
            2,
            "fresh run should append one record, got {recs:?}"
        );
        assert_eq!(recs[1].iter, 2);
        assert_eq!(
            recs[1].outcome,
            Outcome::Discarded,
            "iter 2 regressed vs the prior best and must be discarded"
        );

        let after_fresh = storage::read_state(&root.join("state.json"))
            .unwrap()
            .unwrap();
        assert_eq!(
            after_fresh.best_score,
            Some(best_score),
            "best_score preserved across --fresh"
        );
        assert_eq!(
            after_fresh.best_iter,
            Some(1),
            "best_iter preserved across --fresh"
        );

        let tip_after = git.resolve_ref("autorize/test").unwrap().unwrap();
        assert_eq!(
            tip_before, tip_after,
            "no merge → --fresh leaves the tracking-branch tip unchanged"
        );
    }

    #[test]
    fn fresh_recomputes_deadline_from_total_budget() {
        // (e1): a total_budget schedule gets its deadline recomputed as
        // now + total_budget, and started_at advances.
        let tmp = init_test_repo();
        let root = write_experiment(
            tmp.path(),
            "test",
            "echo 3.14 > value.txt",
            "bash score.sh",
            Duration::from_secs(3600),
            1,
        );

        run_loop(
            "test".to_string(),
            false,
            tmp.path().to_path_buf(),
            false,
            false,
        )
        .unwrap();
        let first = storage::read_state(&root.join("state.json"))
            .unwrap()
            .unwrap();

        run_loop(
            "test".to_string(),
            false,
            tmp.path().to_path_buf(),
            false,
            true,
        )
        .unwrap();
        let fresh = storage::read_state(&root.join("state.json"))
            .unwrap()
            .unwrap();

        assert!(
            fresh.deadline > first.deadline,
            "deadline should be recomputed forward: {:?} -> {:?}",
            first.deadline,
            fresh.deadline
        );
        assert!(
            fresh.started_at > first.started_at,
            "started_at should advance on --fresh"
        );
        assert_eq!(fresh.run_iterations_completed, 1);
        assert_eq!(fresh.iterations_completed, 2, "lifetime keeps climbing");
    }

    #[test]
    fn fresh_errors_on_expired_absolute_deadline() {
        // (e2): an already-past absolute RFC3339 deadline errors clearly under
        // --fresh rather than entering a loop that exits immediately.
        let tmp = init_test_repo();
        let root = write_experiment(
            tmp.path(),
            "test",
            "true",
            "bash score.sh",
            Duration::from_secs(60),
            0,
        );
        let cfg_path = root.join("config.toml");
        let mut cfg: Config = toml::from_str(&fs::read_to_string(&cfg_path).unwrap()).unwrap();
        cfg.schedule = Schedule {
            total_budget: None,
            deadline: Some("2020-01-01T00:00:00Z".to_string()),
        };
        fs::write(&cfg_path, toml::to_string(&cfg).unwrap()).unwrap();

        // First run creates state and exits immediately (no error, no iters).
        run_loop(
            "test".to_string(),
            false,
            tmp.path().to_path_buf(),
            false,
            false,
        )
        .unwrap();
        let recs = storage::read_iterations(&root.join("iterations.jsonl")).unwrap();
        assert!(
            recs.is_empty(),
            "expired deadline should run no iterations, got {recs:?}"
        );

        // --fresh recomputes the same past instant → clear error.
        let err = run_loop(
            "test".to_string(),
            false,
            tmp.path().to_path_buf(),
            false,
            true,
        )
        .unwrap_err();
        assert!(format!("{err}").contains("in the past"), "got: {err}");
    }

    #[test]
    fn fresh_on_in_progress_still_errors() {
        // (f): --fresh on an experiment with an in-progress iteration still
        // refuses and points at `autorize resume`.
        let tmp = init_test_repo();
        let root = write_experiment(
            tmp.path(),
            "test",
            "true",
            "bash score.sh",
            Duration::from_secs(60),
            1,
        );
        let git = Git::new(tmp.path().to_path_buf());
        let sha = git.head_sha().unwrap();
        seed_state(&root, "test", &sha, Some(3), 2);

        let err = run_loop(
            "test".to_string(),
            false,
            tmp.path().to_path_buf(),
            false,
            true,
        )
        .unwrap_err();
        assert!(format!("{err}").contains("resume"), "got: {err}");
    }

    #[test]
    fn fresh_on_never_run_behaves_like_first_run() {
        // (g): --fresh on a never-run experiment is a no-op flag — it behaves
        // exactly like a normal first run (no error).
        let tmp = init_test_repo();
        write_experiment(
            tmp.path(),
            "test",
            "true",
            "bash score.sh",
            Duration::from_millis(50),
            0,
        );
        run_loop(
            "test".to_string(),
            false,
            tmp.path().to_path_buf(),
            false,
            true,
        )
        .unwrap();

        let state_path = tmp.path().join(".autorize").join("test").join("state.json");
        assert!(state_path.exists(), "state.json should exist");
        let git = Git::new(tmp.path().to_path_buf());
        assert!(git.branch_exists("autorize/test").unwrap());
    }

    #[test]
    fn guidance_jsonl_picked_up_by_run_loop() {
        // Acceptance (a)/(b): an entry sitting in guidance.jsonl before the run
        // (as `autorize tell` or a hand-edit would leave it) is read off disk at
        // the top of the iteration and rendered into the prompt the agent saw.
        let tmp = init_test_repo();
        let root = write_experiment(
            tmp.path(),
            "test",
            "true", // noop agent: prompt.md is still written before InvokeAgent
            "bash score.sh",
            Duration::from_secs(60),
            1,
        );
        let entry = crate::storage::GuidanceEntry {
            ts: Utc::now(),
            added_at_iter: Some(0),
            text: "switch to a spigot algorithm".to_string(),
        };
        crate::storage::append_guidance(&root.join("guidance.jsonl"), &entry).unwrap();

        run_loop(
            "test".to_string(),
            false,
            tmp.path().to_path_buf(),
            false,
            false,
        )
        .unwrap();

        let prompt = fs::read_to_string(root.join("iter-0001").join("prompt.md")).unwrap();
        assert!(
            prompt.contains("## Operator guidance"),
            "guidance section missing:\n{prompt}"
        );
        assert!(
            prompt.contains("switch to a spigot algorithm"),
            "guidance text missing:\n{prompt}"
        );
    }
}
