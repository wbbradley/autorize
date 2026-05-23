use std::env;

#[derive(clap::Args, Debug)]
/// Resume a previously-started experiment. If a mid-iteration crash left
/// `state.json` pointing at an in-progress iteration, that iteration is
/// recorded as outcome `"killed"`, its worktree is cleaned up, and the
/// run continues from the next iter number.
pub struct ResumeArgs {
    /// Experiment name.
    pub name: String,
    /// Proceed even if the working tree has uncommitted changes
    /// (excluding `.autorize/`).
    #[arg(long)]
    pub allow_dirty: bool,
}

pub fn run(args: ResumeArgs) -> anyhow::Result<()> {
    let project_root = env::current_dir()?;
    super::run::run_loop(args.name, args.allow_dirty, project_root, true)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        fs,
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
        process::Command,
        time::Duration,
    };

    use chrono::Utc;
    use tempfile::{TempDir, tempdir};

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
        storage::{self, CurrentStep, IterationRecord, Outcome, StateSnapshot},
        worktree::Git,
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

    #[test]
    fn resume_records_killed_for_in_progress() {
        let tmp = init_test_repo();
        let root = write_experiment(
            tmp.path(),
            "test",
            "true",
            "bash score.sh",
            Duration::from_secs(60),
            4,
        );
        let git = Git::new(tmp.path().to_path_buf());
        let sha = git.head_sha().unwrap();
        git.create_branch_at("autorize/test", &sha).unwrap();

        let now = Utc::now();
        let initial_completed = 2u64;
        let state = StateSnapshot {
            experiment: "test".to_string(),
            branch: "autorize/test".to_string(),
            base_commit: sha,
            iter_in_progress: Some(3),
            current_step: CurrentStep::InvokeAgent,
            best_score: None,
            best_iter: None,
            started_at: now,
            deadline: now + chrono::Duration::seconds(3600),
            iterations_completed: initial_completed,
            consecutive_noops: 0,
        };
        storage::write_state(&root.join("state.json"), &state).unwrap();

        crate::cli::run::run_loop("test".to_string(), false, tmp.path().to_path_buf(), true)
            .unwrap_or_else(|e| panic!("resume failed: {e}"));

        let recs = storage::read_iterations(&root.join("iterations.jsonl")).unwrap();
        let killed = recs
            .iter()
            .find(|r| r.iter == 3)
            .expect("kill record missing");
        assert_eq!(killed.outcome, Outcome::Killed);

        let final_state = storage::read_state(&root.join("state.json"))
            .unwrap()
            .expect("state.json should still exist");
        assert!(final_state.iter_in_progress.is_none());
        assert!(
            final_state.iterations_completed > initial_completed,
            "iterations_completed should have advanced; got {}",
            final_state.iterations_completed
        );
    }

    #[test]
    fn resume_reconciles_merged_from_branch_tip() {
        let tmp = init_test_repo();
        let root = write_experiment(
            tmp.path(),
            "test",
            "true",
            "bash score.sh",
            Duration::from_secs(60),
            // max_iterations = 3 so the loop exits immediately after
            // reconciliation lands iter 3 (no extra iters).
            3,
        );
        let git = Git::new(tmp.path().to_path_buf());
        let sha = git.head_sha().unwrap();
        git.create_branch_at("autorize/test", &sha).unwrap();

        // Pre-create a real merge commit on the tracking branch with the
        // production-format subject for iter 3.
        let wt_parent = tempdir().unwrap();
        let wt = wt_parent.path().join("wt");
        git.worktree_add(&wt, "autorize/test").unwrap();
        fs::write(wt.join("value.txt"), "3.1\n").unwrap();
        let score = 0.123456_f64;
        // Detached worktree: commit, then advance the branch tip explicitly so
        // reconciliation can read the production-format subject off it.
        let sha = git
            .commit_all_in(&wt, &format!("autorize iter 3: score {score}"))
            .unwrap();
        git.update_branch_ref("autorize/test", &sha).unwrap();
        git.worktree_remove(&wt).unwrap();

        let now = Utc::now();
        let initial_completed = 2u64;
        let state = StateSnapshot {
            experiment: "test".to_string(),
            branch: "autorize/test".to_string(),
            base_commit: sha,
            iter_in_progress: Some(3),
            current_step: CurrentStep::Merge,
            best_score: None,
            best_iter: None,
            started_at: now,
            deadline: now + chrono::Duration::seconds(3600),
            iterations_completed: initial_completed,
            consecutive_noops: 0,
        };
        storage::write_state(&root.join("state.json"), &state).unwrap();

        crate::cli::run::run_loop("test".to_string(), false, tmp.path().to_path_buf(), true)
            .unwrap_or_else(|e| panic!("resume failed: {e}"));

        let recs = storage::read_iterations(&root.join("iterations.jsonl")).unwrap();
        let for_3: Vec<&IterationRecord> = recs.iter().filter(|r| r.iter == 3).collect();
        assert_eq!(
            for_3.len(),
            1,
            "expected exactly one record for iter 3, got {for_3:?}"
        );
        let rec = for_3[0];
        assert_eq!(rec.outcome, Outcome::Merged);
        assert_eq!(rec.score, Some(score));
        assert_eq!(rec.best_so_far, Some(score));
        assert!(!rec.notes.is_empty(), "notes should explain reconciliation");

        let final_state = storage::read_state(&root.join("state.json"))
            .unwrap()
            .expect("state.json should still exist");
        assert!(final_state.iter_in_progress.is_none());
        assert_eq!(final_state.best_score, Some(score));
        assert_eq!(final_state.best_iter, Some(3));
        assert!(
            final_state.iterations_completed > initial_completed,
            "iterations_completed should advance; got {}",
            final_state.iterations_completed
        );
        assert_eq!(final_state.consecutive_noops, 0);
    }

    #[test]
    fn resume_skips_duplicate_record() {
        let tmp = init_test_repo();
        let root = write_experiment(
            tmp.path(),
            "test",
            "true",
            "bash score.sh",
            Duration::from_secs(60),
            // max_iterations = 5 so reconciliation alone reaches the cap.
            5,
        );
        let git = Git::new(tmp.path().to_path_buf());
        let sha = git.head_sha().unwrap();
        git.create_branch_at("autorize/test", &sha).unwrap();

        // Pre-write a Merged record for iter 5.
        let now = Utc::now();
        let existing = IterationRecord {
            iter: 5,
            started_at: now,
            ended_at: now,
            outcome: Outcome::Merged,
            score: Some(0.5),
            best_so_far: Some(0.5),
            agent_exit: Some(0),
            agent_killed_by_budget: false,
            diff_lines: 1,
            notes: String::new(),
        };
        storage::append_iteration(&root.join("iterations.jsonl"), &existing).unwrap();

        let state = StateSnapshot {
            experiment: "test".to_string(),
            branch: "autorize/test".to_string(),
            base_commit: sha,
            iter_in_progress: Some(5),
            current_step: CurrentStep::Record,
            best_score: None,
            best_iter: None,
            started_at: now,
            deadline: now + chrono::Duration::seconds(3600),
            iterations_completed: 4,
            consecutive_noops: 0,
        };
        storage::write_state(&root.join("state.json"), &state).unwrap();

        crate::cli::run::run_loop("test".to_string(), false, tmp.path().to_path_buf(), true)
            .unwrap_or_else(|e| panic!("resume failed: {e}"));

        let recs = storage::read_iterations(&root.join("iterations.jsonl")).unwrap();
        let for_5: Vec<&IterationRecord> = recs.iter().filter(|r| r.iter == 5).collect();
        assert_eq!(
            for_5.len(),
            1,
            "expected exactly one record for iter 5, got {for_5:?}"
        );
        assert_eq!(for_5[0].outcome, Outcome::Merged);
        assert_eq!(for_5[0].score, Some(0.5));

        let final_state = storage::read_state(&root.join("state.json"))
            .unwrap()
            .expect("state.json should still exist");
        assert!(final_state.iter_in_progress.is_none());
        assert_eq!(final_state.iterations_completed, 5);
        assert_eq!(final_state.best_score, Some(0.5));
        assert_eq!(final_state.best_iter, Some(5));
    }

    #[test]
    fn resume_with_no_state_errors() {
        let tmp = init_test_repo();
        write_experiment(
            tmp.path(),
            "test",
            "true",
            "bash score.sh",
            Duration::from_secs(60),
            1,
        );

        // Resume requires state.json to exist.
        let err =
            crate::cli::run::run_loop("test".to_string(), false, tmp.path().to_path_buf(), true)
                .unwrap_err();
        assert!(format!("{err}").contains("state.json"), "got: {err}");
    }
}
