use std::{
    path::{Path, PathBuf},
    process::Command,
};

use globset::{Glob, GlobSetBuilder};

use crate::error::{Error, Result};

pub struct Git {
    repo_root: PathBuf,
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // fields read by Phase 4/5 callers
pub struct WorktreeEntry {
    pub path: PathBuf,
    pub head: String,
    pub branch: Option<String>,
}

#[allow(dead_code)] // several methods don't have callers until Phase 4/5
impl Git {
    pub fn new(repo_root: PathBuf) -> Self {
        Self { repo_root }
    }

    pub fn is_inside_repo(&self) -> Result<bool> {
        let (st, out, _) = run_git_raw(&["rev-parse", "--is-inside-work-tree"], &self.repo_root)?;
        Ok(st.success() && out.trim() == "true")
    }

    pub fn is_clean(&self) -> Result<bool> {
        let out = run_git(&["status", "--porcelain"], &self.repo_root)?;
        Ok(out.trim().is_empty())
    }

    pub fn head_sha(&self) -> Result<String> {
        run_git_trim(&["rev-parse", "HEAD"], &self.repo_root)
    }

    pub fn resolve_ref(&self, r: &str) -> Result<Option<String>> {
        let arg = format!("{r}^{{}}");
        let (st, out, _) = run_git_raw(&["rev-parse", "--verify", &arg], &self.repo_root)?;
        if !st.success() {
            return Ok(None);
        }
        Ok(Some(out.trim().to_string()))
    }

    pub fn branch_exists(&self, branch: &str) -> Result<bool> {
        let refname = format!("refs/heads/{branch}");
        let (st, _, _) = run_git_raw(
            &["show-ref", "--verify", "--quiet", &refname],
            &self.repo_root,
        )?;
        Ok(st.success())
    }

    pub fn create_branch_at(&self, branch: &str, sha: &str) -> Result<()> {
        run_git(&["branch", branch, sha], &self.repo_root)?;
        Ok(())
    }

    pub fn worktree_add(&self, wt: &Path, branch: &str) -> Result<()> {
        let wt_str = path_str(wt)?;
        run_git(&["worktree", "add", wt_str, branch], &self.repo_root)?;
        Ok(())
    }

    pub fn worktree_remove(&self, wt: &Path) -> Result<()> {
        let wt_str = path_str(wt)?;
        run_git(&["worktree", "remove", "--force", wt_str], &self.repo_root)?;
        Ok(())
    }

    pub fn worktree_list(&self) -> Result<Vec<WorktreeEntry>> {
        let out = run_git(&["worktree", "list", "--porcelain"], &self.repo_root)?;
        let mut entries = Vec::new();
        let mut current: Option<WorktreeEntry> = None;
        for line in out.lines() {
            if line.is_empty() {
                if let Some(e) = current.take() {
                    entries.push(e);
                }
                continue;
            }
            let (key, value) = match line.split_once(' ') {
                Some((k, v)) => (k, v),
                None => (line, ""),
            };
            match key {
                "worktree" => {
                    current = Some(WorktreeEntry {
                        path: PathBuf::from(value),
                        head: String::new(),
                        branch: None,
                    });
                }
                "HEAD" => {
                    if let Some(e) = current.as_mut() {
                        e.head = value.to_string();
                    }
                }
                "branch" => {
                    if let Some(e) = current.as_mut() {
                        e.branch = Some(
                            value
                                .strip_prefix("refs/heads/")
                                .unwrap_or(value)
                                .to_string(),
                        );
                    }
                }
                "detached" => {
                    if let Some(e) = current.as_mut() {
                        e.branch = None;
                    }
                }
                _ => {}
            }
        }
        if let Some(e) = current.take() {
            entries.push(e);
        }
        Ok(entries)
    }

    pub fn diff_against(&self, wt: &Path, base: &str) -> Result<String> {
        run_git(&["diff", base], wt)
    }

    pub fn diff_paths_against(&self, wt: &Path, base: &str) -> Result<Vec<String>> {
        let out = run_git(&["diff", "--name-only", base], wt)?;
        Ok(out
            .lines()
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect())
    }

