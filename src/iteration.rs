use std::{collections::BTreeMap, fs, path::Path};

use chrono::Utc;

use crate::{
    agent::{self, AgentSpec},
    config::{Config, Direction},
    error::{Error, Result},
    experiment::ExperimentPaths,
    prompt::{self, BestSnapshot, PromptContext, SummaryContext},
    scoring::{self, ScoreDecision},
    storage::{self, CurrentStep, GuidanceEntry, IterationRecord, Outcome, StateSnapshot},
    subproc,
    worktree::{self, Git},
};

pub struct IterationInputs<'a> {
    pub cfg: &'a Config,
    pub paths: &'a ExperimentPaths,
    pub git: &'a Git,
    pub branch: &'a str,
    pub iter: u64,
    pub best: Option<(f64, u64)>,
    pub recent: &'a [IterationRecord],
    pub program_md: &'a str,
    pub best_diff: Option<&'a str>,
    /// Operator guidance loaded from `guidance.jsonl` at the top of this
    /// iteration; injected verbatim into the prompt.
    pub guidance: &'a [GuidanceEntry],
}

/// Run one full iteration end-to-end. Mutates `state` and persists it
/// (atomic write) at each major step transition. Appends the resulting
/// `IterationRecord` to `iterations.jsonl`. Returns the record.
pub fn run_iteration(
    inputs: &IterationInputs,
    state: &mut StateSnapshot,
) -> Result<IterationRecord> {
    let started_at = Utc::now();
    let iter_dir = inputs.paths.iter_dir(inputs.iter);

    // Ensure the experiment root exists so checkpoint writes can land.
    tracing::info!("mkdir -p {}", inputs.paths.root().display());
    fs::create_dir_all(inputs.paths.root())?;

    checkpoint(state, inputs, CurrentStep::AllocateIter)?;
    tracing::info!("mkdir -p {}", iter_dir.display());
    fs::create_dir_all(&iter_dir)?;

    checkpoint(state, inputs, CurrentStep::CreateWorktree)?;
    let wt = iter_dir.join("wt");
    inputs.git.worktree_add(&wt, inputs.branch)?;

    checkpoint(state, inputs, CurrentStep::RunSetup)?;
    if !inputs.cfg.setup.command.trim().is_empty() {
        subproc::run_command_with_budget(
            &inputs.cfg.setup.command,
            &wt,
            inputs.cfg.setup.timeout,
            &BTreeMap::new(),
            None,
        )?;
    }

    checkpoint(state, inputs, CurrentStep::BuildPrompt)?;
    let best_snapshot = match (inputs.best, inputs.best_diff) {
        (Some((score, iter)), Some(diff)) => Some(BestSnapshot { iter, score, diff }),
        _ => None,
    };
    let prompt_text = prompt::build_prompt(&PromptContext {
        program_md: inputs.program_md,
        boundaries: &inputs.cfg.boundaries,
        guidance: inputs.guidance,
        recent: inputs.recent,
        best: best_snapshot,
        iter: inputs.iter,
        budget: inputs.cfg.iteration.budget,
        direction: inputs.cfg.objective.direction,
    });
    let prompt_path = iter_dir.join("prompt.md");
    tracing::info!("write {}", prompt_path.display());
    fs::write(&prompt_path, &prompt_text)?;

    checkpoint(state, inputs, CurrentStep::InvokeAgent)?;
    let agent_out = agent::run_agent(&AgentSpec {
        command_template: &inputs.cfg.agent.command,
        prompt_file: &prompt_path,
        workdir: &wt,
        iter: inputs.iter,
        budget: inputs.cfg.iteration.budget,
        workdir_var: &inputs.cfg.agent.workdir_var,
        env: &inputs.cfg.agent.env,
        stdin: inputs.cfg.agent.stdin,
    })?;
    tracing::info!("write {}", iter_dir.join("agent.stdout").display());
    fs::write(iter_dir.join("agent.stdout"), &agent_out.stdout)?;
    tracing::info!("write {}", iter_dir.join("agent.stderr").display());
    fs::write(iter_dir.join("agent.stderr"), &agent_out.stderr)?;

    checkpoint(state, inputs, CurrentStep::CaptureDiff)?;
    // Stage untracked-and-new files into the index so they show up in
    // `git diff <branch>` (which otherwise ignores untracked content) —
    // needed both for the deny-path scan and the saved changes.diff.
    inputs.git.stage_all_in(&wt)?;
    let diff_text = inputs.git.diff_against(&wt, inputs.branch)?;
    tracing::info!("write {}", iter_dir.join("changes.diff").display());
    fs::write(iter_dir.join("changes.diff"), &diff_text)?;
    let changed = inputs.git.diff_paths_against(&wt, inputs.branch)?;
    // Unwind the capture-time staging so a kept (non-merged) worktree reads as
    // an ordinary unstaged dirty checkout instead of a fully-staged index. The
    // merge path re-stages independently via `commit_all_in`, so this does not
    // affect committed content.
    inputs.git.unstage_all_in(&wt)?;
    let denied = worktree::deny_path_matches(&changed, &inputs.cfg.boundaries.deny_paths)?;
    let diff_lines = diff_text.lines().count() as u64;

    let outcome: Outcome;
    // Harness-derived reason for this outcome, surfaced to the next iteration's
    // prompt and to `autorize status`. Set on every record-producing branch.
    let notes: String;
    let mut final_score: Option<f64> = None;
    let mut new_best_score: Option<f64> = inputs.best.map(|(s, _)| s);
    let mut new_best_iter: Option<u64> = inputs.best.map(|(_, i)| i);

    if changed.is_empty() {
        outcome = Outcome::Noop;
        notes = "no changes produced".to_string();
    } else if !denied.is_empty() {
        outcome = Outcome::Denied;
        notes = format!("denied: touched {}", denied.join(", "));
    } else {
        checkpoint(state, inputs, CurrentStep::RunTeardown)?;
        if !inputs.cfg.teardown.command.trim().is_empty() {
            subproc::run_command_with_budget(
                &inputs.cfg.teardown.command,
                &wt,
                inputs.cfg.teardown.timeout,
                &BTreeMap::new(),
                None,
            )?;
        }

        checkpoint(state, inputs, CurrentStep::Score)?;
        let so = scoring::score(&wt, &inputs.cfg.objective)?;
        let decision = scoring::apply_fail_mode(&so, &inputs.cfg.objective);

        checkpoint(state, inputs, CurrentStep::Decide)?;
        match decision {
            ScoreDecision::Abort(reason) => {
                return Err(Error::Config(format!("scoring abort: {reason}")));
            }
            ScoreDecision::Discard => {
                outcome = Outcome::Invalid;
                notes = format!(
                    "invalid: {}",
                    so.failure
                        .as_ref()
                        .map(scoring::describe_failure)
                        .unwrap_or_else(|| "no score".to_string())
                );
            }
            ScoreDecision::Use(s) => {
                final_score = Some(s);
                let improved = match (inputs.best, inputs.cfg.objective.direction) {
                    (None, _) => true,
                    (Some((b, _)), Direction::Min) => s < b,
                    (Some((b, _)), Direction::Max) => s > b,
                };
                if improved {
                    checkpoint(state, inputs, CurrentStep::Merge)?;
                    // The worktree is on a detached HEAD, so committing only
                    // creates a commit object — we advance the tracking branch
                    // ref to it explicitly so the next iteration starts here.
                    let sha = inputs
                        .git
                        .commit_all_in(&wt, &format!("autorize iter {}: score {s}", inputs.iter))?;
                    inputs.git.update_branch_ref(inputs.branch, &sha)?;
                    outcome = Outcome::Merged;
                    notes = match inputs.best {
                        Some((b, _)) => format!("improved: {} from {}", fmt_score(s), fmt_score(b)),
                        None => format!("first valid score: {}", fmt_score(s)),
                    };
                    new_best_score = Some(s);
                    new_best_iter = Some(inputs.iter);
                } else {
                    outcome = Outcome::Discarded;
                    // `improved` was false, so a prior best exists.
                    let best_b = inputs.best.map(|(b, _)| b).unwrap_or(s);
                    notes = format!(
                        "regressed: {} vs best {} ({})",
                        fmt_score(s),
                        fmt_score(best_b),
                        dir_label(inputs.cfg.objective.direction),
                    );
                }
            }
        }
    }

    // Best-effort post-iteration summary (A2). Runs AFTER the worker exits,
    // bounded by `summarize.timeout` independently of `iteration.budget`. Done
    // while the worktree still exists (it is the summarizer's `{workdir}`) but
    // writing only under `iter_dir/`, never `wt/`, so it can't trip deny
    // enforcement. Skipped for `noop` (no diff to summarize) and when disabled;
    // any failure leaves `summary` empty without affecting the outcome.
    let summary = if inputs.cfg.summarize.enabled && outcome != Outcome::Noop {
        summarize_iteration(
            inputs,
            &iter_dir,
            &wt,
            outcome,
            final_score,
            &diff_text,
            &agent_out.stdout,
            &agent_out.stderr,
        )
    } else {
        String::new()
    };

    checkpoint(state, inputs, CurrentStep::Cleanup)?;
    if !inputs.cfg.iteration.keep_worktrees {
        inputs.git.worktree_remove(&wt)?;
    }

    checkpoint(state, inputs, CurrentStep::Record)?;
    let record = IterationRecord {
        iter: inputs.iter,
        started_at,
        ended_at: Utc::now(),
        outcome,
        score: final_score,
        best_so_far: new_best_score,
        agent_exit: agent_out.exit_code,
        agent_killed_by_budget: agent_out.killed_by_budget,
        diff_lines,
        notes,
        summary,
    };
    storage::append_iteration(&inputs.paths.iterations_log(), &record)?;

    state.iter_in_progress = None;
    state.current_step = CurrentStep::Idle;
    state.iterations_completed += 1;
    // A normally-completed iteration always counts toward the current run's
    // `max_iterations` budget (the outcome here is never `killed`).
    state.run_iterations_completed += 1;
    if outcome == Outcome::Noop {
        state.consecutive_noops += 1;
    } else {
        state.consecutive_noops = 0;
    }
    state.best_score = new_best_score;
    state.best_iter = new_best_iter;
    storage::write_state(&inputs.paths.state_path(), state)?;

    Ok(record)
}

