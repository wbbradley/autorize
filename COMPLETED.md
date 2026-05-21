# COMPLETED.md

## Phase 1 — Skeleton + `autorize init` (2026-05-20)

Stood up the `autorize` binary crate. `Cargo.toml` has the Phase-1 deps
(clap derive, serde + serde_json, toml, anyhow, thiserror, humantime + humantime-serde,
tracing + tracing-subscriber; tempfile as dev-dep). Added `src/main.rs`, `src/cli.rs` with
clap-derived subcommands, full `src/cli/init.rs`, plus `Phase 5`-deferred stubs for
`run`/`status`/`resume`. Added the full TOML schema in `src/config.rs` with serde structs,
`Config::from_toml`, and `validate()` (schedule XOR, name regex, prompt_file placeholder rule,
non-empty objective/agent command). Embedded `config.toml` + `program.md` templates with
`{{experiment_name}}` substitution in `src/templates.rs`. Added `src/experiment.rs` path
helpers and `src/error.rs` (`Error`/`Result` via thiserror). `.gitignore` covers Cargo + the
per-iter worktree dirs.

`autorize init pi` scaffolds `.autorize/pi/{config.toml,program.md}` with a round-trip parse as
a self-check; rerunning is rejected with `ExperimentExists`; bad names hit `InvalidName`. The
three runtime subcommands exit non-zero with "not yet implemented (Phase 5)" messages.

21 unit tests pass (config parse variants, schedule variants, validation failures, template
rendering, init success/refusal). `chk` is clean.

## Phase 2 — Scoring + Worktree (2026-05-20)

Added `src/scoring.rs` and `src/worktree.rs` as standalone leaf modules. Picked up
`regex`, `globset`, and `serde_json_path` via `cargo add`; extended `src/error.rs` with
`Git`/`Scoring` variants and `#[from]` conversions for regex / globset / serde_json.

`scoring::score` runs `bash -lc <objective.command>` inside a workdir, draining stdout/stderr
in background threads so even >256 KB outputs can't deadlock against the 64 KB pipe buffer,
polls `try_wait`, and kills the child via `child.kill()` on timeout (Phase 3 will upgrade
this to a process-group SIGTERM/SIGKILL). The captured output goes through `ParseSpec`
(float / regex with one capture group / JSONPath via `serde_json_path`, accepting jq-style
leading `.` by rewriting to `$.`). `apply_fail_mode` turns the resulting `Option<f64>` into a
`ScoreDecision::{Use, Discard, Abort}` per the configured `fail_mode` + `direction`
(`worst` returns `+inf` for min, `-inf` for max).

