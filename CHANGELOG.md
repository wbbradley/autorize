# Changelog

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
- Agent integration is CLI-agnostic — works with Claude Code, `aider`, shell scripts, or anything else; supports `{prompt_file}`/`{workdir}`/`{iter}` substitution, env var injection with `$VAR` expansion, and prompt-via-file or prompt-via-stdin delivery.
- Durable on-disk record: atomic `state.json` checkpoints at every step, append-only `iterations.jsonl` log, per-iter prompt/diff/stdout/stderr artifacts; torn-write-tail tolerant on resume.
- Loop termination on total-budget deadline, `max_iterations` cap, or `max_consecutive_noops` streak.
- Pre-flight safety: refuses dirty trees (with `--allow-dirty` escape hatch that still excludes `.autorize/`), validates base commit reachability, and refuses to start over an in-progress experiment without `resume`.
- End-to-end `examples/pi-digits/` demo where a mock agent converges a number in `value.txt` toward π.
