# Changelog

## [0.2.15] - 2026-05-24

### Changed

- `autorize list <name>` now lists iterations **oldest-first** (ascending iteration number) instead of newest-first, so reading top-to-bottom follows the experiment's chronological progression.

## [0.2.14] - 2026-05-24

### Added

- New `autorize list <name>` subcommand: a read-only sibling to `autorize status` that dumps every iteration as markdown, newest-first, with one `##` section per iteration carrying its model-written `summary` (or a `_(no summary)_` placeholder). A title line and a dimmed meta line (`_<N> iterations · best <score> (iter <n>)_`, sourced from `state.json`) head the output. When stdout is a real terminal the headings, meta line, and per-iteration outcome are ANSI-styled (merged→green, discarded→yellow, noop→dim, invalid/killed/denied→red) via `owo-colors`; when piped or redirected the output is clean markdown with zero ANSI escapes, suitable for writing to a `.md` file. `--color <auto|always|never>` (default `auto`) overrides the TTY detection. A missing `state.json` is not fatal (the best clause is simply omitted), and a zero-iteration experiment prints `_No iterations yet._`.

## [0.2.13] - 2026-05-24

### Added

- `autorize backfill <name> --force` regenerates summaries for *every* eligible record, overwriting existing ones, instead of only filling records that are missing a summary. Use it to re-summarize a whole experiment after changing `summarize.command` (e.g. to replace chatty summaries written by the old default). `noop`/`killed` records and records whose `iter-NNNN/` artifacts are gone are still skipped. The automatic startup backfill in `autorize run` / `resume` is unchanged — it never overwrites and still only fills missing summaries.

## [0.2.12] - 2026-05-24

### Changed

- The default `summarize.command` now produces **tight, summary-only output** from Haiku. Previously the default passed the prompt as a *path* (`claude --model haiku --print {prompt_file}`, `stdin = "none"`), which made `claude` Read the file and answer in an interactive-assistant voice — prefacing summaries with `"I've read the file…"` and appending follow-up questions like `"What would you like to do next?"`. The default is now `claude --model haiku --print --tools "" --system-prompt "You are a terse summarizer. …"` with `stdin = "prompt"`: the prompt is piped on stdin (nothing to Read), a full `--system-prompt` replaces the agentic persona with a terse summarizer (no preamble, no markdown, no trailing questions), and `--tools ""` disables all tools. The summary-prompt body (`build_summary_prompt`) drops its in-body output instructions accordingly, since those now live in the system prompt. **Note:** `--bare` was deliberately *not* added — it forces API-key auth and never reads OAuth/keychain, which would silently break summarization for subscription-auth users. Existing experiments with their own `[summarize]` section are unaffected; only the built-in default and the scaffolding template change.

## [0.2.11] - 2026-05-24

### Changed

- `[summarize]` is now **enabled by default**, even when the section is absent: `summarize.enabled` defaults to `true` and `summarize.command` defaults to `claude --model haiku --print {prompt_file}` (Haiku via `claude --print`). Previously summarization was off unless a config opted in. Existing experiments without a `[summarize]` section now get per-iteration summaries automatically — and, on the next `autorize run` / `resume` (or `autorize backfill <name>`), a one-time startup backfill that may fire many independent Haiku calls to fill in summaries for past iterations. To keep the old behavior, add `[summarize]\nenabled = false` to the experiment's `config.toml`. The command inherits `[agent.env]` for credentials (e.g. `ANTHROPIC_API_KEY`), as before.

## [0.2.10] - 2026-05-24

### Added

- New hidden `autorize backfill <name>` maintenance subcommand. It runs the same missing-summary backfill that `autorize run` / `resume` perform at startup, but as a one-shot that exits immediately — so you can fill in summaries for a *stopped* experiment without having to start (and then stop) a run. It acquires the experiment lock for the duration of the call, so it never races a live `autorize run` that is appending to `iterations.jsonl` (a concurrent run makes it fail fast with the lock error rather than corrupting the log). With `[summarize]` disabled it prints a no-op message and exits 0 without touching any file; otherwise it reports whether any summaries were written. The command is intentionally omitted from `autorize --help` as an internal utility.