`worktree::Git` wraps shelled-out `git` calls through a small `run_git_raw`/`run_git` pair:
pre-flight (`is_inside_repo`, `is_clean`, `head_sha`, `resolve_ref`), branch ops
(`branch_exists`, `create_branch_at`), worktree lifecycle (`worktree_add`, `worktree_remove
--force`, `worktree_list` via `--porcelain` parsing), and in-worktree ops (`diff_against`,
`diff_paths_against`, `commit_all_in` which advances the tracking branch by committing on
the worktree's checked-out ref with an explicit `-c user.email=autorize@local
-c user.name=autorize` identity). `deny_path_matches` builds a `GlobSet` from
`boundaries.deny_paths` patterns and filters diff paths against it.

36 new tests bring the suite to 57 passing. Coverage includes all three parse variants and
their failure modes, a 200 ms timeout that finishes in under 2 s, a 256 KB-output pipe-drain
proof, every fail-mode/direction combination, and end-to-end git worktree
add/list/commit/remove against real `git` invocations in tempdirs. `chk` is clean.

## Phase 3 — Schedule + Agent (2026-05-20)

Added `chrono` (with `serde`) and `nix` (with `signal`, `process`) via `cargo add`. Three new
modules: `src/subproc.rs`, `src/agent.rs`, `src/schedule.rs`. Extended `src/error.rs` with a
new `Subproc(String)` variant and a `Schedule(String)` variant; removed the now-unused
`Scoring(String)` variant.

`subproc::run_command_with_budget` is the shared "spawn `bash -lc <cmd>` in a new session,
drain stdout/stderr in background threads, kill the whole process group on budget expiry"
primitive. It calls `setsid(2)` inside `Command::pre_exec` (async-signal-safe), so the
resulting pgid equals the bash pid; on timeout it sends `SIGTERM` via `killpg`, waits up to
5 s, then `SIGKILL`. The smoking test (`kills_grandchildren_via_pgroup`) spawns
`sleep 30 & echo $! >pidfile; wait`, then asserts the recorded grandchild pid no longer
exists (`kill(pid, None)` -> `ESRCH`) within 3 s of budget expiry — proving the pgroup kill
reaches grandchildren that a plain `child.kill()` would orphan.

`agent::run_agent` wraps the subproc primitive: substitutes `{prompt_file}`, `{workdir}`,
`{iter}` in `agent.command`; expands `$NAME`/`${NAME}` (one-pass byte scanner, with
`[A-Za-z_][A-Za-z0-9_]*` name rule, unset -> empty, literal `$` preserved when not followed
by a name char) in every `agent.env` value against the parent process env; injects
`AUTORIZE_WORKDIR` (or the user-overridden var name); and delivers the prompt either as a
file argument or piped to stdin per `agent.stdin`. Parent env passes through automatically
(`Command` doesn't `env_clear`), with `agent.env` overlaid.

`schedule::Deadline` is a thin `DateTime<Utc>` newtype with `at`/`is_expired`/`remaining`
helpers (saturates to zero past the deadline). `parse_deadline_expr` accepts three forms:
humantime durations (`"4h"`, `"30m"`), RFC3339 timestamps, and a tiny natural-language
grammar — `tomorrow`, `today`, `[tomorrow|today] <time>`, or bare `<time>` (rolls to the
next occurrence). The time parser handles `9am`, `9pm`, `9:30am`, `09:30` (24h), `14:30`,
`12am`/`12pm` (midnight/noon). `compute_deadline` picks `total_budget` if set, otherwise
parses `deadline`.

`scoring::run_with_timeout`'s body collapsed to a one-call delegate to
`subproc::run_command_with_budget` (signature unchanged); the spawn-failure arm in
`score()` now matches `Error::Subproc(_)`. All previous scoring tests keep passing, now
exercising the new pgroup-kill code path.

35 new tests (6 subproc + 11 agent + 18 schedule), suite now 92 passing total. `chk` clean.

## Phase 4 — Storage + Prompt + Iteration (2026-05-20)

Three new modules: `src/storage.rs`, `src/prompt.rs`, `src/iteration.rs` — the integration
phase that wires Phase 2/3 primitives into the one-iteration state machine + durable
on-disk record-keeping.

`storage` defines `Outcome` (lowercase serde-rename) and `CurrentStep` enums plus the
`IterationRecord` / `StateSnapshot` records, with a private `write_atomic(path, &[u8])`
helper that does the tmp-file + fsync + rename + best-effort directory fsync dance, and
cleans up the tmp file on any error along the way. `read_state` returns `Ok(None)` on
NotFound. `append_iteration` opens with `create(true).append(true)`, writes the JSON line
plus `\n`, then `sync_all()`. `read_iterations` splits by `\n`, drops blank lines, and if
the last non-empty line fails to parse it's treated as a torn-write tail and dropped
silently; any other corrupt line surfaces as `Error::Json`.

`prompt::build_prompt` renders deterministically: `program.md` (verbatim, trim_end'd), a
horizontal-rule separator, a Boundaries section (both allow_paths and deny_paths, with
`- (none)` when empty), a Recent iterations markdown table (`| {:>4} | {:<9} | {:>10} |`)
or `No prior iterations.` when empty, a Best iteration block with a `` ```diff `` fence
holding the verbatim best diff (or `No improvement merged yet.`), and a closing "This
iteration" stanza with iter number, budget (in seconds), and a min/max direction
explanation. Score formatting is `{:.5}` for finite values; `inf` / `-inf` for non-finite;
em-dash for `None`.

`iteration::run_iteration(&IterationInputs, &mut StateSnapshot)` runs one iteration end-to-
end: AllocateIter → CreateWorktree → RunSetup → BuildPrompt → InvokeAgent → CaptureDiff →
RunTeardown → Score → Decide → Merge/Discard → Cleanup → Record. Every transition
checkpoints `current_step` via an atomic `state.json` rewrite, so an external `kill -9`
between any two steps leaves a parseable file pointing at the last-completed phase.
Agent stdout/stderr are persisted next to the iter dir for debugging; the captured diff
also lands at `iter-NNNN/changes.diff`. Outcomes: empty diff → `Noop`; deny-path match →
`Denied` (scoring skipped); scoring abort → `Error::Config` propagated; scoring fail with
`fail_mode = invalid` → `Invalid`; score worse than best → `Discarded`; score better →
`Merged` with a `commit_all_in` advancing the tracking branch. The function returns the
appended `IterationRecord` and leaves `state.current_step = Idle`, with
`iterations_completed += 1`, noop counter reset (or incremented), and best updated.

To make deny enforcement see new files (agents that create paths under a deny pattern),
added `Git::stage_all_in` (`git add -A`) in `src/worktree.rs` and called it before
`diff_paths_against` / `diff_against` in iteration.rs — `git diff <branch>` alone skips
untracked content, which would let a new `forbidden/x.txt` slip past.

19 new tests: 8 storage (atomic overwrites + stray-tmp doesn't corrupt + missing-returns-None
+ JSON round-trip + 100-append + torn-last-line tolerance × 2 + corrupt-middle-line errors),
6 prompt (program-md verbatim + boundaries lists + no-history/no-best messages + history
table + best diff fence + full snapshot), and 5 iteration (merged on improvement against a
real git repo + awk-based `score.sh`, plus the noop/denied/discarded/invalid outcomes).
Suite is now 111 passing total. `chk` clean.

## Phase 5 — CLI `run` / `status` / `resume` + pre-flight (2026-05-20)

Wired Phases 1–4 into a working binary. `src/cli/run.rs` implements `run` and a
`pub(crate) run_loop` shared with resume. The loop does pre-flight (git repo
check, `--allow-dirty`-gated dirty-tree check that excludes `.autorize/` so
autorize's own state writes don't trip it, base_commit reachability,
in-progress refusal), then drives `iteration::run_iteration` until deadline,
`max_iterations`, or `max_consecutive_noops` fires. Fresh starts compute the
deadline via `schedule::compute_deadline`, create the `autorize/<name>` branch
at HEAD, and seed `state.json`; subsequent invocations read state and resume
from it. Per-iter status line prints outcome + score + best; final summary
prints experiment / iterations / best on exit.

`src/cli/resume.rs` delegates to `run_loop(.., recover_iter=true)`. When state
shows an in-progress iter, the new `record_killed` helper best-effort removes
the leftover worktree, appends an `outcome:"killed"` record with notes
`"resumed after crash"`, clears `iter_in_progress`, and bumps
`iterations_completed` — then the loop continues at iter+1. Resume errors out
cleanly when no state.json exists.

`src/cli/status.rs` reads state + jsonl and prints a one-shot summary
(experiment / branch / base_commit / iterations / noop streak / last outcome /
best (iter + score) / elapsed / remaining via `humantime::format_duration`, +
optional in-progress line). Errors with a hint when state.json is missing.

Support work: added `Git::is_clean_excluding(&[&str])` to `worktree.rs` (parses
porcelain-v1 output and ignores paths under given prefixes) plus 2 tests;
`ExperimentPaths::{load_config,load_program}` helpers; dropped `#[allow(dead_code)]`
on `storage::{read_state, read_iterations}` now that the run loop uses them.

14 new tests bring the suite to 125 passing: 7 run tests (refuses dirty,
allow-dirty flag, tolerates dirty `.autorize/`, refuses unreachable
base_commit, refuses in-progress without resume, fresh-run creates branch +
state, respects `max_iterations`), 2 resume tests (records killed +
continues, errors on missing state), 3 status tests (no iterations, best
formatted, missing state errors), 2 worktree clean-excluding tests. `chk`
clean.

## Phase 6 — Examples + e2e tests + polish (2026-05-20)

Shipped the runnable end-to-end demo and a binary-level integration suite that
proves every v1 acceptance criterion. v1 is now complete.

`examples/pi-digits/` contains the fixture (`value.txt` seeded at `3.0`,
`score.sh` printing `|π − value|` via `awk`, a `mock-agent.sh` that nudges
30% toward π each iter and deliberately regresses on `iter % 4 == 0` to
produce >=1 `discarded` outcome, a `bad-agent.sh` that writes
`.autorize/pi/state.json` for the deny-path test, plus `.autorize/pi/config.toml`
and `program.md`). The dir has no `*.rs` files so Cargo's `examples/`
auto-discovery ignores it.

`tests/e2e_pi.rs` drives the compiled `autorize` binary
(via `env!("CARGO_BIN_EXE_autorize")`) against the fixture copied into a
per-test `tempdir`:

- `loop_converges_with_merges_and_discards` — runs 6 iters, asserts >=3
  records, >=1 `merged` + >=1 `discarded`, strict iter 1..=N sequence,
  `best_score < 0.1`, and the `autorize/pi` branch tip's `value.txt` is
  closer to π than 3.0 (final value ~3.118, score ~0.024).
- `dirty_tree_refused_then_allow_dirty_succeeds` — `stray.txt` outside
  `.autorize/` causes `autorize run pi` to exit non-zero with `"uncommitted"`
  in stderr; `--allow-dirty` succeeds and produces records.
- `deny_path_violation_yields_denied_outcome` — patches config to point at
  `bad-agent.sh` and `max_iterations = 1`; asserts the only record is
  `outcome:"denied"` and `git rev-parse autorize/pi` still equals
  `state.json`'s `base_commit` (tracking branch did not advance).
- `resume_records_killed_then_continues` — hand-writes a `state.json` with
  `iter_in_progress=1, current_step="InvokeAgent"`, pre-creates the
  `autorize/pi` branch at HEAD, runs `autorize resume pi`; asserts iter 1 is
  recorded as `outcome:"killed", notes:"resumed after crash"`, followed by
  iters 2 and 3 as `merged`.

Adjusted the example's `agent.stdin = "prompt"` (config validation requires
`{prompt_file}` in the command when stdin is `"none"`); the mock agent
ignores stdin.

