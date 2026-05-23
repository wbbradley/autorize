# Changelog

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
