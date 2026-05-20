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