## [0.2.9] - 2026-05-24

### Added

- When `[summarize]` is enabled, `autorize run` / `resume` now **backfill** missing summaries at startup: any `iterations.jsonl` record without a summary (written before `[summarize]` was enabled, or whose summarize step failed) is regenerated from its persisted `iter-NNNN/` artifacts (`changes.diff`, `agent.stdout`, `agent.stderr` — which survive `autorize clean`), then written back to `iterations.jsonl` (atomic full rewrite under the run lock) and `iter-NNNN/summary.md`. There is no flag. It is best-effort: it skips `noop`/`killed` records and records whose artifacts are gone, and a single failure logs a warning and continues without aborting the run. The reconstructed records also feed the very first prompt's recent-iterations slice. The first run after enabling summaries may fire many one-time, independent model calls.

### Changed

- The per-iteration prompt no longer embeds a "Best iteration so far" section with the best iteration's full diff. Each iteration's worktree is created off the `autorize/<name>` branch, which only advances on a merge, so the next iteration's working tree *already contains* the best result on disk (retrievable via `git show HEAD` / `git diff <base_commit>`) — re-rendering it as prompt text was redundant and could bloat the prompt. The recent-iterations table still surfaces the best score.

## [0.2.8] - 2026-05-24

### Added

- New `autorize tell <name> <message>` subcommand: an operator-driven channel to steer a live run. It appends a structured entry to `.autorize/<name>/guidance.jsonl` (`{ ts, added_at_iter, text }`); the running loop re-reads that file at the top of every iteration and injects all entries into a prominent `## Operator guidance` prompt section, framed as authoritative direction that takes precedence over `program.md` where they conflict. The message appears in the very next iteration and persists thereafter. `guidance.jsonl` is also safe to hand-edit; a missing/empty file renders no section, and a malformed file is non-fatal to the run (logged and ignored). In v1 all guidance is kept and shown every iteration (no consumed/expiry mechanism yet). Unquoted trailing words are joined with spaces, so `autorize tell pi do X` and `autorize tell pi "do X"` are equivalent.
- New `[summarize]` config section: after the worker agent exits, autorize can run a **separate** (typically weaker/cheaper) model to write a 1-2 sentence summary of what the iteration attempted and why the score moved. Summaries are surfaced to the agent in later iterations under a `## Recent attempt summaries` prompt section (so it can learn from discarded attempts instead of re-exploring dead ends) and by `autorize status` (`last summary`). The step has its own `command` and `timeout` (independent of `iteration.budget`), mirrors `[agent]`'s `{prompt_file}`/`{workdir}`/`{iter}` substitution and `stdin` modes, inherits `[agent.env]`, and writes `iter-NNNN/summary-prompt.md` + `iter-NNNN/summary.md` outside the scored worktree. It is skipped for `noop` iterations and is best-effort: any failure (model unavailable, timeout, nonzero exit) leaves the summary empty without affecting the iteration outcome. Disabled by default when the section is absent (back-compatible); the scaffolded `config.toml` enables it with `claude --model haiku --print {prompt_file}`.
- `IterationRecord` gains a `summary` field in `iterations.jsonl`. It is `#[serde(default)]`, so pre-existing logs without the field still load.
- Every `iterations.jsonl` record now carries a populated `notes` reason describing why the iteration ended as it did: `improved: <s> from <b>` / `first valid score: <s>` (merged), `regressed: <s> vs best <b> (min|max)` (discarded), `denied: touched <paths>` (denied), `invalid: <failure detail>` (invalid), and `no changes produced` (noop). Previously `notes` was empty for normal outcomes. The field remains `#[serde(default)]`, so old logs are unaffected.

### Changed

