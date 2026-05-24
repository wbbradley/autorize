use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use serde_json::Value;
use tempfile::{TempDir, tempdir};

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_autorize")
}

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("examples")
        .join("pi-digits")
}

fn copy_example(dst: &Path) {
    copy_dir(&fixture_root(), dst);
}

fn copy_dir(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let ty = entry.file_type().unwrap();
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir(&from, &to);
        } else if ty.is_file() {
            fs::copy(&from, &to).unwrap();
            if from
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e == "sh")
            {
                let mut perms = fs::metadata(&to).unwrap().permissions();
                perms.set_mode(0o755);
                fs::set_permissions(&to, perms).unwrap();
            }
        }
    }
}

fn git(args: &[&str], cwd: &Path) {
    let st = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .status()
        .unwrap_or_else(|e| panic!("git {args:?} spawn failed: {e}"));
    assert!(st.success(), "git {args:?} failed: {st:?}");
}

fn git_init_commit(dir: &Path) {
    git(&["init", "-q", "-b", "main"], dir);
    git(&["config", "user.email", "test@example.com"], dir);
    git(&["config", "user.name", "Test"], dir);
    git(&["add", "."], dir);
    git(&["commit", "-qm", "init"], dir);
}

fn head_sha(dir: &Path) -> String {
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "rev-parse HEAD failed: {out:?}");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn rev_parse(dir: &Path, refname: &str) -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "--verify", refname])
        .current_dir(dir)
        .output()
        .unwrap();
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8(out.stdout).unwrap().trim().to_string())
}

fn run_autorize(args: &[&str], dir: &Path) -> Output {
    Command::new(binary())
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("spawn autorize: {e}"))
}

fn read_jsonl(path: &Path) -> Vec<Value> {
    let text = fs::read_to_string(path).expect("iterations.jsonl missing");
    text.lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str::<Value>(l).expect("non-json line"))
        .collect()
}

fn bootstrap() -> TempDir {
    let tmp = tempdir().unwrap();
    copy_example(tmp.path());
    git_init_commit(tmp.path());
    tmp
}

