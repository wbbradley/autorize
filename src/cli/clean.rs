use std::{
    env,
    path::{Path, PathBuf},
};

use tracing::info;

use crate::{
    error::{Error, Result},
    experiment::ExperimentPaths,
    worktree::Git,
};

#[derive(clap::Args, Debug)]
/// Tidy a finished or abandoned experiment without destroying its historical
/// record. Frees the `autorize/<name>` tracking branch if a stale worktree
/// still holds it (pre-v0.2.4 residue), clears any leftover `git add -A`
/// index on kept iteration worktrees, and prunes registrations for `wt/`
/// directories that no longer exist. `iterations.jsonl` and `state.json` are
/// never touched; kept worktree checkouts and per-iter artifacts are
/// preserved unless `--remove-worktrees` is given.
pub struct CleanArgs {
    /// Experiment name (must exist under `.autorize/<name>/`).
    pub name: String,
    /// Also delete the kept `wt/` worktree checkouts to reclaim disk. The
    /// durable log and the per-iter artifacts (prompt.md, agent.stdout,
    /// agent.stderr, changes.diff) are still preserved.
    #[arg(long)]
    pub remove_worktrees: bool,
}

pub fn run(args: CleanArgs) -> anyhow::Result<()> {
    let project_root = env::current_dir()?;
    run_with_root(args, project_root)?;
    Ok(())
}

pub(crate) fn run_with_root(args: CleanArgs, project_root: PathBuf) -> Result<()> {
    let paths = ExperimentPaths::new(project_root.clone(), args.name.clone());
    if !paths.root().exists() {
        return Err(Error::Config(format!(
            "experiment {:?} not found at {:?}",
            args.name,
            paths.root()
        )));
    }

    let git = Git::new(paths.project_root().clone());
    if !git.is_inside_repo()? {
        return Err(Error::Git(
            "not a git repository (cd into one or `git init`)".to_string(),
        ));
    }

    let branch = format!("autorize/{}", args.name);
    let exp_root = canonical(&paths.root());
    let main_root = canonical(&project_root);

    let mut freed_branch = false;
    let mut unstaged = 0u32;
    let mut removed = 0u32;

    for wt in git.worktree_list()? {
        // Only act on worktrees that physically exist; vanished ones are
        // cleared by `git worktree prune` below.
        let Some(wt_canon) = canonical(&wt.path) else {
            continue;
        };
        let under_exp = exp_root.as_ref().is_some_and(|e| wt_canon.starts_with(e));

        if args.remove_worktrees && under_exp {
            git.worktree_remove(&wt.path)?;
            removed += 1;
            continue;
        }

        // Free the tracking branch if a non-main worktree still holds it.
        let is_main = main_root.as_ref().is_some_and(|m| &wt_canon == m);
        if wt.branch.as_deref() == Some(branch.as_str()) && !is_main {
            git.detach_worktree(&wt.path)?;
            freed_branch = true;
        }

        // Clear any stale `git add -A` index on a kept iteration worktree.
        if under_exp {
            git.unstage_all_in(&wt.path)?;
            unstaged += 1;
        }
    }

    git.worktree_prune()?;

    if freed_branch {
        info!("freed tracking branch {branch} from a worktree that held it");
    }
    if removed > 0 {
        info!(
            "removed {removed} worktree checkout(s) under .autorize/{}",
            args.name
        );
    } else if unstaged > 0 {
        info!(
            "unstaged {unstaged} kept worktree(s) under .autorize/{}",
            args.name
        );
    }
    info!(
        "cleaned experiment {:?}: branch and registrations tidied; log and records preserved",
        args.name
    );
    Ok(())
}

