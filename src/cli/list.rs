use std::{env, fmt::Write as _, io::IsTerminal, path::PathBuf};

use owo_colors::OwoColorize;

use crate::{
    error::Result,
    experiment::ExperimentPaths,
    storage::{self, IterationRecord, Outcome, StateSnapshot},
};

#[derive(clap::ValueEnum, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[value(rename_all = "lower")]
pub enum ColorChoice {
    /// Colorize only when stdout is a terminal (the default).
    #[default]
    Auto,
    /// Always emit ANSI styling, even when piped or redirected.
    Always,
    /// Never emit ANSI styling, even on a TTY.
    Never,
}

#[derive(clap::Args, Debug)]
/// Dump every iteration of an experiment as markdown, oldest-first, one
/// section per iteration carrying its model-written summary. Plain markdown
/// when piped or redirected (zero ANSI escapes); ANSI-styled on a TTY.
pub struct ListArgs {
    /// Experiment name (must exist under `.autorize/<name>/`).
    pub name: String,
    /// When to colorize output: `auto` (TTY-detect), `always`, or `never`.
    #[arg(long, value_enum, default_value_t = ColorChoice::Auto)]
    pub color: ColorChoice,
}

pub fn run(args: ListArgs) -> anyhow::Result<()> {
    let project_root = env::current_dir()?;
    run_with_root(args, project_root)?;
    Ok(())
}

fn run_with_root(args: ListArgs, project_root: PathBuf) -> Result<()> {
    let paths = ExperimentPaths::new(project_root, args.name.clone());
    // Unlike `status`, a missing state.json is not fatal: we can still list the
    // iterations; we just omit the "best" clause from the meta line.
    let state = storage::read_state(&paths.state_path())?;
    let records = storage::read_iterations(&paths.iterations_log())?;
    let colorize = match args.color {
        ColorChoice::Always => true,
        ColorChoice::Never => false,
        ColorChoice::Auto => std::io::stdout().is_terminal(),
    };
    let out = render(&args.name, &records, state.as_ref(), colorize);
    print!("{out}");
    Ok(())
}

/// Pure renderer: builds the markdown for `autorize list`. `colorize` gates
/// ANSI styling so tests can assert on plain output without a PTY, and `run`
/// decides it from `--color` + TTY detection.
fn render(
    name: &str,
    records: &[IterationRecord],
    state: Option<&StateSnapshot>,
    colorize: bool,
) -> String {
    let mut s = String::new();
    let title = format!("# Experiment: {name}");
    let _ = writeln!(s, "{}", maybe_bold(&title, colorize));

    if records.is_empty() {
        let _ = writeln!(s, "{}", maybe_dim("_No iterations yet._", colorize));
        return s;
    }

    let n = records.len();
    let meta = match best_clause(state) {
        Some(clause) => format!("_{n} iterations · {clause}_"),
        None => format!("_{n} iterations_"),
    };
    let _ = writeln!(s, "{}", maybe_dim(&meta, colorize));

    // Oldest-first: ascending iteration number.
    let mut ordered: Vec<&IterationRecord> = records.iter().collect();
    ordered.sort_by_key(|r| r.iter);
    for r in ordered {
        s.push('\n');
        let heading = maybe_bold(&format!("## Iteration {} \u{2014}", r.iter), colorize);
        let outcome = styled_outcome(r.outcome, colorize);
        let _ = writeln!(s, "{heading} {outcome} \u{b7} {}", format_score(r.score));
        if r.summary.is_empty() {
            let _ = writeln!(s, "{}", maybe_dim("_(no summary)_", colorize));
        } else {
            let _ = writeln!(s, "{}", r.summary);
        }
    }
    s
}

/// The `best <score> (iter <n>)` clause, sourced from `state.json` (which
/// already accounts for `objective.direction`). `None` when state is absent or
/// no best has been recorded yet.
fn best_clause(state: Option<&StateSnapshot>) -> Option<String> {
    let st = state?;
    match (st.best_iter, st.best_score) {
        (Some(i), Some(sc)) => Some(format!("best {} (iter {i})", format_score(Some(sc)))),
        _ => None,
    }
}

fn maybe_bold(text: &str, colorize: bool) -> String {
    if colorize {
        text.bold().to_string()
    } else {
        text.to_string()
    }
}

fn maybe_dim(text: &str, colorize: bool) -> String {
    if colorize {
        text.dimmed().to_string()
    } else {
        text.to_string()
    }
}