/// Run the optional `[summarize]` step for one iteration. Builds a
/// self-contained summary prompt from the iteration's own artifacts (the diff,
/// stdio tails, outcome/score/best), writes it to `iter_dir/summary-prompt.md`,
/// runs `summarize.command` through the same subprocess machinery as the worker
/// (bounded by `summarize.timeout`), and returns the trimmed stdout — also
/// persisted to `iter_dir/summary.md`.
///
/// Best-effort by contract: this never returns an `Err` and never mutates the
/// iteration outcome. Any failure (spawn error, timeout, nonzero exit, empty
/// output, artifact write error) logs a warning and yields an empty summary.
#[allow(clippy::too_many_arguments)]
fn summarize_iteration(
    inputs: &IterationInputs,
    iter_dir: &Path,
    workdir: &Path,
    outcome: Outcome,
    score: Option<f64>,
    diff: &str,
    stdout: &str,
    stderr: &str,
) -> String {
    let prompt_text = prompt::build_summary_prompt(&SummaryContext {
        iter: inputs.iter,
        outcome,
        score,
        best: inputs.best,
        direction: inputs.cfg.objective.direction,
        diff,
        stdout_tail: stdout,
        stderr_tail: stderr,
    });
    let prompt_path = iter_dir.join("summary-prompt.md");
    tracing::info!("write {}", prompt_path.display());
    if let Err(e) = fs::write(&prompt_path, &prompt_text) {
        tracing::warn!("summarize: failed to write {}: {e}", prompt_path.display());
        return String::new();
    }

    // Reuse the worker's subprocess machinery (env, workdir_var, signal-safe
    // budget kill) but with the summarize command/timeout/stdin and its own
    // prompt file. The summarizer inherits `[agent.env]` so it sees the same
    // credentials (e.g. ANTHROPIC_API_KEY) as the worker.
    let out = match agent::run_agent(&AgentSpec {
        command_template: &inputs.cfg.summarize.command,
        prompt_file: &prompt_path,
        workdir,
        iter: inputs.iter,
        budget: inputs.cfg.summarize.timeout,
        workdir_var: &inputs.cfg.agent.workdir_var,
        env: &inputs.cfg.agent.env,
        stdin: inputs.cfg.summarize.stdin,
    }) {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!("summarize: command failed to run: {e}; leaving summary empty");
            return String::new();
        }
    };

    if out.killed_by_budget {
        tracing::warn!(
            "summarize: killed by summarize.timeout ({}s); leaving summary empty",
            inputs.cfg.summarize.timeout.as_secs()
        );
        return String::new();
    }
    if out.exit_code != Some(0) {
        tracing::warn!(
            "summarize: nonzero exit {:?}; leaving summary empty",
            out.exit_code
        );
        return String::new();
    }

    let summary = out.stdout.trim().to_string();
    if summary.is_empty() {
        tracing::warn!("summarize: empty output; leaving summary empty");
        return String::new();
    }

    let summary_path = iter_dir.join("summary.md");
    tracing::info!("write {}", summary_path.display());
    if let Err(e) = fs::write(&summary_path, &summary) {
        // The artifact write failed but we still have the text in-memory; keep
        // it in the record rather than discarding a successful summarization.
        tracing::warn!("summarize: failed to write {}: {e}", summary_path.display());
    }
    summary
}

