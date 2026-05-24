use std::{env, path::PathBuf};

use chrono::Utc;

use crate::{
    error::{Error, Result},
    experiment::ExperimentPaths,
    storage::{self, GuidanceEntry},
};

#[derive(clap::Args, Debug)]
/// Append a line of operator guidance to an experiment's `guidance.jsonl`.
///
/// `autorize tell` is the steering channel: a separate process appends a
/// message that the running `autorize run` loop re-reads at the top of every
/// iteration and injects into the agent's prompt under `## Operator guidance`
/// (framed as authoritative direction). Guidance persists and is shown every
/// subsequent iteration. `guidance.jsonl` is also safe to hand-edit.
pub struct TellArgs {
    /// Experiment name (must exist under `.autorize/<name>/`).
    pub name: String,
    /// The guidance message. Unquoted trailing words are joined with spaces,
    /// so both `autorize tell pi "do X"` and `autorize tell pi do X` work.
    #[arg(required = true, num_args = 1.., trailing_var_arg = true)]
    pub message: Vec<String>,
}

pub fn run(args: TellArgs) -> anyhow::Result<()> {
    let project_root = env::current_dir()?;
    run_with_root(args, project_root)?;
    Ok(())
}

fn run_with_root(args: TellArgs, project_root: PathBuf) -> Result<()> {
    let paths = ExperimentPaths::new(project_root, args.name.clone());
    if !paths.root().exists() {
        return Err(Error::Config(format!(
            "experiment {:?} not found at {:?}; run `autorize init {}` first",
            args.name,
            paths.root(),
            args.name
        )));
    }

    let text = args.message.join(" ").trim().to_string();
    if text.is_empty() {
        return Err(Error::Config("guidance message is empty".to_string()));
    }

    // Best-effort stamp of where the run is: the in-flight iter if one is
    // running, else the per-run completed count. `null` when never run.
    let added_at_iter = storage::read_state(&paths.state_path())
        .ok()
        .flatten()
        .map(|s| s.iter_in_progress.unwrap_or(s.run_iterations_completed));

    let entry = GuidanceEntry {
        ts: Utc::now(),
        added_at_iter,
        text,
    };
    storage::append_guidance(&paths.guidance_path(), &entry)?;

    match added_at_iter {
        Some(i) => println!(
            "recorded guidance for {:?} (run at iter {i}); applies from the next iteration",
            args.name
        ),
        None => println!(
            "recorded guidance for {:?}; applies once the run starts",
            args.name
        ),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use chrono::Utc;
    use tempfile::{TempDir, tempdir};

    use super::*;
    use crate::storage::{self, CurrentStep, StateSnapshot};

    fn make_exp(name: &str) -> (TempDir, PathBuf) {
        let tmp = tempdir().unwrap();
        let root = tmp.path().join(".autorize").join(name);
        fs::create_dir_all(&root).unwrap();
        (tmp, root)
    }

    fn tell(name: &str, words: &[&str], root: &Path) -> Result<()> {
        run_with_root(
            TellArgs {
                name: name.to_string(),
                message: words.iter().map(|s| s.to_string()).collect(),
            },
            root.to_path_buf(),
        )
    }

    fn seed_state(root: &Path, iter_in_progress: Option<u64>, run_completed: u64) {
        let now = Utc::now();
        let state = StateSnapshot {
            experiment: "test".to_string(),
            branch: "autorize/test".to_string(),
            base_commit: "abc123".to_string(),
            iter_in_progress,
            current_step: CurrentStep::Idle,
            best_score: None,
            best_iter: None,
            started_at: now,
            deadline: now + chrono::Duration::seconds(3600),
            iterations_completed: run_completed,
            run_iterations_completed: run_completed,
            consecutive_noops: 0,
        };
        storage::write_state(&root.join("state.json"), &state).unwrap();
    }

    #[test]
    fn tell_appends_with_in_progress_iter() {
        let (tmp, root) = make_exp("test");
        seed_state(&root, Some(7), 6);
        tell("test", &["explore", "a", "spigot", "algorithm"], tmp.path()).unwrap();

        let got = storage::read_guidance(&root.join("guidance.jsonl")).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].text, "explore a spigot algorithm");
        assert_eq!(got[0].added_at_iter, Some(7));
    }

    #[test]
    fn tell_uses_run_completed_when_idle() {
        let (tmp, root) = make_exp("test");
        seed_state(&root, None, 4);
        tell("test", &["keep going"], tmp.path()).unwrap();

        let got = storage::read_guidance(&root.join("guidance.jsonl")).unwrap();
        assert_eq!(got[0].added_at_iter, Some(4));
    }

    #[test]
    fn tell_null_iter_when_no_state() {
        let (tmp, root) = make_exp("test");
        tell("test", &["before the run starts"], tmp.path()).unwrap();

        let got = storage::read_guidance(&root.join("guidance.jsonl")).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].added_at_iter, None);
    }

    #[test]
    fn tell_appends_multiple_in_order() {
        let (tmp, root) = make_exp("test");
        tell("test", &["first"], tmp.path()).unwrap();
        tell("test", &["second"], tmp.path()).unwrap();
        let got = storage::read_guidance(&root.join("guidance.jsonl")).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].text, "first");
        assert_eq!(got[1].text, "second");
    }

    #[test]
    fn tell_empty_message_errors() {
        let (tmp, _root) = make_exp("test");
        let err = tell("test", &["   "], tmp.path()).unwrap_err();
        assert!(format!("{err}").contains("empty"), "got: {err}");
    }

    #[test]
    fn tell_missing_experiment_errors() {
        let tmp = tempdir().unwrap();
        let err = tell("nope", &["hello"], tmp.path()).unwrap_err();
        assert!(format!("{err}").contains("not found"), "got: {err}");
    }
}