fn styled_outcome(o: Outcome, colorize: bool) -> String {
    let label = outcome_label(o);
    if !colorize {
        return label.to_string();
    }
    match o {
        Outcome::Merged => label.green().to_string(),
        Outcome::Discarded => label.yellow().to_string(),
        Outcome::Noop => label.dimmed().to_string(),
        Outcome::Invalid | Outcome::Killed | Outcome::Denied => label.red().to_string(),
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

/// Score convention shared with `autorize`'s prompt history table: em-dash for
/// `None`, else five decimals.
fn format_score(s: Option<f64>) -> String {
    match s {
        None => "\u{2014}".to_string(),
        Some(v) => format!("{v:.5}"),
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::*;
    use crate::storage::CurrentStep;

    fn rec(iter: u64, outcome: Outcome, score: Option<f64>, summary: &str) -> IterationRecord {
        IterationRecord {
            iter,
            started_at: Utc.with_ymd_and_hms(2026, 5, 20, 8, 0, 0).unwrap(),
            ended_at: Utc.with_ymd_and_hms(2026, 5, 20, 8, 1, 0).unwrap(),
            outcome,
            score,
            best_so_far: score,
            agent_exit: Some(0),
            agent_killed_by_budget: false,
            diff_lines: 4,
            notes: String::new(),
            summary: summary.to_string(),
        }
    }

    fn state_with_best(best_iter: Option<u64>, best_score: Option<f64>) -> StateSnapshot {
        let now = Utc::now();
        StateSnapshot {
            experiment: "pi".to_string(),
            branch: "autorize/pi".to_string(),
            base_commit: "abc1234".to_string(),
            iter_in_progress: None,
            current_step: CurrentStep::Idle,
            best_score,
            best_iter,
            started_at: now,
            deadline: now + chrono::Duration::seconds(3600),
            iterations_completed: 0,
            run_iterations_completed: 0,
            consecutive_noops: 0,
        }
    }

    #[test]
    fn render_zero_iterations() {
        let out = render("pi", &[], None, false);
        assert!(out.contains("# Experiment: pi"), "title missing: {out}");
        assert!(
            out.contains("_No iterations yet._"),
            "placeholder missing: {out}"
        );
        assert!(!out.contains("## Iteration"), "no sections expected: {out}");
    }

    #[test]
    fn render_zero_iterations_ignores_state() {
        // Even with a state carrying a best, zero records => the empty placeholder.
        let st = state_with_best(Some(5), Some(1.23456));
        let out = render("pi", &[], Some(&st), false);
        assert!(
            out.contains("_No iterations yet._"),
            "placeholder missing: {out}"
        );
        assert!(!out.contains("best"), "no meta line expected: {out}");
    }

    #[test]
    fn render_one_iteration_with_best() {
        let st = state_with_best(Some(1), Some(0.09920));
        let recs = vec![rec(
            1,
            Outcome::Merged,
            Some(0.09920),
            "Nudged value.txt toward pi.",
        )];
        let out = render("pi", &recs, Some(&st), false);
        assert!(out.contains("# Experiment: pi"), "title missing: {out}");
        assert!(
            out.contains("_1 iterations · best 0.09920 (iter 1)_"),
            "meta line wrong: {out}"
        );
        assert!(
            out.contains("## Iteration 1 \u{2014} merged \u{b7} 0.09920"),
            "section heading wrong: {out}"
        );
        assert!(
            out.contains("Nudged value.txt toward pi."),
            "summary missing: {out}"
        );
    }

    #[test]
    fn render_meta_omits_best_when_state_absent() {
        let recs = vec![rec(1, Outcome::Merged, Some(0.5), "did a thing")];
        let out = render("pi", &recs, None, false);
        assert!(out.contains("_1 iterations_"), "meta line wrong: {out}");
        assert!(!out.contains("best"), "best clause should be absent: {out}");
    }

    #[test]
    fn render_several_oldest_first_with_outcomes_and_scores() {
        let st = state_with_best(Some(3), Some(std::f64::consts::PI));
        // Deliberately out of order on input to prove the renderer sorts.
        let recs = vec![
            rec(3, Outcome::Merged, Some(std::f64::consts::PI), "best"),
            rec(1, Outcome::Merged, Some(3.20000), "first"),
            rec(4, Outcome::Noop, None, "no diff"),
            rec(2, Outcome::Discarded, Some(3.50000), "regressed"),
        ];
        let out = render("pi", &recs, Some(&st), false);
        // Oldest-first ordering: iter 1 must appear before iter 4.
        let p4 = out.find("## Iteration 4").expect("iter 4 missing");
        let p1 = out.find("## Iteration 1").expect("iter 1 missing");
        assert!(p1 < p4, "expected oldest-first ordering: {out}");
        // Outcome labels and 5-decimal scores.
        assert!(
            out.contains("## Iteration 3 \u{2014} merged \u{b7} 3.14159"),
            "{out}"
        );
        assert!(
            out.contains("## Iteration 2 \u{2014} discarded \u{b7} 3.50000"),
            "{out}"
        );
        // None score renders as the em-dash.
        assert!(
            out.contains("## Iteration 4 \u{2014} noop \u{b7} \u{2014}"),
            "{out}"
        );
    }

    #[test]
    fn render_empty_summary_placeholder() {
        let recs = vec![rec(1, Outcome::Merged, Some(1.0), "")];
        let out = render("pi", &recs, None, false);
        assert!(
            out.contains("_(no summary)_"),
            "summary placeholder missing: {out}"
        );
    }

    #[test]
    fn render_plain_has_no_ansi_escapes() {
        let st = state_with_best(Some(2), Some(1.0));
        let recs = vec![
            rec(1, Outcome::Merged, Some(2.0), "a"),
            rec(2, Outcome::Denied, Some(1.0), ""),
        ];
        let out = render("pi", &recs, Some(&st), false);
        assert!(
            !out.contains('\u{1b}'),
            "plain output must have no ANSI escapes: {out:?}"
        );
    }

    #[test]
    fn render_colorized_has_ansi_escapes() {
        let st = state_with_best(Some(1), Some(2.0));
        let recs = vec![rec(1, Outcome::Merged, Some(2.0), "a")];
        let out = render("pi", &recs, Some(&st), true);
        assert!(
            out.contains('\u{1b}'),
            "colorized output must contain ANSI escapes"
        );
        // The underlying markdown text still survives the styling.
        assert!(
            out.contains("Experiment: pi"),
            "title text missing: {out:?}"
        );
        assert!(out.contains("merged"), "outcome text missing: {out:?}");
    }
}
