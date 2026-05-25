use std::{env, path::PathBuf};

use crate::{
    error::{Error, Result},
    experiment::ExperimentPaths,
    iteration,
    lock::ExperimentLock,
    storage,
};

#[derive(clap::Args, Debug)]
/// One-shot: generate summaries for any iteration records still missing one,
/// then exit.
///
/// Normally this backfill runs automatically at the top of `autorize run` /
/// `resume`. This hidden, maintenance-only command lets an operator fill
/// missing summaries on a *stopped* experiment without having to start (and
/// then stop) a run. It acquires the experiment lock, so it fails fast rather
/// than racing a live `run` that is appending to `iterations.jsonl`.
pub struct BackfillArgs {
    /// Experiment name (must exist under `.autorize/<name>/`).
    pub name: String,
}

pub fn run(args: BackfillArgs) -> anyhow::Result<()> {
    let project_root = env::current_dir()?;
    run_with_root(args, project_root)?;
    Ok(())
}

fn run_with_root(args: BackfillArgs, project_root: PathBuf) -> Result<()> {
    let paths = ExperimentPaths::new(project_root, args.name.clone());
    if !paths.root().exists() {
        return Err(Error::Config(format!(
            "experiment {:?} not found at {:?}; run `autorize init {}` first",
            args.name,
            paths.root(),
            args.name
        )));
    }

    // Hold the lock for the whole call. `backfill_missing_summaries` does a
    // full atomic rewrite of `iterations.jsonl` and must never race a live
    // `autorize run` that is appending. The non-blocking flock means we fail
    // fast with `Error::Locked` (pid hint) when a run is active — desired.
    let _lock = ExperimentLock::acquire(&paths.lock_path())?;

    let cfg = paths.load_config()?;
    if !cfg.summarize.enabled {
        // Don't error and don't override the flag: just report the no-op.
        println!("summarize is disabled in config; nothing to backfill");
        return Ok(());
    }

    let mut records = storage::read_iterations(&paths.iterations_log())?;
    let changed = iteration::backfill_missing_summaries(&cfg, &paths, &mut records)?;
    if changed {
        println!("backfilled missing summaries");
    } else {
        println!("no missing summaries to backfill");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use chrono::Utc;
    use tempfile::{TempDir, tempdir};

    use super::*;
    use crate::storage::{self, IterationRecord, Outcome};

    fn make_exp(name: &str) -> (TempDir, PathBuf) {
        let tmp = tempdir().unwrap();
        let root = tmp.path().join(".autorize").join(name);
        fs::create_dir_all(&root).unwrap();
        (tmp, root)
    }

    /// Write a minimal valid config.toml, optionally with `[summarize]`
    /// enabled and a deterministic stub summarizer.
    fn write_config(root: &Path, summarize_enabled: bool) {
        let summarize = if summarize_enabled {
            "\n[summarize]\nenabled = true\ncommand = \"echo SUMMARY_MARKER\"\nstdin = \"prompt\"\n"
        } else {
            // Summaries now default to enabled, so the disabled path must be
            // requested explicitly rather than by omitting the section.
            "\n[summarize]\nenabled = false\n"
        };
        let body = format!(
            r#"[experiment]
name = "test"
description = "unit-test experiment"

[objective]
command = "bash score.sh"
direction = "min"
parse = {{ kind = "float" }}
timeout = "30s"
fail_mode = "invalid"

[boundaries]
allow_paths = ["value.txt"]
deny_paths = [".autorize/**"]

[setup]
command = ""

[teardown]
command = ""

[iteration]
budget = "30s"

[schedule]
total_budget = "5m"

[agent]
command = "bash mock-agent.sh {{iter}}"
stdin = "prompt"
{summarize}"#
        );
        fs::write(root.join("config.toml"), body).unwrap();
    }

    fn mk_rec(iter: u64, outcome: Outcome, score: Option<f64>, summary: &str) -> IterationRecord {
        let now = Utc::now();
        IterationRecord {
            iter,
            started_at: now,
            ended_at: now,
            outcome,
            score,
            best_so_far: score,
            agent_exit: Some(0),
            agent_killed_by_budget: false,
            diff_lines: 1,
            notes: String::new(),
            summary: summary.to_string(),
        }
    }

    fn seed_iter_artifacts(paths: &ExperimentPaths, iter: u64) {
        let d = paths.iter_dir(iter);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("changes.diff"), "diff --git a/x b/x\n+change\n").unwrap();
        fs::write(d.join("agent.stdout"), "agent did a thing\n").unwrap();
    }

    fn backfill(name: &str, root: &Path) -> Result<()> {
        run_with_root(
            BackfillArgs {
                name: name.to_string(),
            },
            root.to_path_buf(),
        )
    }

    #[test]
    fn backfill_missing_experiment_errors() {
        let tmp = tempdir().unwrap();
        let err = backfill("nope", tmp.path()).unwrap_err();
        assert!(format!("{err}").contains("not found"), "got: {err}");
    }

    #[test]
    fn backfill_disabled_is_noop() {
        let (tmp, root) = make_exp("test");
        write_config(&root, false);
        // Records with a missing summary exist, but summarize is disabled, so
        // nothing should be written and the call should still succeed.
        let records = vec![mk_rec(1, Outcome::Merged, Some(0.1), "")];
        storage::rewrite_iterations(&root.join("iterations.jsonl"), &records).unwrap();

        backfill("test", tmp.path()).unwrap();

        let on_disk = storage::read_iterations(&root.join("iterations.jsonl")).unwrap();
        assert_eq!(on_disk[0].summary, "", "disabled backfill must not write");
    }

    #[test]
    fn backfill_fills_missing_summary() {
        let (tmp, root) = make_exp("test");
        write_config(&root, true);

        let paths = ExperimentPaths::new(tmp.path().to_path_buf(), "test".to_string());
        seed_iter_artifacts(&paths, 1);
        let records = vec![mk_rec(1, Outcome::Merged, Some(0.1), "")];
        storage::rewrite_iterations(&paths.iterations_log(), &records).unwrap();

        backfill("test", tmp.path()).unwrap();

        let on_disk = storage::read_iterations(&paths.iterations_log()).unwrap();
        assert_eq!(on_disk[0].summary, "SUMMARY_MARKER");
        assert_eq!(
            fs::read_to_string(paths.iter_dir(1).join("summary.md")).unwrap(),
            "SUMMARY_MARKER"
        );
    }
}
