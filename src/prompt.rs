use std::{fmt::Write, time::Duration};

use crate::{
    config::{Boundaries, Direction},
    storage::{GuidanceEntry, IterationRecord, Outcome},
};

pub struct BestSnapshot<'a> {
    pub iter: u64,
    pub score: f64,
    pub diff: &'a str,
}

pub struct PromptContext<'a> {
    pub program_md: &'a str,
    pub boundaries: &'a Boundaries,
    /// Operator guidance entries (from `guidance.jsonl`), most-recently-added
    /// last. Rendered as a prominent, authoritative section; empty → omitted.
    pub guidance: &'a [GuidanceEntry],
    pub recent: &'a [IterationRecord],
    pub best: Option<BestSnapshot<'a>>,
    pub iter: u64,
    pub budget: Duration,
    pub direction: Direction,
}

/// Inputs to [`build_summary_prompt`] — the artifacts of a single iteration,
/// assembled into the prompt handed to the (separate, typically weaker)
/// summarization model after the worker agent exits.
pub struct SummaryContext<'a> {
    pub iter: u64,
    pub outcome: Outcome,
    pub score: Option<f64>,
    pub best: Option<(f64, u64)>,
    pub direction: Direction,
    pub diff: &'a str,
    pub stdout_tail: &'a str,
    pub stderr_tail: &'a str,
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

    // Operator guidance is the steering channel an operator drives mid-run via
    // `autorize tell` (or by hand-editing guidance.jsonl). Place it high and
    // frame it as authoritative so it outranks the general program guidance.
    if !ctx.guidance.is_empty() {
        s.push_str("\n## Operator guidance\n\n");
        s.push_str(
            "The operator has issued the following direction for this run. Treat it as \
             authoritative \u{2014} it takes precedence over the general instructions above \
             where they conflict, and should shape what you attempt this iteration:\n\n",
        );
        for g in ctx.guidance {
            match g.added_at_iter {
                Some(i) => {
                    let _ = writeln!(s, "- (since iter {i}) {}", g.text);
                }
                None => {
                    let _ = writeln!(s, "- {}", g.text);
                }
            }
        }
    }

    s.push_str("\n## Recent iterations\n\n");
    if ctx.recent.is_empty() {
        s.push_str("No prior iterations.\n");
    } else {
        push_history_table(&mut s, ctx.recent);
    }

    // Multi-sentence model-written summaries don't fit in the table cell above,
    // so they get their own list. Only rendered when at least one recent record
    // carries a non-empty `summary` (i.e. the `[summarize]` step produced one).
    let summarized: Vec<&IterationRecord> = ctx
        .recent
        .iter()
        .filter(|r| !r.summary.is_empty())
        .collect();
    if !summarized.is_empty() {
        s.push_str("\n## Recent attempt summaries\n\n");
        for r in summarized {
            let _ = writeln!(
                s,
                "- iter {} ({}): {}",
                r.iter,
                outcome_label(r.outcome),
                r.summary,
            );
        }
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

    s.push_str(
        "\nJust edit files in the working tree \u{2014} do NOT run `git add`, `git commit`, \
         or otherwise create commits yourself. autorize captures your uncommitted changes \
         and commits them on your behalf.\n",
    );

    s
}

/// Maximum diff / stdio lines fed to the summarizer. Bounds the prompt so a
/// cheap model isn't blown out by a huge diff or a chatty agent; stdio is
/// tailed (the tail holds the agent's final reasoning), the diff is headed
/// (the leading hunks identify what was changed).
const SUMMARY_MAX_DIFF_LINES: usize = 600;
const SUMMARY_MAX_STDIO_LINES: usize = 80;