fn checkpoint(
    state: &mut StateSnapshot,
    inputs: &IterationInputs,
    step: CurrentStep,
) -> Result<()> {
    state.iter_in_progress = Some(inputs.iter);
    state.current_step = step;
    storage::write_state(&inputs.paths.state_path(), state)
}

/// Compact score rendering for the human-readable `notes` reason.
fn fmt_score(v: f64) -> String {
    format!("{v:.6}")
}

fn dir_label(d: Direction) -> &'static str {
    match d {
        Direction::Min => "min",
        Direction::Max => "max",
    }
}

#[cfg(test)]
mod tests {
    use std::{os::unix::fs::PermissionsExt, path::Path, process::Command, time::Duration};

    use chrono::Utc;
    use tempfile::{TempDir, tempdir};

    use super::*;
    use crate::config::{
        Agent,
        AgentStdin,
        Boundaries,
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
    };

    fn run_cmd(args: &[&str], cwd: &Path) {
        let st = Command::new(args[0])
            .args(&args[1..])
            .current_dir(cwd)
            .status()
            .unwrap_or_else(|e| panic!("spawning {args:?} failed: {e}"));
        assert!(st.success(), "command {args:?} failed: {st:?}");
    }

    /// Run a command and return its trimmed stdout, asserting success.
    fn git_out(args: &[&str], cwd: &Path) -> String {
        let out = Command::new(args[0])
            .args(&args[1..])
            .current_dir(cwd)
            .output()
            .unwrap_or_else(|e| panic!("spawning {args:?} failed: {e}"));
        assert!(out.status.success(), "command {args:?} failed: {out:?}");
        String::from_utf8(out.stdout).unwrap()
    }

