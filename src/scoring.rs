use std::{collections::BTreeMap, path::Path, time::Duration};

use crate::{
    config::{Direction, FailMode, Objective, ParseSpec},
    error::{Error, Result},
    subproc,
};

#[derive(Debug)]
#[allow(dead_code)] // fields consumed in Phase 4 by the iteration state machine
pub struct ScoreOutput {
    pub score: Option<f64>,
    pub failure: Option<ScoreFailure>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)] // Spawn variant constructed only via run_with_timeout failure path
pub enum ScoreFailure {
    Spawn(String),
    Exit(i32),
    Signal,
    Timeout,
    Parse(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ScoreDecision {
    Use(f64),
    Discard,
    Abort(String),
}

#[allow(dead_code)] // wired in by Phase 4
pub fn score(workdir: &Path, obj: &Objective) -> Result<ScoreOutput> {
    let (exit_code, stdout, stderr, timed_out) =
        match run_with_timeout(&obj.command, workdir, obj.timeout) {
            Ok(v) => v,
            Err(Error::Subproc(msg)) => {
                return Ok(ScoreOutput {
                    score: None,
                    failure: Some(ScoreFailure::Spawn(msg)),
                    stdout: String::new(),
                    stderr: String::new(),
                    timed_out: false,
                    exit_code: None,
                });
            }
            Err(e) => return Err(e),
        };

    let (score_val, failure) = if timed_out {
        (None, Some(ScoreFailure::Timeout))
    } else if let Some(code) = exit_code {
        if code != 0 {
            (None, Some(ScoreFailure::Exit(code)))
        } else {
            match parse_score(&stdout, &obj.parse) {
                Ok(v) => (Some(v), None),
                Err(e) => (None, Some(e)),
            }
        }
    } else {
        (None, Some(ScoreFailure::Signal))
    };

    Ok(ScoreOutput {
        score: score_val,
        failure,
        stdout,
        stderr,
        timed_out,
        exit_code,
    })
}

#[allow(dead_code)] // wired in by Phase 4
pub fn apply_fail_mode(out: &ScoreOutput, obj: &Objective) -> ScoreDecision {
    if let Some(s) = out.score {
        return ScoreDecision::Use(s);
    }
    match obj.fail_mode {
        FailMode::Invalid => ScoreDecision::Discard,
        // Use finite sentinels (f64::MAX/MIN) rather than +/-infinity so the
        // value round-trips through JSON (serde_json serializes non-finite f64
        // as `null`, which Option<f64> then reads back as None, losing the
        // sentinel and silently breaking comparison/persistence).
        FailMode::Worst => match obj.direction {
            Direction::Min => ScoreDecision::Use(f64::MAX),
            Direction::Max => ScoreDecision::Use(f64::MIN),
        },
        FailMode::Abort => {
            let reason = out
                .failure
                .as_ref()
                .map(describe_failure)
                .unwrap_or_else(|| "no score".to_string());
            ScoreDecision::Abort(reason)
        }
    }
}

/// Render a `ScoreFailure` as a short human-readable reason. Shared by
/// `apply_fail_mode`'s `Abort` arm and the `Invalid`-outcome `notes` the
/// iteration state machine records, so both phrase failures identically.
pub fn describe_failure(f: &ScoreFailure) -> String {
    match f {
        ScoreFailure::Spawn(s) => format!("spawn: {s}"),
        ScoreFailure::Exit(c) => format!("exit code {c}"),
        ScoreFailure::Signal => "killed by signal".to_string(),
        ScoreFailure::Timeout => "timed out".to_string(),
        ScoreFailure::Parse(s) => format!("parse: {s}"),
    }
}

fn parse_score(stdout: &str, spec: &ParseSpec) -> std::result::Result<f64, ScoreFailure> {
    match spec {
        ParseSpec::Float => parse_float(stdout),
        ParseSpec::Regex { pattern } => parse_regex(stdout, pattern),
        ParseSpec::Jq { path } => parse_jq(stdout, path),
    }
}

fn parse_float(s: &str) -> std::result::Result<f64, ScoreFailure> {
    let v = s
        .trim()
        .parse::<f64>()
        .map_err(|e| ScoreFailure::Parse(format!("not a float: {e}")))?;
    finite_or_parse_err(v)
}

fn finite_or_parse_err(v: f64) -> std::result::Result<f64, ScoreFailure> {
    if v.is_finite() {
        Ok(v)
    } else {
        Err(ScoreFailure::Parse(format!("non-finite score: {v}")))
    }
}

fn parse_regex(s: &str, pattern: &str) -> std::result::Result<f64, ScoreFailure> {
    let re =
        regex::Regex::new(pattern).map_err(|e| ScoreFailure::Parse(format!("bad regex: {e}")))?;
    let caps = re
        .captures(s)
        .ok_or_else(|| ScoreFailure::Parse(format!("regex {pattern:?} matched nothing")))?;
    let m = caps
        .get(1)
        .ok_or_else(|| ScoreFailure::Parse(format!("regex {pattern:?} has no capture group")))?;
    let text = m.as_str();
    let v = text
        .trim()
        .parse::<f64>()
        .map_err(|e| ScoreFailure::Parse(format!("capture {text:?} not a float: {e}")))?;
    finite_or_parse_err(v)
}

fn parse_jq(s: &str, path: &str) -> std::result::Result<f64, ScoreFailure> {
    // serde_json_path speaks JSONPath ($.foo.bar). v1 users typically pass jq-style
    // (.foo.bar); accept both by rewriting a leading bare `.` to `$.`.
    let rewritten = if let Some(rest) = path.strip_prefix('.') {
        format!("$.{rest}")
    } else {
        path.to_string()
    };
    let parsed: serde_json::Value =
        serde_json::from_str(s).map_err(|e| ScoreFailure::Parse(format!("invalid json: {e}")))?;
    let p = serde_json_path::JsonPath::parse(&rewritten)
        .map_err(|e| ScoreFailure::Parse(format!("bad jq path: {e}")))?;
    let nodes = p.query(&parsed);
    let vals = nodes.all();
    if vals.len() != 1 {
        return Err(ScoreFailure::Parse(format!(
            "jq path {path:?} matched {} values, expected 1",
            vals.len()
        )));
    }
    let v = vals[0]
        .as_f64()
        .ok_or_else(|| ScoreFailure::Parse(format!("jq path {path:?} value is not a number")))?;
    // Note: serde_json rejects literal `NaN` / `Infinity` JSON at parse time
    // and any numeric literal it does accept fits in finite f64, so a
    // dedicated parse_jq non-finite test is not practical. We still run the
    // finite check here for defense-in-depth.
    finite_or_parse_err(v)
}

// Delegates to `subproc::run_command_with_budget` so objective subprocesses
// share the same SIGTERM-pgroup-then-SIGKILL kill path as the agent (and
// can't orphan grandchildren). Phase 3 refactor.
fn run_with_timeout(
    command: &str,
    workdir: &Path,
    timeout: Duration,
) -> Result<(Option<i32>, String, String, bool)> {
    let out = subproc::run_command_with_budget(command, workdir, timeout, &BTreeMap::new(), None)?;
    Ok((out.exit_code, out.stdout, out.stderr, out.timed_out))
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use tempfile::tempdir;

    use super::*;

    fn obj(parse: ParseSpec, fail_mode: FailMode, direction: Direction) -> Objective {
        Objective {
            command: String::new(),
            direction,
            parse,
            timeout: Duration::from_secs(5),
            fail_mode,
        }
    }

    #[test]
    fn parse_float_ok() {
        assert_eq!(parse_float("2.5").unwrap(), 2.5);
    }

    #[test]
    fn parse_float_trims_whitespace() {
        assert_eq!(parse_float(" 2.5 \n").unwrap(), 2.5);
    }

    #[test]
    fn parse_float_rejects_non_numeric() {
        let e = parse_float("hello").unwrap_err();
        assert!(matches!(e, ScoreFailure::Parse(_)));
    }

    #[test]
    fn parse_float_rejects_nan() {
        let e = parse_float("NaN").unwrap_err();
        match e {
            ScoreFailure::Parse(m) => assert!(m.contains("non-finite"), "got: {m}"),
            _ => panic!("expected Parse"),
        }
    }

    #[test]
    fn parse_float_rejects_inf() {
        for s in ["inf", "-inf", "+Infinity", "infinity", "-Infinity"] {
            let e = parse_float(s).unwrap_err();
            match e {
                ScoreFailure::Parse(m) => assert!(m.contains("non-finite"), "{s}: got: {m}"),
                _ => panic!("{s}: expected Parse"),
            }
        }
    }

    #[test]
    fn parse_regex_ok() {
        assert_eq!(
            parse_regex("foo score=2.5 bar", "score=([0-9.]+)").unwrap(),
            2.5
        );
    }

    #[test]
    fn parse_regex_no_match_err() {
        let e = parse_regex("nothing here", "score=([0-9.]+)").unwrap_err();
        assert!(matches!(e, ScoreFailure::Parse(_)));
    }

    #[test]
    fn parse_regex_no_capture_group_err() {
        let e = parse_regex("score=2.5", "score=[0-9.]+").unwrap_err();
        match e {
            ScoreFailure::Parse(m) => assert!(m.contains("capture group"), "got: {m}"),
            _ => panic!("expected Parse"),
        }
    }

    #[test]
    fn parse_regex_capture_not_number_err() {
        let e = parse_regex("score=abc", "score=([a-z]+)").unwrap_err();
        match e {
            ScoreFailure::Parse(m) => assert!(m.contains("not a float"), "got: {m}"),
            _ => panic!("expected Parse"),
        }
    }

    #[test]
    fn parse_regex_rejects_nonfinite_capture() {
        let e = parse_regex("score=NaN", "score=(\\S+)").unwrap_err();
        match e {
            ScoreFailure::Parse(m) => assert!(m.contains("non-finite"), "got: {m}"),
            _ => panic!("expected Parse"),
        }
        let e = parse_regex("score=inf", "score=(\\S+)").unwrap_err();
        match e {
            ScoreFailure::Parse(m) => assert!(m.contains("non-finite"), "got: {m}"),
            _ => panic!("expected Parse"),
        }
    }

    #[test]
    fn parse_jq_ok() {
        assert_eq!(
            parse_jq(r#"{"metrics":{"bpb":1.5}}"#, ".metrics.bpb").unwrap(),
            1.5
        );
    }

    #[test]
    fn parse_jq_dollar_syntax_also_works() {
        assert_eq!(
            parse_jq(r#"{"metrics":{"bpb":1.5}}"#, "$.metrics.bpb").unwrap(),
            1.5
        );
    }

    #[test]
    fn parse_jq_missing_field_err() {
        let e = parse_jq(r#"{"metrics":{}}"#, ".metrics.bpb").unwrap_err();
        match e {
            ScoreFailure::Parse(m) => assert!(m.contains("matched 0"), "got: {m}"),
            _ => panic!("expected Parse"),
        }
    }

    #[test]
    fn parse_jq_not_a_number_err() {
        let e = parse_jq(r#"{"metrics":{"bpb":"hi"}}"#, ".metrics.bpb").unwrap_err();
        match e {
            ScoreFailure::Parse(m) => assert!(m.contains("not a number"), "got: {m}"),
            _ => panic!("expected Parse"),
        }
    }

    #[test]
    fn parse_jq_invalid_json_err() {
        let e = parse_jq("not json", ".metrics.bpb").unwrap_err();
        match e {
            ScoreFailure::Parse(m) => assert!(m.contains("invalid json"), "got: {m}"),
            _ => panic!("expected Parse"),
        }
    }

    #[test]
    fn score_runs_command_and_parses_float() {
        let dir = tempdir().unwrap();
        let mut o = obj(ParseSpec::Float, FailMode::Invalid, Direction::Min);
        o.command = "echo 2.5".to_string();
        let out = score(dir.path(), &o).unwrap();
        assert_eq!(out.score, Some(2.5));
        assert!(out.failure.is_none());
        assert_eq!(out.exit_code, Some(0));
        assert!(!out.timed_out);
    }

    #[test]
    fn score_reports_nonzero_exit_failure() {
        let dir = tempdir().unwrap();
        let mut o = obj(ParseSpec::Float, FailMode::Invalid, Direction::Min);
        o.command = "echo 1.5; exit 1".to_string();
        let out = score(dir.path(), &o).unwrap();
        assert_eq!(out.score, None);
        assert_eq!(out.exit_code, Some(1));
        assert_eq!(out.failure, Some(ScoreFailure::Exit(1)));
        assert!(!out.timed_out);
    }

    #[test]
    fn score_timeout_kills_and_reports() {
        let dir = tempdir().unwrap();
        let mut o = obj(ParseSpec::Float, FailMode::Invalid, Direction::Min);
        o.command = "sleep 5".to_string();
        o.timeout = Duration::from_millis(200);
        let started = Instant::now();
        let out = score(dir.path(), &o).unwrap();
        let elapsed = started.elapsed();
        assert!(out.timed_out, "expected timed_out, got {out:?}");
        assert_eq!(out.score, None);
        assert_eq!(out.failure, Some(ScoreFailure::Timeout));
        assert!(
            elapsed < Duration::from_secs(2),
            "took too long: {elapsed:?}"
        );
    }

    #[test]
    fn score_captures_large_stdout() {
        let dir = tempdir().unwrap();
        let mut o = obj(
            ParseSpec::Regex {
                pattern: r"score=([0-9.]+)".to_string(),
            },
            FailMode::Invalid,
            Direction::Min,
        );
        // ~256 KB of `x` characters, then a final score line. Far larger than the
        // 64 KB pipe buffer — proves the drain threads keep the child unblocked.
        o.command = "yes x | head -c 262144; echo; echo score=2.5".to_string();
        let out = score(dir.path(), &o).unwrap();
        assert_eq!(out.exit_code, Some(0));
        assert!(!out.timed_out);
        assert_eq!(out.score, Some(2.5));
        assert!(out.stdout.len() >= 262144);
    }

    fn out_with(score: Option<f64>, failure: Option<ScoreFailure>) -> ScoreOutput {
        ScoreOutput {
            score,
            failure,
            stdout: String::new(),
            stderr: String::new(),
            timed_out: false,
            exit_code: None,
        }
    }

    #[test]
    fn apply_fail_mode_passes_through_on_success() {
        let o_invalid = obj(ParseSpec::Float, FailMode::Invalid, Direction::Min);
        let o_worst = obj(ParseSpec::Float, FailMode::Worst, Direction::Max);
        let o_abort = obj(ParseSpec::Float, FailMode::Abort, Direction::Min);
        let so = out_with(Some(3.0), None);
        assert_eq!(apply_fail_mode(&so, &o_invalid), ScoreDecision::Use(3.0));
        assert_eq!(apply_fail_mode(&so, &o_worst), ScoreDecision::Use(3.0));
        assert_eq!(apply_fail_mode(&so, &o_abort), ScoreDecision::Use(3.0));
    }

    #[test]
    fn apply_fail_mode_invalid_discards() {
        let o = obj(ParseSpec::Float, FailMode::Invalid, Direction::Min);
        let so = out_with(None, Some(ScoreFailure::Timeout));
        assert_eq!(apply_fail_mode(&so, &o), ScoreDecision::Discard);
    }

    #[test]
    fn apply_fail_mode_worst_min_returns_f64_max() {
        let o = obj(ParseSpec::Float, FailMode::Worst, Direction::Min);
        let so = out_with(None, Some(ScoreFailure::Exit(1)));
        let d = apply_fail_mode(&so, &o);
        match d {
            ScoreDecision::Use(v) => {
                assert_eq!(v, f64::MAX, "got {v}");
                assert!(v.is_finite(), "must be finite to survive JSON: {v}");
            }
            _ => panic!("expected Use"),
        }
    }

    #[test]
    fn apply_fail_mode_worst_max_returns_f64_min() {
        let o = obj(ParseSpec::Float, FailMode::Worst, Direction::Max);
        let so = out_with(None, Some(ScoreFailure::Exit(1)));
        let d = apply_fail_mode(&so, &o);
        match d {
            ScoreDecision::Use(v) => {
                assert_eq!(v, f64::MIN, "got {v}");
                assert!(v.is_finite(), "must be finite to survive JSON: {v}");
            }
            _ => panic!("expected Use"),
        }
    }

    #[test]
    fn describe_failure_renders_each_variant() {
        assert_eq!(describe_failure(&ScoreFailure::Exit(3)), "exit code 3");
        assert_eq!(describe_failure(&ScoreFailure::Timeout), "timed out");
        assert_eq!(describe_failure(&ScoreFailure::Signal), "killed by signal");
        assert_eq!(
            describe_failure(&ScoreFailure::Spawn("boom".into())),
            "spawn: boom"
        );
        assert_eq!(
            describe_failure(&ScoreFailure::Parse("nope".into())),
            "parse: nope"
        );
    }

    #[test]
    fn apply_fail_mode_abort_aborts() {
        let o = obj(ParseSpec::Float, FailMode::Abort, Direction::Min);
        let so = out_with(None, Some(ScoreFailure::Parse("bad".into())));
        match apply_fail_mode(&so, &o) {
            ScoreDecision::Abort(r) => assert!(r.contains("parse"), "got {r}"),
            _ => panic!("expected Abort"),
        }
    }
}