    pub fn commit_all_in(&self, wt: &Path, msg: &str) -> Result<String> {
        run_git(&["add", "-A"], wt)?;
        run_git(
            &[
                "-c",
                "user.email=autorize@local",
                "-c",
                "user.name=autorize",
                "commit",
                "-m",
                msg,
            ],
            wt,
        )?;
        run_git_trim(&["rev-parse", "HEAD"], wt)
    }
}

#[allow(dead_code)] // wired into Phase 4 iteration logic
pub fn deny_path_matches(paths: &[String], deny_patterns: &[String]) -> Result<Vec<String>> {
    if deny_patterns.is_empty() {
        return Ok(Vec::new());
    }
    let mut builder = GlobSetBuilder::new();
    for p in deny_patterns {
        builder.add(Glob::new(p)?);
    }
    let set = builder.build()?;
    Ok(paths
        .iter()
        .filter(|p| set.is_match(p.as_str()))
        .cloned()
        .collect())
}

fn path_str(p: &Path) -> Result<&str> {
    p.to_str()
        .ok_or_else(|| Error::Git(format!("path {p:?} is not valid UTF-8")))
}

fn run_git_raw(args: &[&str], cwd: &Path) -> Result<(std::process::ExitStatus, String, String)> {
    let out = Command::new("git").args(args).current_dir(cwd).output()?;
    Ok((
        out.status,
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    ))
}

fn run_git(args: &[&str], cwd: &Path) -> Result<String> {
    let (st, out, err) = run_git_raw(args, cwd)?;
    if !st.success() {
        return Err(Error::Git(format!(
            "git {} failed: {}",
            args.join(" "),
            err.trim()
        )));
    }
    Ok(out)
}

fn run_git_trim(args: &[&str], cwd: &Path) -> Result<String> {
    run_git(args, cwd).map(|s| s.trim().to_string())
}

#[cfg(test)]
mod tests {
    use tempfile::{TempDir, tempdir};

    use super::*;

    fn init_repo() -> TempDir {
        let tmp = tempdir().unwrap();
        let p = tmp.path();
        run_git(&["init", "-q", "-b", "main"], p).unwrap();
        run_git(&["config", "user.email", "test@example.com"], p).unwrap();
        run_git(&["config", "user.name", "Test"], p).unwrap();
        std::fs::write(p.join("README.md"), "hi\n").unwrap();
        run_git(&["add", "."], p).unwrap();
        run_git(&["commit", "-qm", "init"], p).unwrap();
        tmp
    }

    #[test]
    fn is_inside_repo_true_for_initted() {
        let tmp = init_repo();
        let g = Git::new(tmp.path().to_path_buf());
        assert!(g.is_inside_repo().unwrap());
    }

    #[test]
    fn is_inside_repo_false_for_non_repo() {
        let tmp = tempdir().unwrap();
        let g = Git::new(tmp.path().to_path_buf());
        assert!(!g.is_inside_repo().unwrap());
    }

    #[test]
    fn is_clean_true_after_commit() {
        let tmp = init_repo();
        let g = Git::new(tmp.path().to_path_buf());
        assert!(g.is_clean().unwrap());
    }

    #[test]
    fn is_clean_false_after_modify() {
        let tmp = init_repo();
        std::fs::write(tmp.path().join("README.md"), "changed\n").unwrap();
        let g = Git::new(tmp.path().to_path_buf());
        assert!(!g.is_clean().unwrap());
    }