#[test]
fn loop_converges_with_merges_and_discards() {
    let tmp = bootstrap();
    let p = tmp.path();

    let out = run_autorize(&["run", "pi"], p);
    assert!(
        out.status.success(),
        "autorize run failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let log = p.join(".autorize/pi/iterations.jsonl");
    let recs = read_jsonl(&log);
    assert!(recs.len() >= 3, "expected >=3 records, got {}", recs.len());

    let merged = recs.iter().filter(|r| r["outcome"] == "merged").count();
    let discarded = recs.iter().filter(|r| r["outcome"] == "discarded").count();
    assert!(merged >= 1, "expected >=1 merged record, got recs={recs:?}");
    assert!(
        discarded >= 1,
        "expected >=1 discarded record, got recs={recs:?}"
    );

    for (idx, rec) in recs.iter().enumerate() {
        let want = (idx as u64) + 1;
        let got = rec["iter"].as_u64().unwrap();
        assert_eq!(got, want, "iter numbers must be 1..=N strict; rec={rec:?}");
    }

    let state_text = fs::read_to_string(p.join(".autorize/pi/state.json")).unwrap();
    let state: Value = serde_json::from_str(&state_text).unwrap();
    let best = state["best_score"].as_f64().expect("best_score is null");
    assert!(best < 0.1, "best_score {best} should be < 0.1");
    let best_iter = state["best_iter"].as_u64().expect("best_iter null");
    assert!(best_iter >= 1, "best_iter {best_iter} should be >= 1");

    // Read value.txt from the autorize/pi branch via a fresh worktree.
    let inspect_dir = tempdir().unwrap();
    let inspect = inspect_dir.path().join("wt");
    git(
        &["worktree", "add", inspect.to_str().unwrap(), "autorize/pi"],
        p,
    );
    let final_value: f64 = fs::read_to_string(inspect.join("value.txt"))
        .unwrap()
        .trim()
        .parse()
        .expect("value.txt should be a float");
    git(
        &["worktree", "remove", "--force", inspect.to_str().unwrap()],
        p,
    );

    let pi = std::f64::consts::PI;
    let final_dist = (pi - final_value).abs();
    let start_dist = (pi - 3.0_f64).abs();
    assert!(
        final_dist < start_dist,
        "final value {final_value} (dist {final_dist}) should be closer to π than 3.0 (dist {start_dist})"
    );
}

#[test]
fn dirty_tree_refused_then_allow_dirty_succeeds() {
    let tmp = bootstrap();
    let p = tmp.path();

    // Introduce an unrelated untracked file outside .autorize/.
    fs::write(p.join("stray.txt"), "x\n").unwrap();

    let out = run_autorize(&["run", "pi"], p);
    assert!(
        !out.status.success(),
        "expected non-zero exit on dirty tree; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("uncommitted"),
        "expected 'uncommitted' in stderr, got: {stderr}"
    );

    let out2 = run_autorize(&["run", "pi", "--allow-dirty"], p);
    assert!(
        out2.status.success(),
        "expected success with --allow-dirty; stdout={} stderr={}",
        String::from_utf8_lossy(&out2.stdout),
        String::from_utf8_lossy(&out2.stderr)
    );

    let recs = read_jsonl(&p.join(".autorize/pi/iterations.jsonl"));
    assert!(
        !recs.is_empty(),
        "expected >=1 iteration record with --allow-dirty"
    );
}

#[test]
fn deny_path_violation_yields_denied_outcome() {
    let tmp = tempdir().unwrap();
    let p = tmp.path();
    copy_example(p);

    // Swap the agent to bad-agent.sh and lower max_iterations to 1.
    let cfg_path = p.join(".autorize/pi/config.toml");
    let cfg = fs::read_to_string(&cfg_path).unwrap();
    let cfg = cfg.replace(
        "command = \"bash mock-agent.sh {iter}\"",
        "command = \"bash bad-agent.sh {iter}\"",
    );
    let cfg = cfg.replace("max_iterations = 6", "max_iterations = 1");
    fs::write(&cfg_path, cfg).unwrap();

    git_init_commit(p);

    let out = run_autorize(&["run", "pi"], p);
    assert!(
        out.status.success(),
        "autorize run failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let recs = read_jsonl(&p.join(".autorize/pi/iterations.jsonl"));
    assert_eq!(recs.len(), 1, "expected exactly 1 record, got {recs:?}");
    assert_eq!(
        recs[0]["outcome"], "denied",
        "expected denied outcome; rec={:?}",
        recs[0]
    );

    let state_text = fs::read_to_string(p.join(".autorize/pi/state.json")).unwrap();
    let state: Value = serde_json::from_str(&state_text).unwrap();
    let base = state["base_commit"]
        .as_str()
        .expect("base_commit must be a string")
        .to_string();

    let branch_head = rev_parse(p, "autorize/pi").expect("autorize/pi branch missing");
    assert_eq!(
        branch_head, base,
        "tracking branch must not advance on denied iteration"
    );
}

#[test]
fn central_log_appends_and_tees_child_output() {
    // Part C: every run writes a project-root `logs/autorize.log` that holds
    // both autorize's own narrative and the teed child stdout/stderr, opened
    // in append mode so repeated runs extend rather than truncate it.
    let tmp = bootstrap();
    let p = tmp.path();

    // Make the agent emit a unique marker on stdout so we can prove its output
    // was teed into the central log (not just autorize's own narrative).
    let cfg_path = p.join(".autorize/pi/config.toml");
    let cfg = fs::read_to_string(&cfg_path).unwrap();
    let cfg = cfg.replace(
        "command = \"bash mock-agent.sh {iter}\"",
        "command = \"bash mock-agent.sh {iter} && echo TEE_MARKER_7F3A\"",
    );
    fs::write(&cfg_path, cfg).unwrap();

    let out = run_autorize(&["run", "pi"], p);
    assert!(
        out.status.success(),
        "autorize run failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let log_path = p.join("logs/autorize.log");
    assert!(log_path.is_file(), "logs/autorize.log should exist");
    let log1 = fs::read_to_string(&log_path).unwrap();
    assert!(
        log1.contains("TEE_MARKER_7F3A"),
        "central log missing teed child stdout: {log1}"
    );
    assert!(
        log1.contains("iter 1:"),
        "central log missing iteration narrative: {log1}"
    );
    let len1 = fs::metadata(&log_path).unwrap().len();

    // A second run appends (the loop is already at max_iterations and stops,
    // but still logs); the file must grow and keep the earlier content.
    let out2 = run_autorize(&["run", "pi"], p);
    assert!(out2.status.success(), "second run failed: {out2:?}");
    let log2 = fs::read_to_string(&log_path).unwrap();
    let len2 = fs::metadata(&log_path).unwrap().len();
    assert!(
        len2 > len1,
        "log should grow on a second run (append): {len1} -> {len2}"
    );
    assert!(
        log2.contains("TEE_MARKER_7F3A"),
        "append mode must preserve earlier content"
    );
}

#[test]
fn resume_records_killed_then_continues() {
    let tmp = tempdir().unwrap();
    let p = tmp.path();
    copy_example(p);

    let cfg_path = p.join(".autorize/pi/config.toml");
    let cfg = fs::read_to_string(&cfg_path).unwrap();
    let cfg = cfg.replace("max_iterations = 6", "max_iterations = 3");
    fs::write(&cfg_path, cfg).unwrap();

    git_init_commit(p);

    // Pre-create the tracking branch at HEAD.
    let sha = head_sha(p);
    git(&["branch", "autorize/pi", &sha], p);

    // Hand-write state.json simulating a mid-iteration crash on iter 1.
    let state_json = format!(
        r#"{{
  "experiment": "pi",
  "branch": "autorize/pi",
  "base_commit": "{sha}",
  "iter_in_progress": 1,
  "current_step": "InvokeAgent",
  "best_score": null,
  "best_iter": null,
  "started_at": "2026-05-20T00:00:00Z",
  "deadline": "2099-01-01T00:00:00Z",
  "iterations_completed": 0,
  "consecutive_noops": 0
}}
"#,
    );
    fs::write(p.join(".autorize/pi/state.json"), state_json).unwrap();

    let out = run_autorize(&["resume", "pi"], p);
    assert!(
        out.status.success(),
        "autorize resume failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let recs = read_jsonl(&p.join(".autorize/pi/iterations.jsonl"));
    assert_eq!(recs.len(), 3, "expected 3 records, got {recs:?}");
    assert_eq!(recs[0]["iter"].as_u64(), Some(1));
    assert_eq!(recs[0]["outcome"], "killed");
    assert_eq!(recs[0]["notes"], "resumed after crash");
    assert_eq!(recs[1]["iter"].as_u64(), Some(2));
    assert_eq!(recs[2]["iter"].as_u64(), Some(3));
    assert_eq!(
        recs[1]["outcome"], "merged",
        "iter 2 should merge; rec={:?}",
        recs[1]
    );
    assert_eq!(
        recs[2]["outcome"], "merged",
        "iter 3 should merge; rec={:?}",
        recs[2]
    );
}
