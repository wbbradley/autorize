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