    #[test]
    fn head_sha_matches_rev_parse() {
        let tmp = init_repo();
        let g = Git::new(tmp.path().to_path_buf());
        let sha = g.head_sha().unwrap();
        assert_eq!(sha.len(), 40);
        assert!(sha.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn resolve_ref_missing_returns_none() {
        let tmp = init_repo();
        let g = Git::new(tmp.path().to_path_buf());
        assert!(g.resolve_ref("no-such-branch").unwrap().is_none());
    }

    #[test]
    fn resolve_ref_present_returns_sha() {
        let tmp = init_repo();
        let g = Git::new(tmp.path().to_path_buf());
        let got = g.resolve_ref("HEAD").unwrap().unwrap();
        assert_eq!(got, g.head_sha().unwrap());
    }

    #[test]
    fn create_branch_and_branch_exists() {
        let tmp = init_repo();
        let g = Git::new(tmp.path().to_path_buf());
        let sha = g.head_sha().unwrap();
        assert!(!g.branch_exists("autorize/test").unwrap());
        g.create_branch_at("autorize/test", &sha).unwrap();
        assert!(g.branch_exists("autorize/test").unwrap());
    }

    #[test]
    fn worktree_add_and_list_and_remove() {
        let tmp = init_repo();
        let g = Git::new(tmp.path().to_path_buf());
        let sha = g.head_sha().unwrap();
        g.create_branch_at("autorize/test", &sha).unwrap();
        let wt_dir = tempdir().unwrap();
        let wt = wt_dir.path().join("wt");
        g.worktree_add(&wt, "autorize/test").unwrap();
        let list = g.worktree_list().unwrap();
        assert!(
            list.iter()
                .any(|e| e.path == wt && e.branch.as_deref() == Some("autorize/test")),
            "wt missing from list: {list:?}"
        );
        g.worktree_remove(&wt).unwrap();
        let list = g.worktree_list().unwrap();
        assert!(
            !list.iter().any(|e| e.path == wt),
            "wt still in list: {list:?}"
        );
    }

    #[test]
    fn commit_all_in_advances_tracking_branch() {
        let tmp = init_repo();
        let g = Git::new(tmp.path().to_path_buf());
        let head = g.head_sha().unwrap();
        g.create_branch_at("autorize/test", &head).unwrap();
        let wt_dir = tempdir().unwrap();
        let wt = wt_dir.path().join("wt");
        g.worktree_add(&wt, "autorize/test").unwrap();

        std::fs::write(wt.join("README.md"), "changed\n").unwrap();
        let new_head = g.commit_all_in(&wt, "iter 1").unwrap();
        assert_ne!(new_head, head);

        let branch_head = g.resolve_ref("autorize/test").unwrap().unwrap();
        assert_eq!(branch_head, new_head);

        g.worktree_remove(&wt).unwrap();
    }

    #[test]
    fn diff_paths_against_returns_changed_files() {
        let tmp = init_repo();
        let g = Git::new(tmp.path().to_path_buf());
        let head = g.head_sha().unwrap();
        g.create_branch_at("autorize/test", &head).unwrap();
        let wt_dir = tempdir().unwrap();
        let wt = wt_dir.path().join("wt");
        g.worktree_add(&wt, "autorize/test").unwrap();

        std::fs::write(wt.join("README.md"), "changed\n").unwrap();
        let paths = g.diff_paths_against(&wt, "autorize/test").unwrap();
        assert_eq!(paths, vec!["README.md".to_string()]);
    }

    #[test]
    fn diff_against_returns_diff_text() {
        let tmp = init_repo();
        let g = Git::new(tmp.path().to_path_buf());
        let head = g.head_sha().unwrap();
        g.create_branch_at("autorize/test", &head).unwrap();
        let wt_dir = tempdir().unwrap();
        let wt = wt_dir.path().join("wt");
        g.worktree_add(&wt, "autorize/test").unwrap();

        std::fs::write(wt.join("README.md"), "changed\n").unwrap();
        let diff = g.diff_against(&wt, "autorize/test").unwrap();
        assert!(diff.contains("+++"), "diff lacks +++ line: {diff}");
        assert!(diff.contains("changed"), "diff lacks new content: {diff}");
    }

    #[test]
    fn deny_path_matches_basic() {
        let paths = vec!["foo.lock".to_string(), "src/main.rs".to_string()];
        let patterns = vec!["*.lock".to_string()];
        let got = deny_path_matches(&paths, &patterns).unwrap();
        assert_eq!(got, vec!["foo.lock".to_string()]);
    }

    #[test]
    fn deny_path_matches_globstar() {
        let paths = vec![
            ".autorize/state.json".to_string(),
            "src/main.rs".to_string(),
        ];
        let patterns = vec![".autorize/**".to_string()];
        let got = deny_path_matches(&paths, &patterns).unwrap();
        assert_eq!(got, vec![".autorize/state.json".to_string()]);
    }

    #[test]
    fn deny_path_matches_empty_returns_empty() {
        let paths = vec!["a".to_string(), "b".to_string()];
        let patterns: Vec<String> = vec![];
        let got = deny_path_matches(&paths, &patterns).unwrap();
        assert!(got.is_empty());
    }
}