Polish: removed stale `#[allow(dead_code)]` attrs on
`iteration::{IterationInputs, run_iteration}`, `agent::{AgentSpec,
AgentOutput, run_agent}`, and `storage::{write_state, append_iteration}` —
all reachable from the run loop now. Dropped the genuinely-unused
`AgentOutput::signal` field (it was set but never read; `subproc::CommandOutput`
still carries it for tests).

4 new integration tests bring the suite to 125 unit + 4 e2e = 129 passing.
`chk` clean.

## `autorize llms` — agent-targeted docs subcommand (2026-05-20)

Added a 5th subcommand, `autorize llms`, that prints an exhaustive
plainly-formatted markdown reference aimed at LLM/agent consumers. The doc
covers what autorize is, the worktree-per-iter mechanism, the end-to-end
workflow, the iteration state machine with all 16 `CurrentStep` variants and
all 6 `Outcome` values, every `config.toml` field with type + default + validation
rule, all three `objective.parse` variants and all three `fail_mode` variants,
the `schedule` grammar (humantime / RFC3339 / natural language), `agent.command`
substitutions + `agent.env` `$VAR`/`${VAR}` expansion + `agent.stdin` modes, the
on-disk layout, the full `IterationRecord` and `StateSnapshot` schemas, the
`autorize run` pre-flight checks, and an inline `examples/pi-digits/`
walkthrough (config + program.md + sample `iterations.jsonl` + sample status
output + a simulated crash + resume).

