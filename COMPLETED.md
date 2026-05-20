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
