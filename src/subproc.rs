use std::{
    collections::BTreeMap,
    io::{Read, Write},
    os::unix::process::{CommandExt, ExitStatusExt},
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use nix::{
    sys::signal::{Signal, killpg},
    unistd::Pid,
};

use crate::error::{Error, Result};

#[derive(Debug)]
#[allow(dead_code)] // fields consumed by Phase 4 callers
pub struct CommandOutput {
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

const GRACE: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(20);
const GRACE_POLL: Duration = Duration::from_millis(100);

/// Spawn `bash -lc <command>` in a new session (so the resulting pgroup
/// covers any grandchildren), drain stdout/stderr in background threads,
/// poll for completion, and on budget expiry send SIGTERM to the pgroup,
/// wait up to `GRACE`, then SIGKILL.
#[allow(dead_code)] // wired in by Phase 4
pub fn run_command_with_budget(
    command: &str,
    workdir: &Path,
    budget: Duration,
    extra_env: &BTreeMap<String, String>,
    stdin_payload: Option<Vec<u8>>,
) -> Result<CommandOutput> {
    let mut cmd = Command::new("bash");
    cmd.arg("-lc").arg(command).current_dir(workdir);
    cmd.stdin(if stdin_payload.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    // SAFETY: the closure runs after fork in the child; we only call
    // `setsid(2)`, which is async-signal-safe.
    unsafe {
        cmd.pre_exec(|| {
            nix::unistd::setsid()
                .map(|_| ())
                .map_err(|e| std::io::Error::from_raw_os_error(e as i32))
        });
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| Error::Subproc(format!("spawn failed: {e}")))?;
    // After setsid, the child's pid is the new session leader and its pgid
    // equals its pid.
    let pgid = Pid::from_raw(child.id() as i32);

    let stdin_thread = stdin_payload.map(|payload| {
        let mut handle = child.stdin.take().expect("stdin was piped");
        std::thread::spawn(move || {
            let _ = handle.write_all(&payload);
            drop(handle);
        })
    });

    let mut stdout_pipe = child.stdout.take().expect("stdout was piped");
    let mut stderr_pipe = child.stderr.take().expect("stderr was piped");
    let stdout_thread = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = stdout_pipe.read_to_string(&mut buf);
        buf
    });
    let stderr_thread = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = stderr_pipe.read_to_string(&mut buf);
        buf
    });

    let start = Instant::now();
    let mut timed_out = false;
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if start.elapsed() >= budget {
            timed_out = true;
            let _ = killpg(pgid, Signal::SIGTERM);
            let grace_start = Instant::now();
            loop {
                if child.try_wait()?.is_some() {
                    break;
                }
                if grace_start.elapsed() >= GRACE {
                    let _ = killpg(pgid, Signal::SIGKILL);
                    break;
                }
                std::thread::sleep(GRACE_POLL);
            }
            break child.wait()?;
        }
        std::thread::sleep(POLL);
    };

    let stdout = stdout_thread.join().unwrap_or_default();
    let stderr = stderr_thread.join().unwrap_or_default();
    if let Some(t) = stdin_thread {
        let _ = t.join();
    }

    Ok(CommandOutput {
        exit_code: status.code(),
        signal: status.signal(),
        stdout,
        stderr,
        timed_out,
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn runs_simple_echo_with_no_budget_hit() {
        let dir = tempdir().unwrap();
        let out = run_command_with_budget(
            "echo hi",
            dir.path(),
            Duration::from_secs(5),
            &BTreeMap::new(),
            None,
        )
        .unwrap();
        assert_eq!(out.exit_code, Some(0));
        assert_eq!(out.stdout, "hi\n");
        assert!(!out.timed_out);
    }

    #[test]
    fn times_out_long_sleep() {
        let dir = tempdir().unwrap();
        let started = Instant::now();
        let out = run_command_with_budget(
            "sleep 30",
            dir.path(),
            Duration::from_secs(1),
            &BTreeMap::new(),
            None,
        )
        .unwrap();
        let elapsed = started.elapsed();
        assert!(out.timed_out, "expected timed_out: {out:?}");
        assert!(
            elapsed < Duration::from_secs(8),
            "took too long: {elapsed:?}"
        );
        // The child should have been killed by signal (no exit code) or
        // exited via shell with a non-zero status; either is acceptable
        // as long as it died quickly.
        assert!(
            out.signal.is_some() || out.exit_code.is_some(),
            "no status: {out:?}"
        );
    }

    #[test]
    fn kills_grandchildren_via_pgroup() {
        use nix::sys::signal::kill;

        let dir = tempdir().unwrap();
        let pidfile = dir.path().join("pid");
        let mut env = BTreeMap::new();
        env.insert(
            "AUTORIZE_PIDFILE".to_string(),
            pidfile.to_str().unwrap().to_string(),
        );
        // Spawn a backgrounded sleep, record its pid, then wait. The
        // backgrounded sleep is in the same pgroup as bash (we called
        // setsid pre-exec), so killpg should reach it.
        let cmd = r#"sleep 30 & echo $! > "$AUTORIZE_PIDFILE"; wait"#;
        let out =
            run_command_with_budget(cmd, dir.path(), Duration::from_secs(1), &env, None).unwrap();
        assert!(out.timed_out, "expected timed_out: {out:?}");

        let pid_str = std::fs::read_to_string(&pidfile).expect("pidfile was written");
        let pid: i32 = pid_str.trim().parse().expect("pidfile holds a pid");

        // Poll for up to 3s waiting for init to reap the (now-orphaned,
        // killed) grandchild. kill(pid, None) returns ESRCH once it's gone.
        let start = Instant::now();
        let mut reaped = false;
        while start.elapsed() < Duration::from_secs(3) {
            match kill(Pid::from_raw(pid), None) {
                Err(nix::errno::Errno::ESRCH) => {
                    reaped = true;
                    break;
                }
                _ => std::thread::sleep(Duration::from_millis(50)),
            }
        }
        assert!(reaped, "grandchild pid {pid} survived pgroup kill");
    }

    #[test]
    fn captures_large_stdout() {
        let dir = tempdir().unwrap();
        let out = run_command_with_budget(
            "yes x | head -c 262144; echo",
            dir.path(),
            Duration::from_secs(10),
            &BTreeMap::new(),
            None,
        )
        .unwrap();
        assert_eq!(out.exit_code, Some(0));
        assert!(!out.timed_out);
        assert!(
            out.stdout.len() >= 262144,
            "stdout too short: {}",
            out.stdout.len()
        );
    }

    #[test]
    fn passes_extra_env() {
        let dir = tempdir().unwrap();
        let mut env = BTreeMap::new();
        env.insert("FOO".to_string(), "bar".to_string());
        let out = run_command_with_budget(
            "echo \"$FOO\"",
            dir.path(),
            Duration::from_secs(5),
            &env,
            None,
        )
        .unwrap();
        assert_eq!(out.exit_code, Some(0));
        assert_eq!(out.stdout.trim(), "bar");
    }

    #[test]
    fn stdin_payload_delivered() {
        let dir = tempdir().unwrap();
        let out = run_command_with_budget(
            "cat",
            dir.path(),
            Duration::from_secs(5),
            &BTreeMap::new(),
            Some(b"hello\n".to_vec()),
        )
        .unwrap();
        assert_eq!(out.exit_code, Some(0));
        assert_eq!(out.stdout, "hello\n");
    }
}
