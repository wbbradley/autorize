use std::{fmt::Write, time::Duration};

use crate::{
    config::{Boundaries, Direction},
    storage::{IterationRecord, Outcome},
};

pub struct BestSnapshot<'a> {
    pub iter: u64,
    pub score: f64,
    pub diff: &'a str,
}

pub struct PromptContext<'a> {
    pub program_md: &'a str,
    pub boundaries: &'a Boundaries,
    pub recent: &'a [IterationRecord],
    pub best: Option<BestSnapshot<'a>>,
    pub iter: u64,
    pub budget: Duration,
    pub direction: Direction,
}

#[allow(dead_code)] // wired in by Phase 4 iteration
pub fn build_prompt(ctx: &PromptContext) -> String {
    let mut s = String::new();

    s.push_str(ctx.program_md.trim_end());
    s.push_str("\n\n---\n\n");

    if !ctx.boundaries.allow_paths.is_empty() || !ctx.boundaries.deny_paths.is_empty() {
        s.push_str("## Boundaries\n\n");
        if !ctx.boundaries.allow_paths.is_empty() {
            s.push_str("You should focus your edits on these paths (PROMPT-ONLY, not enforced):\n");
            push_path_list(&mut s, &ctx.boundaries.allow_paths);
            s.push('\n');
        }
        if !ctx.boundaries.deny_paths.is_empty() {
            s.push_str(
            "You MUST NOT modify these paths (ENFORCED \u{2014} touching them discards the iteration):\n",
        );
            push_path_list(&mut s, &ctx.boundaries.deny_paths);
        }
    }

    s.push_str("\n## Recent iterations\n\n");
    if ctx.recent.is_empty() {
        s.push_str("No prior iterations.\n");
    } else {
        push_history_table(&mut s, ctx.recent);
    }

    s.push_str("\n## Best iteration so far\n\n");
    match &ctx.best {
        None => {
            s.push_str("No improvement merged yet.\n");
        }
        Some(b) => {
            let _ = writeln!(
                s,
                "iter {}, score {} (direction: {}).",
                b.iter,
                format_score_inline(b.score),
                direction_label(ctx.direction),
            );
            s.push_str("\nDiff:\n\n```diff\n");
            s.push_str(b.diff);
            if !b.diff.ends_with('\n') {
                s.push('\n');
            }
            s.push_str("```\n");
        }
    }

    s.push_str("\n## This iteration\n\n");
    let _ = writeln!(
        s,
        "You are working on iteration {}. Hard wall-clock budget: {}s.",
        ctx.iter,
        ctx.budget.as_secs(),
    );
    let _ = writeln!(
        s,
        "The objective direction is `{}` \u{2014} {}.",
        direction_label(ctx.direction),
        direction_explanation(ctx.direction),
    );

    s
}

fn push_path_list(s: &mut String, paths: &[String]) {
    if paths.is_empty() {
        s.push_str("- (none)\n");
        return;
    }
    for p in paths {
        let _ = writeln!(s, "- {p}");
    }
}

fn push_history_table(s: &mut String, recent: &[IterationRecord]) {
    s.push_str("| iter | outcome   | score      |\n");
    s.push_str("|------|-----------|------------|\n");
    for r in recent {
        let _ = writeln!(
            s,
            "| {:>4} | {:<9} | {:>10} |",
            r.iter,
            outcome_label(r.outcome),
            format_score_cell(r.score),
        );
    }
}

fn outcome_label(o: Outcome) -> &'static str {
    match o {
        Outcome::Merged => "merged",
        Outcome::Discarded => "discarded",
        Outcome::Noop => "noop",
        Outcome::Invalid => "invalid",
        Outcome::Killed => "killed",
        Outcome::Denied => "denied",
    }
}

fn direction_label(d: Direction) -> &'static str {
    match d {
        Direction::Min => "min",
        Direction::Max => "max",
    }
}

fn direction_explanation(d: Direction) -> &'static str {
    match d {
        Direction::Min => "lower scores are better",
        Direction::Max => "higher scores are better",
    }
}

fn format_score_cell(s: Option<f64>) -> String {
    match s {
        None => "\u{2014}".to_string(),
        Some(v) => format!("{v:.5}"),
    }
}