- The per-iteration prompt's "Recent iterations" table gains a `reason` column (and, when summaries are enabled, a separate `## Recent attempt summaries` list), so the agent can see *why* prior attempts were kept or discarded instead of just their scores.
- `autorize status` now prints a `last reason` line (and a `last summary` line when summaries are enabled), each shown only when the corresponding field is non-empty. If you parse `autorize status` output, expect these optional extra lines.

## [0.2.7] - 2026-05-24

### Added

- `autorize run --fresh <name>` starts another run on a finished experiment, building on the prior best. It recomputes the deadline from `schedule`, resets the per-run `max_iterations` budget and the consecutive-noop streak, and refreshes `started_at` — while preserving `best_score`/`best_iter`, the `base_commit`, the `autorize/<name>` branch and its tip, and the full `iterations.jsonl` history. New iterations keep comparing against the prior best and keep numbering strictly upward. It is a no-op on a never-run experiment, is refused (with a pointer to `autorize resume`) when an iteration is mid-flight, and errors clearly when the recomputed deadline is an already-past absolute `schedule.deadline` instead of entering a loop that exits immediately. This is the sanctioned replacement for the old "delete `state.json` by hand" recipe.
- At the default `info` log level, `logs/autorize.log` is now a forensic audit trail: every git invocation (argv + cwd), every subprocess spawn (command + workdir + exit/timeout), and every filesystem mutation (the prompt, agent stdout/stderr, the captured diff, `state.json`, and `iterations.jsonl`) are recorded — dozens of lines per iteration. `agent.env` values (e.g. `ANTHROPIC_API_KEY`) are never logged. Set `RUST_LOG=warn` to quiet this (which also hides the run narrative).

### Changed

- `state.json` gains a `run_iterations_completed` field (per-run iteration count) alongside the lifetime `iterations_completed`. `max_iterations` is now checked against the per-run counter, and `--fresh` resets it. Older `state.json` files load unchanged: the field is migrated on read by seeding it from `iterations_completed`, so a non-fresh re-run stops at the same cap as before.
- `autorize status` now prints the iteration line as `iterations   N total, M this run` (previously just `iterations   N`). If you parse status output, update accordingly.
- Reworded `autorize clean`'s output (and its README/llms descriptions) to make clear it only detaches a stale worktree that held the `autorize/<name>` branch — the branch ref itself is never created, moved, or deleted, and is preserved for checkout.

### Fixed

- A crashed iteration that `autorize resume` records as `killed` no longer consumes a `max_iterations` slot. The `killed` record still counts toward the lifetime `iterations_completed`, but the per-run budget is left untouched, so a resumed experiment can complete its full intended number of iterations.

## [0.2.6] - 2026-05-23

### Added

- New `autorize clean <name>` subcommand to tidy a finished or abandoned experiment: it frees the `autorize/<name>` tracking branch when a stale worktree still holds it (pre-v0.2.4 residue), clears leftover staged indexes on kept iteration worktrees, and prunes registrations for `wt/` directories that no longer exist. Pass `--remove-worktrees` to also delete kept `wt/` checkouts and reclaim disk. The durable log (`iterations.jsonl`), `state.json`, and per-iteration artifacts are never touched.
- Central append-only run log at `logs/autorize.log` (project-root relative), capturing autorize's own run narrative plus the teed stdout/stderr of every child process. `logs/` is created on startup and gitignored; set `RUST_LOG` (default `info`) to tune verbosity.

### Changed

- The run narrative (iteration progress, stop reasons, final summary) and `init` output now go through structured logging to stderr and `logs/autorize.log` instead of stdout. If you previously parsed this narrative from stdout, read it from stderr or the log file.
- The dirty-tree pre-flight check for `autorize run` now also ignores the `logs/` directory (in addition to `.autorize/`), since autorize creates the log on startup.

### Fixed

- Kept (non-merged) iteration worktrees no longer retain a stray fully-staged `git add -A` index from diff capture; they now read as ordinary unstaged dirty checkouts. Committed/merged content is unaffected.