`src/cli/llms.rs` follows the `init.rs` shape with a single `LLMS_MD: &str =
include_str!("../llms.md")` body and a no-op `LlmsArgs`. Wired into `src/cli.rs`
as `Command::Llms`. `src/llms.md` is the embedded body.

Drift guard: a unit test renders the default config template, parses the TOML,
recursively collects every key (including sub-table keys like `ANTHROPIC_API_KEY`
inside `[agent.env]`), and asserts each appears as a literal substring in the
embedded markdown. A new field in `Config` that's also added to the template
will fail this test until `llms.md` documents it. Four smaller smoke tests
cover non-emptiness + leading heading, all 6 outcome variants, all parse kinds
and fail-modes, all 5 subcommand names, and all 16 `CurrentStep` variants.

README gets a one-line entry in the Subcommands table pointing at
`autorize llms`. 6 new unit tests bring the suite to 137 unit + 4 e2e = 141
passing. `chk` clean.

## `fail_mode = "worst"` JSON round-trip + non-finite score rejection (2026-05-20)

Two coordinated fixes in `src/scoring.rs` that close one high-severity
data-integrity bug and one correctness bug together.

Replaced the `apply_fail_mode` "worst" sentinels: `f64::INFINITY` →
`f64::MAX` for `Direction::Min`, `f64::NEG_INFINITY` → `f64::MIN` for
`Direction::Max`. `serde_json` serializes non-finite f64 as JSON `null`, which
`Option<f64>` reads back as `None` — so the previous infinity sentinel was
silently lost across every `state.json` rewrite and every `iterations.jsonl`
record. Concretely: the first "worst" iter was recorded as `"merged"` with
`score: null`, and on the next iter `best_score = None` meant any subsequent
score trivially "improved", defeating the whole point of `fail_mode = "worst"`.
The finite sentinels round-trip cleanly and still satisfy `s < f64::MAX` (or
`s > f64::MIN`) for every plausible real score.