fn format_score_inline(v: f64) -> String {
    format!("{v:.5}")
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::*;

    fn rec(iter: u64, outcome: Outcome, score: Option<f64>) -> IterationRecord {
        IterationRecord {
            iter,
            started_at: Utc.with_ymd_and_hms(2026, 5, 20, 8, 0, 0).unwrap(),
            ended_at: Utc.with_ymd_and_hms(2026, 5, 20, 8, 1, 0).unwrap(),
            outcome,
            score,
            best_so_far: score,
            agent_exit: Some(0),
            agent_killed_by_budget: false,
            diff_lines: 1,
            notes: String::new(),
        }
    }

    fn full_boundaries() -> Boundaries {
        Boundaries {
            allow_paths: vec!["src/**/*".to_string(), "README.md".to_string()],
            deny_paths: vec![".autorize/**".to_string(), "*.lock".to_string()],
        }
    }

    #[test]
    fn prompt_renders_program_md_verbatim() {
        let b = Boundaries::default();
        let ctx = PromptContext {
            program_md: "# My program\n\nDo something useful.\n",
            boundaries: &b,
            recent: &[],
            best: None,
            iter: 1,
            budget: Duration::from_secs(60),
            direction: Direction::Min,
        };
        let p = build_prompt(&ctx);
        assert!(p.contains("# My program"), "program text missing: {p}");
        assert!(p.contains("Do something useful."), "body missing: {p}");
    }

    #[test]
    fn prompt_renders_boundaries_lists() {
        let b = full_boundaries();
        let ctx = PromptContext {
            program_md: "",
            boundaries: &b,
            recent: &[],
            best: None,
            iter: 1,
            budget: Duration::from_secs(60),
            direction: Direction::Min,
        };
        let p = build_prompt(&ctx);
        assert!(p.contains("- src/**/*"), "allow path missing: {p}");
        assert!(p.contains("- README.md"), "allow path missing: {p}");
        assert!(p.contains("- .autorize/**"), "deny path missing: {p}");
        assert!(p.contains("- *.lock"), "deny path missing: {p}");
    }

    #[test]
    fn prompt_no_history_no_best_message() {
        let b = Boundaries::default();
        let ctx = PromptContext {
            program_md: "p",
            boundaries: &b,
            recent: &[],
            best: None,
            iter: 1,
            budget: Duration::from_secs(60),
            direction: Direction::Min,
        };
        let p = build_prompt(&ctx);
        assert!(p.contains("No prior iterations."), "missing: {p}");
        assert!(p.contains("No improvement merged yet."), "missing: {p}");
    }

    #[test]
    fn prompt_with_history_table() {
        let b = Boundaries::default();
        let hist = vec![
            rec(7, Outcome::Merged, Some(3.14210)),
            rec(6, Outcome::Discarded, Some(3.15000)),
            rec(5, Outcome::Noop, None),
        ];
        let ctx = PromptContext {
            program_md: "p",
            boundaries: &b,
            recent: &hist,
            best: None,
            iter: 8,
            budget: Duration::from_secs(300),
            direction: Direction::Min,
        };
        let p = build_prompt(&ctx);
        assert!(
            p.contains("| iter | outcome   | score      |"),
            "header missing: {p}"
        );
        assert!(
            p.contains("|------|-----------|------------|"),
            "separator missing: {p}"
        );
        assert!(
            p.contains("|    7 | merged    |    3.14210 |"),
            "row 7 missing: {p}"
        );
        assert!(
            p.contains("|    6 | discarded |    3.15000 |"),
            "row 6 missing: {p}"
        );
        assert!(
            p.contains("|    5 | noop      |          \u{2014} |"),
            "row 5 missing: {p}"
        );
    }

    #[test]
    fn prompt_with_best_diff_block() {
        let b = Boundaries::default();
        let diff = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\n+new\n";
        let best = BestSnapshot {
            iter: 5,
            score: std::f64::consts::PI,
            diff,
        };
        let ctx = PromptContext {
            program_md: "p",
            boundaries: &b,
            recent: &[],
            best: Some(best),
            iter: 8,
            budget: Duration::from_secs(60),
            direction: Direction::Min,
        };
        let p = build_prompt(&ctx);
        assert!(p.contains("```diff\n"), "no opening fence: {p}");
        assert!(p.contains(diff), "diff body missing: {p}");
        assert!(p.contains("\n```\n"), "no closing fence: {p}");
        assert!(
            p.contains("iter 5, score 3.14159 (direction: min)."),
            "best line missing: {p}"
        );
    }

    #[test]
    fn prompt_snapshot() {
        let b = full_boundaries();
        let hist = vec![
            rec(7, Outcome::Merged, Some(3.14210)),
            rec(6, Outcome::Discarded, Some(3.15000)),
        ];
        let diff = "diff --git a/value.txt b/value.txt\n--- a/value.txt\n+++ b/value.txt\n@@ -1 +1 @@\n-3.10\n+3.14\n";
        let best = BestSnapshot {
            iter: 5,
            score: std::f64::consts::PI,
            diff,
        };
        let ctx = PromptContext {
            program_md: "# Pi experiment\n\nMake value.txt closer to pi.\n",
            boundaries: &b,
            recent: &hist,
            best: Some(best),
            iter: 8,
            budget: Duration::from_secs(300),
            direction: Direction::Min,
        };
        let got = build_prompt(&ctx);
        let expected = "\
# Pi experiment

Make value.txt closer to pi.

---

## Boundaries

You should focus your edits on these paths (PROMPT-ONLY, not enforced):
- src/**/*
- README.md

You MUST NOT modify these paths (ENFORCED \u{2014} touching them discards the iteration):
- .autorize/**
- *.lock

## Recent iterations

| iter | outcome   | score      |
|------|-----------|------------|
|    7 | merged    |    3.14210 |
|    6 | discarded |    3.15000 |

## Best iteration so far

iter 5, score 3.14159 (direction: min).

Diff:

```diff
diff --git a/value.txt b/value.txt
--- a/value.txt
+++ b/value.txt
@@ -1 +1 @@
-3.10
+3.14
```

## This iteration

You are working on iteration 8. Hard wall-clock budget: 300s.
The objective direction is `min` \u{2014} lower scores are better.
";
        assert_eq!(got, expected, "got:\n{got}");
    }
}