## [0.2.5] - 2026-05-23

### Fixed

- Tear down the agent's process group on a fatal signal (Ctrl-C / SIGTERM / SIGHUP) instead of orphaning it. Because agents run in their own session (so per-iteration budget kills can reach grandchildren), they were detached from the terminal's foreground group and survived when autorize was interrupted. autorize now SIGTERMs every live child group, waits a short grace period, SIGKILLs any survivors, and exits with the conventional `128 + signal` status.

## [0.2.4] - 2026-05-23

### Fixed

- Experiments with `keep_worktrees = true` no longer fail on the second iteration. Previously the kept worktree held the tracking branch checked out, so creating the next iteration's worktree failed with "branch already used by worktree". Iteration worktrees now use a detached HEAD, which lets multiple worktrees share one tracking branch.

### Changed

- Iteration worktrees are now created with a detached HEAD; the tracking branch (`autorize/<name>`) is advanced explicitly when an iteration's changes are merged. The branch still lands on the same commits with the same `autorize iter N: score S` messages, so resume/reconciliation and existing on-disk experiments are unaffected — no migration needed. As a bonus, an experiment left in the old broken state by a previous version recovers automatically.

## [0.2.3] - 2026-05-23

### Added

- `/autorize` Claude Code skill (`skills/autorize/`) that interviews you about your objective, scoring command, agent CLI, schedule, and boundaries, then drafts `.autorize/<name>/{config.toml,program.md}` plus any helper scoring script for your review before writing — and stops at "ready to run", never starting the loop itself.
- README "Use with Claude Code" section with copy-paste install steps for the skill, and crates.io install instructions (`cargo install autorize`).

### Changed

- The per-iteration prompt now explicitly tells the agent to only edit files in the working tree and not to run `git add` / `git commit` itself, clarifying that autorize captures the uncommitted changes and commits them on the agent's behalf.

## [0.2.2] - 2026-05-21

### Added