/// Build the prompt for the post-iteration summarization model. Self-contained:
/// it carries only this iteration's own artifacts (outcome, score, the diff,
/// and tails of the agent's stdout/stderr) — deliberately *not* the program
/// guidance or prior summaries, so it can run cheaply.
pub fn build_summary_prompt(ctx: &SummaryContext) -> String {
    let mut s = String::new();
    s.push_str(
        "You are summarizing one iteration of an automated code-improvement run. \
         In 1-2 sentences, state what this iteration attempted and why it moved \
         the score the way it did. Be concrete and specific about the change. \
         Output only the summary text \u{2014} no preamble, no markdown headers.\n\n",
    );

    let _ = writeln!(s, "## Outcome");
    let _ = writeln!(s, "- iteration: {}", ctx.iter);
    let _ = writeln!(s, "- outcome: {}", outcome_label(ctx.outcome));
    let _ = writeln!(
        s,
        "- score: {} (objective direction: {} \u{2014} {})",
        match ctx.score {
            Some(v) => format_score_inline(v),
            None => "none".to_string(),
        },
        direction_label(ctx.direction),
        direction_explanation(ctx.direction),
    );
    match ctx.best {
        Some((bs, bi)) => {
            let _ = writeln!(
                s,
                "- best so far: iter {bi}, score {}",
                format_score_inline(bs)
            );
        }
        None => {
            let _ = writeln!(s, "- best so far: none");
        }
    }

    s.push_str("\n## Diff\n\n```diff\n");
    s.push_str(&head_lines(ctx.diff, SUMMARY_MAX_DIFF_LINES));
    if !ctx.diff.is_empty() && !ctx.diff.ends_with('\n') {
        s.push('\n');
    }
    s.push_str("```\n");

    let stdout_tail = tail_lines(ctx.stdout_tail, SUMMARY_MAX_STDIO_LINES);
    if !stdout_tail.trim().is_empty() {
        s.push_str("\n## Agent stdout (tail)\n\n```\n");
        s.push_str(&stdout_tail);
        if !stdout_tail.ends_with('\n') {
            s.push('\n');
        }
        s.push_str("```\n");
    }

    let stderr_tail = tail_lines(ctx.stderr_tail, SUMMARY_MAX_STDIO_LINES);
    if !stderr_tail.trim().is_empty() {
        s.push_str("\n## Agent stderr (tail)\n\n```\n");
        s.push_str(&stderr_tail);
        if !stderr_tail.ends_with('\n') {
            s.push('\n');
        }
        s.push_str("```\n");
    }

    s
}