    /// score.sh: prints |pi - value| using awk so we don't depend on python.
    const SCORE_SH: &str = r#"#!/bin/sh
v=$(cat value.txt)
awk -v x="$v" 'BEGIN { pi=3.141592653589793; d=x-pi; if (d<0) d=-d; printf "%f\n", d }'
"#;

    fn init_test_env() -> (TempDir, Git, ExperimentPaths, String) {
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

        let git = Git::new(p.to_path_buf());
        let sha = git.head_sha().unwrap();
        git.create_branch_at("autorize/test", &sha).unwrap();

        let paths = ExperimentPaths::new(p.to_path_buf(), "test".to_string());
        (tmp, git, paths, "autorize/test".to_string())
    }

    fn make_config(
        agent_cmd: &str,
        objective_cmd: &str,
        fail_mode: FailMode,
        deny_paths: Vec<String>,
        keep_worktrees: bool,
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
                fail_mode,
            },
            boundaries: Boundaries {
                allow_paths: vec![],
                deny_paths,
            },
            setup: Setup::default(),
            teardown: Teardown::default(),
            iteration: Iteration {
                budget: Duration::from_secs(30),
                max_iterations: 0,
                keep_worktrees,
                max_consecutive_noops: 5,
            },
            schedule: Schedule {
                total_budget: Some(Duration::from_secs(3600)),
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

    fn init_state() -> StateSnapshot {
        let now = Utc::now();
        StateSnapshot {
            experiment: "test".to_string(),
            branch: "autorize/test".to_string(),
            base_commit: String::new(),
            iter_in_progress: None,
            current_step: CurrentStep::Idle,
            best_score: None,
            best_iter: None,
            started_at: now,
            deadline: now + chrono::Duration::hours(1),
            iterations_completed: 0,
            run_iterations_completed: 0,
            consecutive_noops: 0,
        }
    }

    #[test]
    fn runs_merged_outcome_on_improvement() {
        let (_tmp, git, paths, branch) = init_test_env();
        let original = git.resolve_ref(&branch).unwrap().unwrap();

        let cfg = make_config(
            "echo 3.14 > value.txt",
            "bash score.sh",
            FailMode::Invalid,
            vec![],
            true, // keep_worktrees so we can introspect if needed
        );
        let inputs = IterationInputs {
            cfg: &cfg,
            paths: &paths,
            git: &git,
            branch: &branch,
            iter: 1,
            best: None,
            recent: &[],
            program_md: "",
            best_diff: None,
            guidance: &[],
        };
        let mut state = init_state();
        let rec = run_iteration(&inputs, &mut state).unwrap();

        assert_eq!(rec.outcome, Outcome::Merged);
        let s = rec.score.expect("score should be set on Merged");
        assert!(s < 0.01, "score {s} too far from pi");
        assert_eq!(rec.best_so_far, Some(s));
        // best was None, so this is the first valid score.
        assert!(
            rec.notes.starts_with("first valid score"),
            "notes: {:?}",
            rec.notes
        );

        assert!(paths.state_path().exists());
        assert_eq!(state.current_step, CurrentStep::Idle);
        assert_eq!(state.iterations_completed, 1);
        assert_eq!(state.run_iterations_completed, 1);
        assert_eq!(state.best_iter, Some(1));
        assert_eq!(state.best_score, Some(s));

        let recs = storage::read_iterations(&paths.iterations_log()).unwrap();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].iter, rec.iter);
        assert_eq!(recs[0].outcome, rec.outcome);

