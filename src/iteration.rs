use std::{collections::BTreeMap, fs};

use chrono::Utc;

use crate::{
    agent::{self, AgentSpec},
    config::{Config, Direction},
    error::{Error, Result},
    experiment::ExperimentPaths,
    prompt::{self, BestSnapshot, PromptContext},
    scoring::{self, ScoreDecision},
    storage::{self, CurrentStep, IterationRecord, Outcome, StateSnapshot},
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
    fs::create_dir_all(inputs.paths.root())?;

    checkpoint(state, inputs, CurrentStep::AllocateIter)?;
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
        recent: inputs.recent,
        best: best_snapshot,
        iter: inputs.iter,
        budget: inputs.cfg.iteration.budget,
        direction: inputs.cfg.objective.direction,
    });
    let prompt_path = iter_dir.join("prompt.md");
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
    fs::write(iter_dir.join("agent.stdout"), &agent_out.stdout)?;
    fs::write(iter_dir.join("agent.stderr"), &agent_out.stderr)?;

    checkpoint(state, inputs, CurrentStep::CaptureDiff)?;
    // Stage untracked-and-new files into the index so they show up in
    // `git diff <branch>` (which otherwise ignores untracked content) —
    // needed both for the deny-path scan and the saved changes.diff.
    inputs.git.stage_all_in(&wt)?;
    let diff_text = inputs.git.diff_against(&wt, inputs.branch)?;
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
    let mut final_score: Option<f64> = None;
    let mut new_best_score: Option<f64> = inputs.best.map(|(s, _)| s);
    let mut new_best_iter: Option<u64> = inputs.best.map(|(_, i)| i);

    if changed.is_empty() {
        outcome = Outcome::Noop;
    } else if !denied.is_empty() {
        outcome = Outcome::Denied;
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
                    new_best_score = Some(s);
                    new_best_iter = Some(inputs.iter);
                } else {
                    outcome = Outcome::Discarded;
                }
            }
        }
    }

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
        notes: String::new(),
    };
    storage::append_iteration(&inputs.paths.iterations_log(), &record)?;

    state.iter_in_progress = None;
    state.current_step = CurrentStep::Idle;
    state.iterations_completed += 1;
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

fn checkpoint(
    state: &mut StateSnapshot,
    inputs: &IterationInputs,
    step: CurrentStep,
) -> Result<()> {
    state.iter_in_progress = Some(inputs.iter);
    state.current_step = step;
    storage::write_state(&inputs.paths.state_path(), state)
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
        };
        let mut state = init_state();
        let rec = run_iteration(&inputs, &mut state).unwrap();

        assert_eq!(rec.outcome, Outcome::Merged);
        let s = rec.score.expect("score should be set on Merged");
        assert!(s < 0.01, "score {s} too far from pi");
        assert_eq!(rec.best_so_far, Some(s));

        assert!(paths.state_path().exists());
        assert_eq!(state.current_step, CurrentStep::Idle);
        assert_eq!(state.iterations_completed, 1);
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
        };
        let mut state = init_state();
        let rec = run_iteration(&inputs, &mut state).unwrap();

        assert_eq!(rec.outcome, Outcome::Noop);
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
        };
        let mut state = init_state();
        let rec = run_iteration(&inputs, &mut state).unwrap();

        assert_eq!(rec.outcome, Outcome::Denied);
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
        };
        let mut state = init_state();
        state.best_score = Some(0.001);
        state.best_iter = Some(0);
        let rec = run_iteration(&inputs, &mut state).unwrap();

        assert_eq!(rec.outcome, Outcome::Discarded);
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
        };
        let mut state = init_state();
        let rec = run_iteration(&inputs, &mut state).unwrap();

        assert_eq!(rec.outcome, Outcome::Invalid);
        assert!(rec.score.is_none());
        assert!(rec.best_so_far.is_none());
        let after = git.resolve_ref(&branch).unwrap().unwrap();
        assert_eq!(original, after);
    }
}
