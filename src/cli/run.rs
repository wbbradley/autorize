use std::{env, fs, path::PathBuf};

use chrono::{Local, Utc};
use regex::Regex;

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
}

pub fn run(args: RunArgs) -> anyhow::Result<()> {
    let project_root = env::current_dir()?;
    run_loop(args.name, args.allow_dirty, project_root, false)?;
    Ok(())
}

/// Shared body used by both `autorize run` and `autorize resume`. When
/// `recover_iter` is true, an in-progress iteration found in state.json
/// is recorded as `killed` and the loop continues; otherwise the loop
/// refuses and points the user at `autorize resume`.
pub(crate) fn run_loop(
    name: String,
    allow_dirty: bool,
    project_root: PathBuf,
    recover_iter: bool,
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
    if !allow_dirty && !git.is_clean_excluding(&[".autorize/"])? {
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
                consecutive_noops: 0,
            };
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
            s
        }
    };

    let deadline = Deadline(state.deadline);

    let mut records = storage::read_iterations(&paths.iterations_log())?;

    loop {
        let now = Utc::now();
        if deadline.is_expired(now) {
            println!("deadline reached at {now}; stopping.");
            break;
        }
        if cfg.iteration.max_iterations > 0
            && state.iterations_completed >= cfg.iteration.max_iterations
        {
            println!(
                "reached max_iterations={}; stopping.",
                cfg.iteration.max_iterations
            );
            break;
        }
        if state.consecutive_noops >= cfg.iteration.max_consecutive_noops {
            println!(
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
        let best_diff_text = best.and_then(|(_, i)| load_best_diff(&paths, i));

        let inputs = IterationInputs {
            cfg: &cfg,
            paths: &paths,
            git: &git,
            branch: &branch,
            iter: next_iter,
            best,
            recent: &recent,
            program_md: &program_md,
            best_diff: best_diff_text.as_deref(),
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
        println!(
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

fn load_best_diff(paths: &ExperimentPaths, iter: u64) -> Option<String> {
    let p = paths.iter_dir(iter).join("changes.diff");
    fs::read_to_string(p).ok()
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
    };
    storage::append_iteration(&paths.iterations_log(), &rec)?;

    state.iter_in_progress = None;
    state.current_step = CurrentStep::Idle;
    state.iterations_completed += 1;
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
    };
    storage::append_iteration(&paths.iterations_log(), &rec)?;

    state.iter_in_progress = None;
    state.current_step = CurrentStep::Idle;
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
    println!("---");
    println!("experiment   {}", state.experiment);
    println!("iterations   {}", state.iterations_completed);
    match (state.best_iter, state.best_score) {
        (Some(i), Some(s)) => println!("best         iter {i}, score {s:.6}"),
        _ => println!("best         (none)"),
    }
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

        let err = run_loop("test".to_string(), false, tmp.path().to_path_buf(), false).unwrap_err();
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
        run_loop("test".to_string(), true, tmp.path().to_path_buf(), false).unwrap();
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
        run_loop("test".to_string(), false, tmp.path().to_path_buf(), false).unwrap();
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

        let err = run_loop("test".to_string(), false, tmp.path().to_path_buf(), false).unwrap_err();
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

        let err = run_loop("test".to_string(), false, tmp.path().to_path_buf(), false).unwrap_err();
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
        run_loop("test".to_string(), false, tmp.path().to_path_buf(), false).unwrap();

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

        let err = run_loop("test".to_string(), false, tmp.path().to_path_buf(), false).unwrap_err();
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
        run_loop("test".to_string(), false, tmp.path().to_path_buf(), false).unwrap();
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
        run_loop("test".to_string(), false, tmp.path().to_path_buf(), false).unwrap();
        let log = tmp
            .path()
            .join(".autorize")
            .join("test")
            .join("iterations.jsonl");
        let recs = storage::read_iterations(&log).unwrap();
        assert_eq!(recs.len(), 2, "expected exactly 2 records, got {recs:?}");
    }
}
