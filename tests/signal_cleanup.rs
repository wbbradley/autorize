//! Integration test: a fatal signal to `autorize` must tear down the agent's
//! process group rather than orphaning it. Agents run in their own session
//! (so budget kills can reach grandchildren), which also detaches them from
//! the controlling terminal — without explicit teardown a Ctrl-C would kill
//! autorize and leave `claude --print` (here, a `sleep`) running.

use std::{
    path::Path,
    process::{Command, Stdio},
    thread::sleep,
    time::{Duration, Instant},
};

use tempfile::tempdir;

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_autorize")
}

fn git(args: &[&str], cwd: &Path) {
    let st = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .status()
        .unwrap_or_else(|e| panic!("git {args:?} spawn failed: {e}"));
    assert!(st.success(), "git {args:?} failed: {st:?}");
}

/// `kill -0`: true while the pid is still alive.
fn pid_alive(pid: i32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn poll_until<F: FnMut() -> bool>(timeout: Duration, mut f: F) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        sleep(Duration::from_millis(50));
    }
    f()
}

#[test]
fn sigint_tears_down_agent_process_group() {
    let repo = tempdir().unwrap();
    let scratch = tempdir().unwrap();
    let p = repo.path();

    // Minimal committed repo so `autorize run` sees a clean tree.
    git(&["init", "-q", "-b", "main"], p);
    git(&["config", "user.email", "test@example.com"], p);
    git(&["config", "user.name", "Test"], p);
    std::fs::write(p.join("README.md"), "hi\n").unwrap();
    git(&["add", "."], p);
    git(&["commit", "-qm", "init"], p);

    // The agent backgrounds a long-lived grandchild, records its pid, and
    // waits — standing in for an agent CLI that spawns a child and blocks.
    let pidfile = scratch.path().join("grandchild.pid");
    let agent_cmd = format!("sleep 300 & echo $! > {}; wait", pidfile.to_str().unwrap());
    let config = format!(
        r#"[experiment]
name = "sig"
description = "signal teardown test"

[objective]
command = "true"
direction = "min"
parse = {{ kind = "float" }}
timeout = "30s"
fail_mode = "invalid"

[boundaries]
allow_paths = []
deny_paths = []

[setup]
command = ""
timeout = "1m"

[teardown]
command = ""
timeout = "1m"

[iteration]
budget = "120s"
max_iterations = 1
keep_worktrees = false
max_consecutive_noops = 5

[schedule]
total_budget = "5m"

[agent]
command = "{agent_cmd}"
workdir_var = "AUTORIZE_WORKDIR"
stdin = "prompt"

[agent.env]

[summarize]
enabled = false
"#
    );
    let exp_dir = p.join(".autorize").join("sig");
    std::fs::create_dir_all(&exp_dir).unwrap();
    std::fs::write(exp_dir.join("config.toml"), config).unwrap();
    std::fs::write(exp_dir.join("program.md"), "# test\n").unwrap();

    let mut child = Command::new(binary())
        .args(["run", "sig"])
        .current_dir(p)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn autorize");

    // Wait for the agent to launch its grandchild.
    assert!(
        poll_until(Duration::from_secs(20), || pidfile.exists()),
        "agent never wrote its grandchild pid; autorize may have failed to start"
    );
    let grandchild: i32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .expect("pidfile holds a pid");
    assert!(pid_alive(grandchild), "grandchild should be running");

    // Simulate Ctrl-C: SIGINT only autorize (the agent is in its own session).
    let st = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(st.success(), "failed to SIGINT autorize");

    // autorize must exit promptly and take the grandchild with it.
    assert!(
        poll_until(Duration::from_secs(8), || child
            .try_wait()
            .map(|s| s.is_some())
            .unwrap_or(true)),
        "autorize did not exit after SIGINT"
    );
    let reaped = poll_until(Duration::from_secs(8), || !pid_alive(grandchild));

    // Best-effort cleanup if the fix regressed, so the test host isn't littered.
    if !reaped {
        let _ = Command::new("kill")
            .args(["-KILL", &grandchild.to_string()])
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();

    assert!(
        reaped,
        "grandchild pid {grandchild} survived SIGINT to autorize (orphaned)"
    );
}
