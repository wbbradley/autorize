# autorize

`autorize` is a generic iterative-improvement harness. You point it at a project, a
scoring command, and an agent CLI, and it runs the agent in sandboxed git worktrees
against the score — keeping improvements, discarding regressions — until a deadline
fires.

It generalizes the [`autoresearch`](https://github.com/karpathy/autoresearch) pattern
into a small Rust CLI you can point at any repo.

## How it works

For each iteration, `autorize`:

1. Creates a fresh git worktree off the `autorize/<name>` tracking branch.
2. Builds a prompt from your `program.md`, the boundary rules, the last 10
   iteration records, and the diff of the best iteration so far.
3. Spawns your agent (any CLI — Claude Code, a shell script, anything)
   inside the worktree with a hard wall-clock budget. On timeout the whole
   process group gets `SIGTERM`, then `SIGKILL` after 5 s.
4. Stages the agent's changes and rejects the iteration if its diff touches a
   `deny_paths` glob.
5. Runs your scoring command (raw float, regex capture, or JSONPath) and
   compares against the best score seen so far.
6. **Better?** Commits onto `autorize/<name>` and advances the tracking branch.
   **Worse / no-op / denied / invalid?** Discards the worktree.
7. Appends an `IterationRecord` to `iterations.jsonl` and rewrites `state.json`
   atomically so you can `Ctrl-C` (or crash) at any point and `autorize resume`
   picks up cleanly.

The loop exits when the total deadline fires, `max_iterations` is hit, or
`max_consecutive_noops` is reached.

## Install

Supported platforms: Linux (`x86_64-unknown-linux-gnu`) and macOS
(`aarch64-apple-darwin`, Apple Silicon).

**From crates.io**:

```sh
cargo install autorize
```

**Prebuilt binary** (from the latest GitHub Release):

```sh
# Pick your target:
TARGET=x86_64-unknown-linux-gnu       # or: aarch64-apple-darwin

# Resolve the latest tag, then download + extract:
TAG=$(curl -fsSL -o /dev/null -w '%{url_effective}' \
  https://github.com/wbbradley/autorize/releases/latest | sed 's#.*/tag/##')
curl -fsSL "https://github.com/wbbradley/autorize/releases/download/${TAG}/autorize-${TAG}-${TARGET}.tar.gz" \
  | tar -xz
./autorize --version
```

Or browse <https://github.com/wbbradley/autorize/releases/latest> and grab the
archive for your target by hand.

**From source**:

```sh
cargo install --path .
```

## Quickstart

```sh
# 1. Scaffold an experiment under .autorize/<name>/
autorize init myexp

# 2. Edit .autorize/myexp/config.toml and .autorize/myexp/program.md
#    - point `objective.command` at your scoring script
#    - point `agent.command` at your agent CLI
#    - set a deadline (`total_budget = "4h"` or `deadline = "..."`)

# 3. Commit your repo (autorize refuses dirty trees by default), then run:
autorize run myexp

# 4. Check progress from another shell:
autorize status myexp

# 5. If the loop dies, restart it:
autorize resume myexp
```

## Use with Claude Code

This repo ships a Claude Code skill at [`skills/autorize/`](skills/autorize/) that
walks you through scaffolding an experiment — it asks about your objective, scoring
command, agent CLI, and schedule, then drafts `.autorize/<name>/config.toml`,
`program.md`, and any helper scoring script for your review before writing.

Install once (user-global, applies to every repo you open):

```sh
mkdir -p ~/.claude/skills
cp -r skills/autorize ~/.claude/skills/
```

Or per-project (only this repo):

```sh
mkdir -p .claude/skills
cp -r skills/autorize .claude/skills/
```

Then, from a Claude Code session in any repo with `autorize` on PATH, invoke
`/autorize`. The skill prints `autorize llms` for context, interviews you,
and stops at "ready to `autorize run <name>`" — it never starts the loop.

## Subcommands

| Command | What it does |
|---|---|
| `autorize init <name>`   | Scaffold `.autorize/<name>/{config.toml,program.md}`. |
| `autorize run <name>`    | Run the loop until deadline / cap / noop streak. |
| `autorize status <name>` | One-shot summary from `state.json` + `iterations.jsonl`. |
| `autorize resume <name>` | Recover after a crash; any in-progress iter is recorded as `killed` and the loop continues. |
| `autorize clean <name>`  | Tidy a finished/abandoned experiment: detach any worktree still holding the tracking branch checked out (the branch ref is preserved), drop stale staged indexes, prune dead worktree registrations (`--remove-worktrees` also deletes kept `wt/` checkouts). Leaves the log and records intact. |
| `autorize llms`          | Print an exhaustive agent-targeted markdown reference (config schema, on-disk layout, `IterationRecord`, state machine). |

`autorize run` accepts `--allow-dirty` if you need to start with uncommitted
changes outside `.autorize/`.

## Config (`.autorize/<name>/config.toml`)

```toml
[experiment]
name = "myexp"
description = "..."

[objective]
command   = "bash score.sh"        # prints the score to stdout
direction = "min"                  # "min" | "max"
parse     = { kind = "float" }     # or { kind = "regex", pattern = "score=([0-9.]+)" }
                                   # or { kind = "jq",    path = ".metrics.loss" }
timeout   = "60s"
fail_mode = "invalid"              # "invalid" | "worst" | "abort"

[boundaries]
allow_paths = ["src/**/*.py"]      # prompt-only in v1
deny_paths  = [".autorize/**"]     # ENFORCED via diff

[setup]    { command = "",  timeout = "5m" }
[teardown] { command = "",  timeout = "1m" }

[iteration]
budget                = "5m"
max_iterations        = 0          # 0 = unbounded
keep_worktrees        = false
max_consecutive_noops = 5

[schedule]
total_budget = "4h"                # OR (exactly one):
# deadline   = "2026-05-21T09:00:00-07:00"

[agent]
command     = "claude --print {prompt_file}"   # {prompt_file}, {workdir}, {iter}
workdir_var = "AUTORIZE_WORKDIR"
stdin       = "none"                            # "none" | "prompt"

[agent.env]
ANTHROPIC_API_KEY = "$ANTHROPIC_API_KEY"
```

`program.md` lives next to `config.toml` and is freeform instructions for the
agent — included verbatim at the top of every prompt.

## On-disk layout

```
<repo>/
  logs/autorize.log        # central append-only run log (narrative + teed child stdio)
  .autorize/<name>/
    config.toml
    program.md
    state.json             # atomic checkpoint of loop state
    iterations.jsonl       # durable append-only log
    iter-0001/
      prompt.md            # what the agent saw
      changes.diff         # captured diff
      agent.stdout
      agent.stderr
    iter-0002/
    ...
```

`logs/` is created on startup (gitignore it). `RUST_LOG` tunes verbosity
(default `info`). At `info` the log is a forensic audit trail — every git
call, subprocess spawn, and filesystem mutation is recorded (dozens of lines
per iteration; `agent.env` secrets are never logged). Use `RUST_LOG=warn` to
quiet it (also hides the run narrative).

The tracking branch `autorize/<name>` records every merged iteration as a
single commit, so `git log autorize/<name>` is your improvement history and
`git diff main..autorize/<name>` is the cumulative change.

## Example

See [`examples/pi-digits/`](examples/pi-digits/) for an end-to-end demo where a
mock agent nudges a number in `value.txt` toward π:

```sh
cp -r examples/pi-digits/. /tmp/pi-demo
cd /tmp/pi-demo
git init -b main
git -c user.email=a@b -c user.name=a add .
git -c user.email=a@b -c user.name=a commit -m init
autorize run pi
```

## Status

v1 is feature-complete on Linux and macOS (Apple Silicon). Out of scope for v1:
parallel iterations, Pareto scoring, web/TUI, token accounting, retry/backoff,
remote storage, allow-path enforcement (allow_paths is prompt-only).

## License

AGPL-3.0-or-later.