fn canonical(p: &Path) -> Option<PathBuf> {
    std::fs::canonicalize(p).ok()
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path, process::Command};

    use tempfile::{TempDir, tempdir};

    use super::*;

    fn run_cmd(args: &[&str], cwd: &Path) {
        let st = Command::new(args[0])
            .args(&args[1..])
            .current_dir(cwd)
            .status()
            .unwrap_or_else(|e| panic!("spawning {args:?} failed: {e}"));
        assert!(st.success(), "command {args:?} failed: {st:?}");
    }

    fn git_out(args: &[&str], cwd: &Path) -> String {
        let out = Command::new(args[0])
            .args(&args[1..])
            .current_dir(cwd)
            .output()
            .unwrap_or_else(|e| panic!("spawning {args:?} failed: {e}"));
        assert!(out.status.success(), "command {args:?} failed: {out:?}");
        String::from_utf8(out.stdout).unwrap()
    }

    fn git_status(args: &[&str], cwd: &Path) -> bool {
        Command::new(args[0])
            .args(&args[1..])
            .current_dir(cwd)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn init_repo() -> TempDir {
        let tmp = tempdir().unwrap();
        let p = tmp.path();
        run_cmd(&["git", "init", "-q", "-b", "main"], p);
        run_cmd(&["git", "config", "user.email", "test@example.com"], p);
        run_cmd(&["git", "config", "user.name", "Test"], p);
        fs::write(p.join("value.txt"), "3.0\n").unwrap();
        run_cmd(&["git", "add", "."], p);
        run_cmd(&["git", "commit", "-qm", "init"], p);
        tmp
    }

    fn make_exp_dir(p: &Path, name: &str) -> PathBuf {
        let root = p.join(".autorize").join(name);
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn clean(name: &str, remove_worktrees: bool, p: &Path) {
        run_with_root(
            CleanArgs {
                name: name.to_string(),
                remove_worktrees,
            },
            p.to_path_buf(),
        )
        .unwrap_or_else(|e| panic!("clean failed: {e}"));
    }

    /// Advance `autorize/<name>` one commit past `base` via a throwaway
    /// detached worktree, mimicking accepted iterations that moved the tip out
    /// from under a stale checkout.
    fn advance_branch(git: &Git, _p: &Path, branch: &str) {
        let scratch = tempdir().unwrap();
        let wt = scratch.path().join("wt");
        git.worktree_add(&wt, branch).unwrap();
        fs::write(wt.join("value.txt"), "3.14\n").unwrap();
        let sha = git.commit_all_in(&wt, "advance").unwrap();
        git.update_branch_ref(branch, &sha).unwrap();
        git.worktree_remove(&wt).unwrap();
    }

    #[test]
    fn clean_frees_branch_held_by_worktree() {
        let tmp = init_repo();
        let p = tmp.path();
        let git = Git::new(p.to_path_buf());
        let sha = git.head_sha().unwrap();
        git.create_branch_at("autorize/test", &sha).unwrap();
        let exp = make_exp_dir(p, "test");

        // Legacy residue: an iteration worktree with the branch checked out
        // (no --detach), then the tip advanced out from under it.
        let iter1 = exp.join("iter-0001").join("wt");
        fs::create_dir_all(iter1.parent().unwrap()).unwrap();
        run_cmd(
            &[
                "git",
                "worktree",
                "add",
                iter1.to_str().unwrap(),
                "autorize/test",
            ],
            p,
        );
        advance_branch(&git, p, "autorize/test");

        // Precondition: the branch is held, so it can't be checked out in main.
        assert!(
            !git_status(&["git", "checkout", "-q", "autorize/test"], p),
            "branch should be held before clean"
        );

        clean("test", false, p);

        // No worktree holds the branch any more.
        let wts = git.worktree_list().unwrap();
        assert!(
            wts.iter()
                .all(|w| w.branch.as_deref() != Some("autorize/test")),
            "branch still held: {wts:?}"
        );
        // And it is freely checkout-able from the main repo.
        assert!(
            git_status(&["git", "checkout", "-q", "autorize/test"], p),
            "checkout of freed branch should succeed"
        );
        // The commit stack is intact (branch still resolves).
        assert!(git.branch_exists("autorize/test").unwrap());
    }

    #[test]
    fn clean_unstages_kept_worktree() {
        let tmp = init_repo();
        let p = tmp.path();
        let git = Git::new(p.to_path_buf());
        let sha = git.head_sha().unwrap();
        git.create_branch_at("autorize/test", &sha).unwrap();
        let exp = make_exp_dir(p, "test");

        let wt = exp.join("iter-0002").join("wt");
        fs::create_dir_all(wt.parent().unwrap()).unwrap();
        git.worktree_add(&wt, "autorize/test").unwrap();
        // Leave a stale fully-staged index (the pre-Part-A residue).
        fs::write(wt.join("value.txt"), "9.9\n").unwrap();
        run_cmd(&["git", "add", "-A"], &wt);

        clean("test", false, p);

        let cached = git_out(&["git", "diff", "--cached"], &wt);
        assert!(cached.trim().is_empty(), "index still staged: {cached:?}");
    }

    #[test]
    fn clean_prunes_dangling_registration() {
        let tmp = init_repo();
        let p = tmp.path();
        let git = Git::new(p.to_path_buf());
        let sha = git.head_sha().unwrap();
        git.create_branch_at("autorize/test", &sha).unwrap();
        let exp = make_exp_dir(p, "test");

        let wt = exp.join("iter-0003").join("wt");
        fs::create_dir_all(wt.parent().unwrap()).unwrap();
        git.worktree_add(&wt, "autorize/test").unwrap();
        // Delete the checkout dir behind git's back -> dangling registration.
        fs::remove_dir_all(&wt).unwrap();

        clean("test", false, p);

        let wts = git.worktree_list().unwrap();
        assert!(
            wts.iter()
                .all(|w| canonical(&w.path) != fs::canonicalize(&wt).ok()),
            "dangling registration not pruned: {wts:?}"
        );
    }

    #[test]
    fn clean_remove_worktrees_deletes_checkout_keeps_artifacts() {
        let tmp = init_repo();
        let p = tmp.path();
        let git = Git::new(p.to_path_buf());
        let sha = git.head_sha().unwrap();
        git.create_branch_at("autorize/test", &sha).unwrap();
        let exp = make_exp_dir(p, "test");

        let iter_dir = exp.join("iter-0004");
        let wt = iter_dir.join("wt");
        fs::create_dir_all(&iter_dir).unwrap();
        git.worktree_add(&wt, "autorize/test").unwrap();
        // Sibling per-iter artifacts and the durable log must survive.
        fs::write(iter_dir.join("changes.diff"), "diff\n").unwrap();
        fs::write(exp.join("iterations.jsonl"), "{}\n").unwrap();

        clean("test", true, p);

        assert!(!wt.exists(), "wt/ should be removed");
        assert!(
            iter_dir.join("changes.diff").exists(),
            "per-iter artifact should be preserved"
        );
        assert!(
            exp.join("iterations.jsonl").exists(),
            "durable log must be preserved"
        );
    }

    #[test]
    fn clean_errors_when_experiment_missing() {
        let tmp = init_repo();
        let err = run_with_root(
            CleanArgs {
                name: "nope".to_string(),
                remove_worktrees: false,
            },
            tmp.path().to_path_buf(),
        )
        .unwrap_err();
        assert!(format!("{err}").contains("not found"), "got: {err}");
    }
}