- Official macOS support (`aarch64-apple-darwin`, Apple Silicon) alongside Linux (`x86_64-unknown-linux-gnu`). Both targets are first-class and exercised in CI.
- GitHub Release workflow publishes prebuilt `autorize` binaries for Linux and macOS on every `v*` tag, with SHA-256 checksums and a self-test that extracts and runs the Linux archive before the release is finalized. Pre-releases (e.g. `v0.2.2-rc1`) are auto-detected from the tag.
- GitHub Actions CI workflow runs `cargo fmt --check` (nightly rustfmt, matching the project's unstable import-grouping config), `cargo clippy --all-targets -D warnings`, and `cargo test --all` on Linux and macOS for every push to `main` and every pull request.
- README install section now documents both the prebuilt-binary path (with a copy-pasteable `curl | tar` snippet) and the `cargo install --path .` from-source path, and lists the supported target triples.

### Fixed

- `worktree_list` test no longer fails on macOS, where git canonicalizes recorded worktree paths through `/private/var/...` while the test created them under `/var/...`. The assertion now compares canonicalized paths on both sides.

### Notes

- No changes to the `autorize` binary's behavior, CLI surface, config schema, or on-disk formats (`state.json`, `iterations.jsonl`). This is a packaging and CI release.

## [0.2.1] - 2026-05-20

### Changed

- Reworded the README opener — autorize is a CLI you point at a repo, not a binary you drop in.
- Dropped the `aider` mention from the agent-integration examples (README and 0.1.0 changelog entry).

## [0.2.0] - 2026-05-20

### Added

- `autorize llms` subcommand prints an exhaustive agent-targeted markdown reference covering every `config.toml` field, all `CurrentStep`/`Outcome` variants, the `IterationRecord`/`StateSnapshot` schemas, the on-disk layout, `agent.command`/`agent.env`/`agent.stdin` semantics, the schedule grammar, pre-flight checks, and an end-to-end example — for dropping an agent into a fresh repo without source reading.
- Exclusive per-experiment flock at `.autorize/<name>/run.lock` guards `autorize run`. A second concurrent run fails fast with the holder's pid and the lock path instead of racing on `state.json` / `iterations.jsonl` / the tracking branch.
- `Error::Locked { path, detail }` variant surfaced when the lock is held by another process.

### Fixed

- `autorize resume` now reconciles mid-merge crashes correctly. Previously, a crash between `git commit` and `append_iteration` either lost the iteration's score (recorded as `killed` / `score: null` despite the merge having landed) or wrote a duplicate `iterations.jsonl` record. Resume now replays an existing record if one already exists for the in-progress iter, synthesizes a `Merged` record from the tracking branch tip when the subject matches `autorize iter <N>: score <S>`, or falls back to the prior `killed` behavior. `state.best_*` is updated via a direction-aware improvement check.
- `fail_mode = "worst"` now uses finite sentinels (`f64::MAX` for `direction = "min"`, `f64::MIN` for `direction = "max"`) instead of `±inf`. `serde_json` serializes non-finite f64 as `null`, so the previous sentinels were lost across every `state.json` rewrite and every `iterations.jsonl` append, silently defeating worst-mode comparisons. The new sentinels round-trip cleanly through JSON.
- Score parsers (`float`, `regex`, `jq`) now reject non-finite values (`NaN`, `inf`, `-inf`, `Infinity`, etc.) as `ScoreFailure::Parse`, routing through the configured `fail_mode` instead of corrupting state.
- Empty `boundaries.allow_paths` and `boundaries.deny_paths` no longer emit an empty `## Boundaries` section in the iteration prompt.

### Notes

- No schema changes to `state.json` or `iterations.jsonl`; v0.1.0 files load unchanged. The new finite worst-mode sentinels appear as large finite numbers (not `null`) in records written after upgrade.

## [0.1.0] - 2026-05-20

### Added

- `autorize init <name>` scaffolds an experiment under `.autorize/<name>/` with a templated `config.toml` and `program.md`.
- `autorize run <name>` drives the iterative-improvement loop: spawns your agent CLI in a fresh git worktree per iteration, scores the result, keeps improvements on a tracking branch (`autorize/<name>`), and discards regressions.
- `autorize status <name>` prints a one-shot summary (best score, iterations completed, noop streak, elapsed/remaining time).
- `autorize resume <name>` recovers cleanly after a crash or `Ctrl-C`, recording any in-progress iteration as `killed` and continuing the loop.
- Configurable scoring via three parse modes: raw float, regex capture group, or JSONPath (with jq-style `.path` accepted).
- Hard wall-clock budgets enforced per-iteration and per-experiment: process-group `SIGTERM` then `SIGKILL` on timeout, reaching grandchildren a plain kill would orphan.
- Deadline expressions accept humantime durations (`4h`), RFC3339 timestamps, or natural language (`tomorrow 9am`, `14:30`).
- Boundary enforcement: `deny_paths` globs reject any iteration whose diff (including new files) touches forbidden paths.
- Agent integration is CLI-agnostic — works with Claude Code, shell scripts, or anything else; supports `{prompt_file}`/`{workdir}`/`{iter}` substitution, env var injection with `$VAR` expansion, and prompt-via-file or prompt-via-stdin delivery.
- Durable on-disk record: atomic `state.json` checkpoints at every step, append-only `iterations.jsonl` log, per-iter prompt/diff/stdout/stderr artifacts; torn-write-tail tolerant on resume.
- Loop termination on total-budget deadline, `max_iterations` cap, or `max_consecutive_noops` streak.
- Pre-flight safety: refuses dirty trees (with `--allow-dirty` escape hatch that still excludes `.autorize/`), validates base commit reachability, and refuses to start over an in-progress experiment without `resume`.
- End-to-end `examples/pi-digits/` demo where a mock agent converges a number in `value.txt` toward π.