Added a `finite_or_parse_err(f64) -> Result<f64, ScoreFailure>` helper and
applied it in `parse_float`, `parse_regex`, and `parse_jq`, so user-produced
`NaN`/`±inf` from scorer stdout now becomes a `ScoreFailure::Parse("non-finite
score: ...")` at parse time. That routes through the configured `fail_mode`
exactly the way every other scoring failure does (the correct place to declare
"what happens when no score can be obtained"). It also fixes the second
PLAN.md item — a `NaN` first iter used to "merge" because `best = None`, and
thereafter every comparison against NaN was false, so every subsequent score
was discarded and the harness was locked.

Once non-finite values can't reach the formatter, the defensive `INFINITY` /
`NEG_INFINITY` / `is_nan()` branches in `format_score_cell` and
`format_score_inline` (`src/prompt.rs`) became dead code and were deleted per
"don't add fallbacks for scenarios that can't happen". Existing on-disk state
with `score: null` from the bug still loads fine — `Option<f64>` reads null as
`None`, no migration needed.

Updated `src/llms.md` section 6 to describe the finite-sentinel behavior
(no more `+inf`/`-inf` mention). Renamed two existing tests to
`apply_fail_mode_worst_min_returns_f64_max` / `..._max_returns_f64_min` and
added `parse_float_rejects_nan`, `parse_float_rejects_inf` (covers `inf`,
`-inf`, `+Infinity`, `infinity`, `-Infinity`),
`parse_regex_rejects_nonfinite_capture` (regex captures `NaN` and `inf`), and
`worst_sentinel_round_trips_through_json` in `src/storage.rs` (asserts the
serialized line contains no `null` and parses back to `Some(f64::MAX)` /
`Some(f64::MIN)`). `parse_jq` got a brief comment explaining why a non-finite
test there is impractical — `serde_json` rejects literal `NaN`/`Infinity` JSON
at parse time, so any number it accepts is already finite; the
`finite_or_parse_err` check stays for defense-in-depth.

6 new tests (135 unit + 4 e2e = 139 passing total). `chk` clean.