        let after = git.resolve_ref(&branch).unwrap().unwrap();
        assert_ne!(original, after, "merge should advance tracking branch");
    }

    #[test]
    fn runs_noop_outcome_when_agent_makes_no_changes() {
        let (_tmp, git, paths, branch) = init_test_env();
        let original = git.resolve_ref(&branch).unwrap().unwrap();

        let cfg = make_config("true", "bash score.sh", FailMode::Invalid, vec![], false);
        let inputs = IterationInputs {
            cfg: &cfg,
            paths: &paths,
            git: &git,
            branch: &branch,
            iter: 1,
            best: None,
            recent: &[],
            program_md: "",
            best_diff: None,
            guidance: &[],
        };
        let mut state = init_state();
        let rec = run_iteration(&inputs, &mut state).unwrap();

        assert_eq!(rec.outcome, Outcome::Noop);
        assert_eq!(rec.notes, "no changes produced");
        assert!(rec.score.is_none());
        assert!(rec.best_so_far.is_none());
        assert_eq!(state.consecutive_noops, 1);
        assert!(state.best_score.is_none());
        assert!(state.best_iter.is_none());

        let after = git.resolve_ref(&branch).unwrap().unwrap();
        assert_eq!(original, after);
    }

    #[test]
    fn runs_denied_outcome_when_diff_touches_deny_path() {
        let (_tmp, git, paths, branch) = init_test_env();
        let original = git.resolve_ref(&branch).unwrap().unwrap();

        let cfg = make_config(
            "mkdir -p forbidden && echo bad > forbidden/x.txt",
            "bash score.sh",
            FailMode::Invalid,
            vec!["forbidden/**".to_string()],
            false,
        );
        let inputs = IterationInputs {
            cfg: &cfg,
            paths: &paths,
            git: &git,
            branch: &branch,
            iter: 1,
            best: None,
            recent: &[],
            program_md: "",
            best_diff: None,
            guidance: &[],
        };
        let mut state = init_state();
        let rec = run_iteration(&inputs, &mut state).unwrap();

        assert_eq!(rec.outcome, Outcome::Denied);
        assert!(
            rec.notes.starts_with("denied: touched") && rec.notes.contains("forbidden"),
            "notes: {:?}",
            rec.notes
        );
        assert!(rec.score.is_none());
        let after = git.resolve_ref(&branch).unwrap().unwrap();
        assert_eq!(original, after, "denied iteration must not advance branch");
    }

    #[test]
    fn runs_discarded_outcome_when_score_worse() {
        let (_tmp, git, paths, branch) = init_test_env();
        let original = git.resolve_ref(&branch).unwrap().unwrap();

        let cfg = make_config(
            "echo 2.0 > value.txt",
            "bash score.sh",
            FailMode::Invalid,
            vec![],
            false,
        );
        let inputs = IterationInputs {
            cfg: &cfg,
            paths: &paths,
            git: &git,
            branch: &branch,
            iter: 1,
            // Pretend a prior iter already got very close to pi.
            best: Some((0.001, 0)),
            recent: &[],
            program_md: "",
            best_diff: None,
            guidance: &[],
        };
        let mut state = init_state();
        state.best_score = Some(0.001);
        state.best_iter = Some(0);
        let rec = run_iteration(&inputs, &mut state).unwrap();

        assert_eq!(rec.outcome, Outcome::Discarded);
        assert!(
            rec.notes.starts_with("regressed:") && rec.notes.contains("(min)"),
            "notes: {:?}",
            rec.notes
        );
        assert!(rec.score.is_some());
        // Score for value=2.0 is ~1.1416, worse than the pre-seeded 0.001.
        let s = rec.score.unwrap();
        assert!(s > 1.0, "score {s} should be far from pi");
        // Branch unchanged; best unchanged.
        let after = git.resolve_ref(&branch).unwrap().unwrap();
        assert_eq!(original, after);
        assert_eq!(state.best_iter, Some(0));
        assert_eq!(state.best_score, Some(0.001));
    }

    #[test]
    fn discarded_kept_worktree_has_clean_index() {
        // Part A: a non-merged but kept worktree must read as an ordinary
        // unstaged dirty checkout — no stray `git add -A` index left behind.
        let (_tmp, git, paths, branch) = init_test_env();

        let cfg = make_config(
            "echo 2.0 > value.txt",
            "bash score.sh",
            FailMode::Invalid,
            vec![],
            true, // keep_worktrees
        );
        let inputs = IterationInputs {
            cfg: &cfg,
            paths: &paths,
            git: &git,
            branch: &branch,
            iter: 1,
            // Pre-seed a much better best so value=2.0 (~1.14) is discarded.
            best: Some((0.001, 0)),
            recent: &[],
            program_md: "",
            best_diff: None,
            guidance: &[],
        };
        let mut state = init_state();
        state.best_score = Some(0.001);
        state.best_iter = Some(0);
        let rec = run_iteration(&inputs, &mut state).unwrap();
        assert_eq!(rec.outcome, Outcome::Discarded);

        let wt = paths.iter_dir(1).join("wt");
        assert!(wt.is_dir(), "kept worktree should exist");
        // Index matches HEAD: nothing staged.
        let cached = git_out(&["git", "diff", "--cached"], &wt);
        assert!(cached.trim().is_empty(), "stray staged index: {cached:?}");
        // The agent's change is still present as an unstaged modification.
        let porcelain = git_out(&["git", "status", "--porcelain"], &wt);
        assert!(
            porcelain.contains("value.txt"),
            "expected unstaged change to value.txt, got: {porcelain:?}"
        );
    }

    #[test]
    fn merged_commit_contains_full_agent_diff() {
        // Part A: unstaging at capture time must not regress merge content —
        // the merged commit's tree must still carry the agent's change.
        let (_tmp, git, paths, branch) = init_test_env();

        let cfg = make_config(
            "echo 3.14 > value.txt",
            "bash score.sh",
            FailMode::Invalid,
            vec![],
            false,
        );
        let inputs = IterationInputs {
            cfg: &cfg,
            paths: &paths,
            git: &git,
            branch: &branch,
            iter: 1,
            best: None,
            recent: &[],
            program_md: "",
            best_diff: None,
            guidance: &[],
        };
        let mut state = init_state();
        let rec = run_iteration(&inputs, &mut state).unwrap();
        assert_eq!(rec.outcome, Outcome::Merged);

        // The committed tree on the tracking branch carries the agent's edit.
        let committed = git_out(
            &["git", "show", &format!("{branch}:value.txt")],
            paths.project_root(),
        );
        assert_eq!(
            committed.trim(),
            "3.14",
            "merged commit missing agent change"
        );
    }

    #[test]
    fn runs_invalid_outcome_when_scoring_fails() {
        let (_tmp, git, paths, branch) = init_test_env();
        let original = git.resolve_ref(&branch).unwrap().unwrap();

        let cfg = make_config(
            "echo 3.14 > value.txt",
            "exit 1",
            FailMode::Invalid,
            vec![],
            false,
        );
        let inputs = IterationInputs {
            cfg: &cfg,
            paths: &paths,
            git: &git,
            branch: &branch,
            iter: 1,
            best: None,
            recent: &[],
            program_md: "",
            best_diff: None,
            guidance: &[],
        };
        let mut state = init_state();
        let rec = run_iteration(&inputs, &mut state).unwrap();

        assert_eq!(rec.outcome, Outcome::Invalid);
        assert!(
            rec.notes.starts_with("invalid:") && rec.notes.contains("exit code 1"),
            "notes: {:?}",
            rec.notes
        );
        assert!(rec.score.is_none());
        assert!(rec.best_so_far.is_none());
        let after = git.resolve_ref(&branch).unwrap().unwrap();
        assert_eq!(original, after);
    }

    /// Build inputs and run one iteration with summarization configured via
    /// the given mutator on `cfg.summarize`.
    fn run_with_summarize(
        agent_cmd: &str,
        mutate: impl FnOnce(&mut crate::config::Summarize),
    ) -> (TempDir, ExperimentPaths, IterationRecord) {
        let (tmp, git, paths, branch) = init_test_env();
        let mut cfg = make_config(
            agent_cmd,
            "bash score.sh",
            FailMode::Invalid,
            vec![],
            true, // keep_worktrees so summary artifacts are easy to introspect
        );
        mutate(&mut cfg.summarize);
        let inputs = IterationInputs {
            cfg: &cfg,
            paths: &paths,
            git: &git,
            branch: &branch,
            iter: 1,
            best: None,
            recent: &[],
            program_md: "",
            best_diff: None,
            guidance: &[],
        };
        let mut state = init_state();
        let rec = run_iteration(&inputs, &mut state).unwrap();
        (tmp, paths, rec)
    }

    #[test]
    fn summary_generated_for_non_noop_iteration() {
        // (a): an enabled, reachable summarize command yields a non-empty
        // summary on a merged iteration, captured into the record and to
        // iter-NNNN/summary.md.
        let (_tmp, paths, rec) = run_with_summarize("echo 3.14 > value.txt", |s| {
            s.enabled = true;
            // stdin = "prompt" so we don't need {prompt_file} in the command;
            // the summarizer just emits a fixed line on stdout.
            s.stdin = AgentStdin::Prompt;
            s.command = "echo SUMMARY_MARKER".to_string();
        });
        assert_eq!(rec.outcome, Outcome::Merged);
        assert_eq!(rec.summary, "SUMMARY_MARKER");
        let summary_md = fs::read_to_string(paths.iter_dir(1).join("summary.md")).unwrap();
        assert_eq!(summary_md, "SUMMARY_MARKER");
        // The summary prompt was written outside the worktree.
        assert!(paths.iter_dir(1).join("summary-prompt.md").exists());
    }

    #[test]
    fn summary_skipped_for_noop_iteration() {
        // (a)/scope: a noop produces no diff, so summarization is skipped even
        // when enabled.
        let (_tmp, paths, rec) = run_with_summarize("true", |s| {
            s.enabled = true;
            s.stdin = AgentStdin::Prompt;
            s.command = "echo SHOULD_NOT_RUN".to_string();
        });
        assert_eq!(rec.outcome, Outcome::Noop);
        assert_eq!(rec.summary, "");
        assert!(!paths.iter_dir(1).join("summary.md").exists());
        assert!(!paths.iter_dir(1).join("summary-prompt.md").exists());
    }

    #[test]
    fn summary_failure_is_best_effort() {
        // (c): a failing summarize command leaves the outcome unchanged and the
        // summary empty.
        let (_tmp, paths, rec) = run_with_summarize("echo 3.14 > value.txt", |s| {
            s.enabled = true;
            s.stdin = AgentStdin::Prompt;
            s.command = "false".to_string();
        });
        assert_eq!(rec.outcome, Outcome::Merged, "outcome must be unaffected");
        assert!(rec.score.is_some());
        assert_eq!(rec.summary, "");
        assert!(!paths.iter_dir(1).join("summary.md").exists());
    }

    #[test]
    fn summary_timeout_is_best_effort() {
        // (c): a summarize command that overruns summarize.timeout is killed and
        // leaves the summary empty without affecting the iteration.
        let (_tmp, _paths, rec) = run_with_summarize("echo 3.14 > value.txt", |s| {
            s.enabled = true;
            s.stdin = AgentStdin::Prompt;
            s.command = "sleep 30".to_string();
            s.timeout = Duration::from_secs(1);
        });
        assert_eq!(rec.outcome, Outcome::Merged);
        assert_eq!(rec.summary, "");
    }

    #[test]
    fn summary_disabled_leaves_empty() {
        // (d): with summarize disabled the field is empty and no artifacts
        // are written.
        let (_tmp, paths, rec) = run_with_summarize("echo 3.14 > value.txt", |s| {
            s.enabled = false;
        });
        assert_eq!(rec.outcome, Outcome::Merged);
        assert_eq!(rec.summary, "");
        assert!(!paths.iter_dir(1).join("summary.md").exists());
        assert!(!paths.iter_dir(1).join("summary-prompt.md").exists());
    }

    #[test]
    fn operator_guidance_appears_in_prompt() {
        // Guidance handed to the iteration is rendered into the prompt the
        // agent sees (iter-NNNN/prompt.md), under `## Operator guidance`.
        let (_tmp, git, paths, branch) = init_test_env();
        let cfg = make_config("true", "bash score.sh", FailMode::Invalid, vec![], true);
        let guidance = vec![GuidanceEntry {
            ts: Utc::now(),
            added_at_iter: Some(2),
            text: "explore a spigot algorithm".to_string(),
        }];
        let inputs = IterationInputs {
            cfg: &cfg,
            paths: &paths,
            git: &git,
            branch: &branch,
            iter: 1,
            best: None,
            recent: &[],
            program_md: "",
            best_diff: None,
            guidance: &guidance,
        };
        let mut state = init_state();
        run_iteration(&inputs, &mut state).unwrap();

        let prompt = fs::read_to_string(paths.iter_dir(1).join("prompt.md")).unwrap();
        assert!(
            prompt.contains("## Operator guidance"),
            "guidance section missing:\n{prompt}"
        );
        assert!(
            prompt.contains("- (since iter 2) explore a spigot algorithm"),
            "guidance text missing:\n{prompt}"
        );
    }
}