/// First `n` lines of `s` (used for the diff: leading hunks identify the
/// change). Returns the whole string when it has `n` lines or fewer.
fn head_lines(s: &str, n: usize) -> String {
    let mut out = String::new();
    for (i, line) in s.lines().enumerate() {
        if i >= n {
            let _ = writeln!(out, "... ({} more lines truncated)", s.lines().count() - n);
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Last `n` lines of `s` (used for stdio: the tail holds the agent's final
/// output). Returns the whole string when it has `n` lines or fewer.
fn tail_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() <= n {
        return s.to_string();
    }
    let mut out = format!("... ({} earlier lines truncated)\n", lines.len() - n);
    for line in &lines[lines.len() - n..] {
        out.push_str(line);
        out.push('\n');
    }
    out
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
    s.push_str("| iter | outcome   | score      | reason |\n");
    s.push_str("|------|-----------|------------|--------|\n");
    for r in recent {
        let _ = writeln!(
            s,
            "| {:>4} | {:<9} | {:>10} | {} |",
            r.iter,
            outcome_label(r.outcome),
            format_score_cell(r.score),
            r.notes,
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

    fn rec(iter: u64, outcome: Outcome, score: Option<f64>, notes: &str) -> IterationRecord {
        rec_with_summary(iter, outcome, score, notes, "")
    }

    fn rec_with_summary(
        iter: u64,
        outcome: Outcome,
        score: Option<f64>,
        notes: &str,
        summary: &str,
    ) -> IterationRecord {
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
            notes: notes.to_string(),
            summary: summary.to_string(),
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
            guidance: &[],
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
            guidance: &[],
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
            guidance: &[],
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
            rec(
                7,
                Outcome::Merged,
                Some(3.14210),
                "improved: 3.14210 from 3.15000",
            ),
            rec(
                6,
                Outcome::Discarded,
                Some(3.15000),
                "regressed: 3.15000 vs best 3.14210 (min)",
            ),
            rec(5, Outcome::Noop, None, "no changes produced"),
        ];
        let ctx = PromptContext {
            program_md: "p",
            boundaries: &b,
            guidance: &[],
            recent: &hist,
            best: None,
            iter: 8,
            budget: Duration::from_secs(300),
            direction: Direction::Min,
        };
        let p = build_prompt(&ctx);
        assert!(
            p.contains("| iter | outcome   | score      | reason |"),
            "header missing: {p}"
        );
        assert!(
            p.contains("|------|-----------|------------|--------|"),
            "separator missing: {p}"
        );
        assert!(
            p.contains("|    7 | merged    |    3.14210 | improved: 3.14210 from 3.15000 |"),
            "row 7 missing: {p}"
        );
        assert!(
            p.contains(
                "|    6 | discarded |    3.15000 | regressed: 3.15000 vs best 3.14210 (min) |"
            ),
            "row 6 missing: {p}"
        );
        assert!(
            p.contains("|    5 | noop      |          \u{2014} | no changes produced |"),
            "row 5 missing: {p}"
        );
    }

    #[test]
    fn prompt_renders_recent_summaries_section() {
        let b = Boundaries::default();
        let hist = vec![
            rec_with_summary(
                6,
                Outcome::Discarded,
                Some(3.15),
                "regressed: ...",
                "Tuned the Leibniz series term count up; slower convergence regressed the score.",
            ),
            // A record with no summary must not appear in the list.
            rec(5, Outcome::Noop, None, "no changes produced"),
            rec_with_summary(
                7,
                Outcome::Merged,
                Some(std::f64::consts::PI),
                "improved: ...",
                "Switched to a spigot algorithm, improving the digit accuracy.",
            ),
        ];
        let ctx = PromptContext {
            program_md: "p",
            boundaries: &b,
            guidance: &[],
            recent: &hist,
            best: None,
            iter: 8,
            budget: Duration::from_secs(60),
            direction: Direction::Min,
        };
        let p = build_prompt(&ctx);
        assert!(
            p.contains("## Recent attempt summaries"),
            "summaries section missing: {p}"
        );
        assert!(
            p.contains("- iter 6 (discarded): Tuned the Leibniz series term count up;"),
            "iter 6 summary missing: {p}"
        );
        assert!(
            p.contains("- iter 7 (merged): Switched to a spigot algorithm,"),
            "iter 7 summary missing: {p}"
        );
        // The summary-less noop iteration must not appear in the list.
        assert!(
            !p.contains("- iter 5 "),
            "iter 5 (no summary) should be omitted: {p}"
        );
    }

    #[test]
    fn prompt_omits_summaries_section_when_all_empty() {
        let b = Boundaries::default();
        let hist = vec![rec(
            7,
            Outcome::Merged,
            Some(std::f64::consts::PI),
            "improved: ...",
        )];
        let ctx = PromptContext {
            program_md: "p",
            boundaries: &b,
            guidance: &[],
            recent: &hist,
            best: None,
            iter: 8,
            budget: Duration::from_secs(60),
            direction: Direction::Min,
        };
        let p = build_prompt(&ctx);
        assert!(
            !p.contains("## Recent attempt summaries"),
            "summaries section should be omitted when no summaries: {p}"
        );
    }

    fn guide(text: &str, at: Option<u64>) -> GuidanceEntry {
        GuidanceEntry {
            ts: Utc.with_ymd_and_hms(2026, 5, 20, 8, 0, 0).unwrap(),
            added_at_iter: at,
            text: text.to_string(),
        }
    }

    #[test]
    fn prompt_renders_operator_guidance() {
        let b = Boundaries::default();
        let guidance = vec![
            guide(
                "try a spigot algorithm instead of tuning the series",
                Some(6),
            ),
            guide("keep the file a single line", None),
        ];
        let ctx = PromptContext {
            program_md: "p",
            boundaries: &b,
            guidance: &guidance,
            recent: &[],
            best: None,
            iter: 8,
            budget: Duration::from_secs(60),
            direction: Direction::Min,
        };
        let p = build_prompt(&ctx);
        assert!(p.contains("## Operator guidance"), "section missing: {p}");
        assert!(p.contains("authoritative"), "framing missing: {p}");
        assert!(
            p.contains("- (since iter 6) try a spigot algorithm instead of tuning the series"),
            "iter-tagged entry missing: {p}"
        );
        // A None added_at_iter renders without the "(since iter ...)" prefix.
        assert!(
            p.contains("- keep the file a single line"),
            "untagged entry missing: {p}"
        );
        assert!(
            !p.contains("(since iter ) keep"),
            "None entry should not render an empty iter tag: {p}"
        );
    }

    #[test]
    fn prompt_omits_operator_guidance_when_empty() {
        let b = Boundaries::default();
        let ctx = PromptContext {
            program_md: "p",
            boundaries: &b,
            guidance: &[],
            recent: &[],
            best: None,
            iter: 1,
            budget: Duration::from_secs(60),
            direction: Direction::Min,
        };
        let p = build_prompt(&ctx);
        assert!(
            !p.contains("## Operator guidance"),
            "guidance section should be omitted when empty: {p}"
        );
    }

    #[test]
    fn summary_prompt_includes_outcome_diff_and_stdio() {
        let ctx = SummaryContext {
            iter: 7,
            outcome: Outcome::Merged,
            score: Some(std::f64::consts::PI),
            best: Some((3.15, 6)),
            direction: Direction::Min,
            diff: "diff --git a/x b/x\n-old\n+new\n",
            stdout_tail: "agent did a thing\n",
            stderr_tail: "",
        };
        let p = build_summary_prompt(&ctx);
        assert!(p.contains("1-2 sentences"), "instruction missing: {p}");
        assert!(p.contains("outcome: merged"), "outcome missing: {p}");
        assert!(p.contains("score: 3.14159"), "score missing: {p}");
        assert!(
            p.contains("best so far: iter 6, score 3.15000"),
            "best missing: {p}"
        );
        assert!(p.contains("```diff\n"), "diff fence missing: {p}");
        assert!(p.contains("+new"), "diff body missing: {p}");
        assert!(p.contains("agent did a thing"), "stdout tail missing: {p}");
        // Empty stderr renders no stderr section.
        assert!(
            !p.contains("## Agent stderr"),
            "stderr section should be omitted: {p}"
        );
    }

    #[test]
    fn summary_prompt_tails_long_stdout() {
        let stdout: String = (0..200).map(|i| format!("line {i}\n")).collect();
        let ctx = SummaryContext {
            iter: 1,
            outcome: Outcome::Discarded,
            score: Some(1.0),
            best: None,
            direction: Direction::Max,
            diff: "diff\n",
            stdout_tail: &stdout,
            stderr_tail: "",
        };
        let p = build_summary_prompt(&ctx);
        assert!(
            p.contains("earlier lines truncated"),
            "tail marker missing: {p}"
        );
        assert!(
            p.contains("line 199"),
            "last line should survive the tail: {p}"
        );
        assert!(
            !p.contains("line 0\n"),
            "first line should be truncated: {p}"
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
            guidance: &[],
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
            rec(
                7,
                Outcome::Merged,
                Some(3.14210),
                "improved: 3.14210 from 3.15000",
            ),
            rec(
                6,
                Outcome::Discarded,
                Some(3.15000),
                "regressed: 3.15000 vs best 3.14210 (min)",
            ),
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
            guidance: &[],
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

| iter | outcome   | score      | reason |
|------|-----------|------------|--------|
|    7 | merged    |    3.14210 | improved: 3.14210 from 3.15000 |
|    6 | discarded |    3.15000 | regressed: 3.15000 vs best 3.14210 (min) |

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

Just edit files in the working tree \u{2014} do NOT run `git add`, `git commit`, or otherwise create commits yourself. autorize captures your uncommitted changes and commits them on your behalf.
";
        assert_eq!(got, expected, "got:\n{got}");
    }
}
