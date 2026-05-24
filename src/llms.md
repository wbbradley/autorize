# autorize — agent-targeted reference

This document is printed by `autorize llms`. It is meant for LLM/agent
consumers landing in a repository that uses `autorize`. Everything an agent
needs to drive `autorize init` → edit config → `autorize run` →
`autorize status` / `autorize resume` is here. No
[source code](https://github.com/wbbradley/autorize) reading required.

## 1. What `autorize` is

`autorize` is a generic iterative-improvement harness. For each iteration it
creates a fresh **git worktree** off the `autorize/<name>` tracking branch,
runs your agent CLI inside the worktree with a hard wall-clock budget, then
runs a scoring command. If the score improves, the worktree's diff is
committed onto the tracking branch; otherwise it is discarded. The loop
stops when a total deadline fires, `max_iterations` is hit, or a configurable
number of consecutive no-op iterations is reached. State is checkpointed
atomically so the loop can be killed and resumed at any point.

## 2. Subcommands and workflow

| Command                  | What it does                                                                                          |
|--------------------------|-------------------------------------------------------------------------------------------------------|
| `autorize init <name>`   | Scaffold `.autorize/<name>/{config.toml, program.md}`.                                                |
| `autorize run <name>`    | Run the loop until deadline / `max_iterations` / `max_consecutive_noops`. `--fresh` starts another run on a finished experiment (see §11/§14). |
| `autorize status <name>` | Print a one-shot summary from `state.json` + `iterations.jsonl`.                                      |
| `autorize tell <name> <message>` | Append a line of **operator guidance** to `guidance.jsonl`; the running loop re-reads it each iteration and injects it into the prompt under `## Operator guidance` (see §15). |
| `autorize resume <name>` | Recover after a crash; any in-progress iter is recorded as `killed` and the loop continues.           |
| `autorize clean <name>`  | Tidy a finished/abandoned experiment: detach any worktree still holding the tracking branch checked out (the branch ref is **preserved** — never created/moved/deleted), drop stale staged indexes, prune dead worktree registrations. `--remove-worktrees` also deletes kept `wt/` checkouts. Never touches `iterations.jsonl`/`state.json`. |
| `autorize llms`          | Print this document.                                                                                  |

End-to-end workflow:

1. `autorize init <name>` — scaffolds `.autorize/<name>/config.toml` and
   `.autorize/<name>/program.md`.
2. Edit `.autorize/<name>/config.toml` (point `objective.command` at a
   scoring script, point `agent.command` at an agent CLI, set a schedule).
3. Edit `.autorize/<name>/program.md` (freeform agent instructions; included
   verbatim at the top of every prompt).
4. Commit the repo — `autorize run` refuses a dirty tree by default
   (use `--allow-dirty` to override; the `.autorize/` directory and the
   `logs/` run log are always ignored for the dirty-tree check).
5. `autorize run <name>` — drives the loop.
6. `autorize status <name>` — one-shot summary from another shell.
7. `autorize resume <name>` — recover after a crash or `Ctrl-C` mid-iter.

## 3. Iteration state machine and outcomes

Each iteration runs through these steps in order, checkpointing
`state.json` between each:

```
Idle
  -> AllocateIter      mkdir iter-NNNN/
  -> CreateWorktree    git worktree add ... autorize/<name>
  -> RunSetup          setup.command (skipped if empty)
  -> BuildPrompt       render -> iter-NNNN/prompt.md
  -> InvokeAgent       spawn agent.command with hard wall-clock budget
                       (SIGTERM the whole process group, 5 s grace, SIGKILL)
  -> CaptureDiff       git stage-all + diff against autorize/<name>
                       empty diff -> noop; touches deny_paths -> denied
  -> RunTeardown       teardown.command (skipped if empty)
  -> Score             run objective.command, parse to Option<f64>;
                       on failure: apply objective.fail_mode
  -> Decide            improved compared to best so far?
  -> Merge             commit on autorize/<name>, advance tracking branch
  -> Discard           (used when the score does not improve)
  -> Cleanup           remove worktree (unless iteration.keep_worktrees)
  -> Record            append IterationRecord to iterations.jsonl + rewrite state.json
  -> CheckDeadline     deadline | max_iterations | consecutive_noops? -> Done
                       otherwise loop back to AllocateIter
```

The `current_step` field in `state.json` always carries one of:
`Idle`, `AllocateIter`, `CreateWorktree`, `RunSetup`, `BuildPrompt`,
`InvokeAgent`, `CaptureDiff`, `RunTeardown`, `Score`, `Decide`, `Merge`,
`Discard`, `Cleanup`, `Record`, `CheckDeadline`, `Done`.

Each iteration ends in exactly one of these six **outcomes** (the
`outcome` field of an `IterationRecord` in `iterations.jsonl`):

| Outcome     | Meaning                                                                                |
|-------------|----------------------------------------------------------------------------------------|
| `merged`    | Score improved over the best so far; diff committed on `autorize/<name>`.              |
| `discarded` | Agent produced a diff that scored, but the score did not improve.                      |
| `noop`      | Agent produced an empty diff (no changes). Counts toward `max_consecutive_noops`.      |
| `invalid`   | Scoring failed under `fail_mode = "invalid"`; iteration is discarded, not counted as best. |
| `killed`    | Recorded by `autorize resume` for an iteration that was in-flight at crash time. Counts toward the lifetime `iterations_completed` but **not** the per-run `max_iterations` budget. |
| `denied`    | Diff touched a `boundaries.deny_paths` pattern; iteration discarded, branch unchanged. |

## 4. Configuration: `.autorize/<name>/config.toml`

Below is the exhaustive schema. All field names, types, defaults, and
validation rules are listed.

### `[experiment]`

| Field         | Type   | Default | Notes                                                    |
|---------------|--------|---------|----------------------------------------------------------|
| `name`        | string | (required) | Must match `[A-Za-z0-9_-]+`. Used as the experiment dir and the `autorize/<name>` branch suffix. |
| `description` | string | `""`    | Freeform.                                                |

### `[objective]`

| Field        | Type     | Default     | Notes                                                                                  |
|--------------|----------|-------------|----------------------------------------------------------------------------------------|
| `command`    | string   | (required)  | Shell command. Run via `bash -lc` inside the iteration's worktree. Must be non-empty.  |
| `direction`  | enum     | (required)  | `"min"` or `"max"`. Determines what counts as an improvement.                          |
| `parse`      | table    | (required)  | See `objective.parse` section below.                                                   |
| `timeout`    | duration | `"60s"`     | humantime duration; how long `objective.command` is allowed to run.                    |
| `fail_mode`  | enum     | `"invalid"` | `"invalid"`, `"worst"`, or `"abort"`. See `objective.fail_mode` section below.         |

### `[boundaries]`

| Field         | Type            | Default | Notes                                                                                |
|---------------|-----------------|---------|--------------------------------------------------------------------------------------|
| `allow_paths` | array of string | `[]`    | Glob patterns. **Prompt-only in v1** — included in the agent prompt, not enforced.   |
| `deny_paths`  | array of string | `[]`    | Glob patterns. **Enforced**: an iteration whose diff touches any of these is `denied`. |

### `[setup]`

Run once per iteration, inside the worktree, before `agent.command`.

| Field     | Type     | Default | Notes                                                                |
|-----------|----------|---------|----------------------------------------------------------------------|
| `command` | string   | `""`    | Empty string skips setup.                                            |
| `timeout` | duration | `"5m"`  | humantime duration.                                                  |

### `[teardown]`

Run once per iteration, inside the worktree, after scoring.

| Field     | Type     | Default | Notes                                                                |
|-----------|----------|---------|----------------------------------------------------------------------|
| `command` | string   | `""`    | Empty string skips teardown.                                         |
| `timeout` | duration | `"1m"`  | humantime duration.                                                  |

### `[iteration]`

| Field                   | Type     | Default | Notes                                                                       |
|-------------------------|----------|---------|-----------------------------------------------------------------------------|
| `budget`                | duration | `"5m"`  | Hard wall-clock per agent invocation. Must be greater than zero.            |
| `max_iterations`        | integer  | `0`     | `0` means unbounded.                                                        |
| `keep_worktrees`        | bool     | `false` | Retain per-iter `wt/` directories under `iter-NNNN/` for debugging.         |
| `max_consecutive_noops` | integer  | `5`     | Loop exits after this many consecutive `noop` outcomes.                     |

### `[schedule]`

**Set exactly one** of `total_budget` or `deadline`. Validation rejects
both-set or neither-set.

| Field          | Type     | Default | Notes                                                                                  |
|----------------|----------|---------|----------------------------------------------------------------------------------------|
| `total_budget` | duration | (unset) | humantime duration. Deadline computed as `now + total_budget` at first `run`.          |
| `deadline`     | string   | (unset) | See `schedule` grammar below for accepted forms.                                       |

### `[agent]`

| Field         | Type   | Default            | Notes                                                                                                                                                  |
|---------------|--------|--------------------|--------------------------------------------------------------------------------------------------------------------------------------------------------|
| `command`     | string | (required)         | Shell command. Substitutions: `{prompt_file}`, `{workdir}`, `{iter}`. Must contain `{prompt_file}` when `stdin = "none"`. Run via `bash -lc`.          |
| `workdir_var` | string | `"AUTORIZE_WORKDIR"` | Name of the env var injected into the agent process containing the absolute path of the iteration's worktree.                                          |
| `stdin`       | enum   | `"none"`           | `"none"`: nothing piped on stdin; the command **must** contain `{prompt_file}`. `"prompt"`: the prompt file contents are piped on stdin.               |

### `[agent.env]`

A sub-table mapping environment variable name to string value. The value is
expanded for `$NAME` / `${NAME}` references against the parent process
environment **before** being passed to the agent.

| Field               | Type   | Default | Notes                                                                                       |
|---------------------|--------|---------|---------------------------------------------------------------------------------------------|
| `ANTHROPIC_API_KEY` | string | (none)  | Example entry in the default template — passes the parent env's `$ANTHROPIC_API_KEY` through. |
| (any name)          | string | (none)  | Any user-defined env var; values with `$VAR` / `${VAR}` are expanded from the parent env.    |

### `[summarize]`

After the worker agent exits, autorize can run a **separate** (typically
weaker/cheaper) model to write a 1-2 sentence summary of what the iteration
attempted and why the score moved. These summaries are surfaced to the agent in
later iterations under a `## Recent attempt summaries` section of the prompt (so
it can learn from discarded attempts) and by `autorize status` (`last summary`).

The step runs **after** the worker is killed/exits, with its own `command` and
`timeout` (independent of `iteration.budget`, so the summary is never truncated
by the worker's budget). It is **skipped** for `noop` iterations (no diff to
summarize) and is **best-effort**: any failure (model unavailable, timeout,
nonzero exit, empty output) logs a warning and leaves `summary` empty without
changing the iteration outcome. The summary prompt is built from the
iteration's own artifacts only — the diff plus tails of the agent's
stdout/stderr and the outcome/score/best context — **not** `program.md`
guidance or prior summaries (kept lean so a cheap model suffices). The prompt
and output are written to `iter-NNNN/summary-prompt.md` and
`iter-NNNN/summary.md` (outside the scored worktree, so they never trip
deny-path enforcement). The summarizer inherits `[agent.env]`.

**Startup backfill.** The live step above only ever runs for the *current*
iteration as it executes, so records written before `[summarize]` was enabled
(or whose summarize step failed) would otherwise keep an empty `summary`
forever. To fix that, at the top of every `autorize run` / `resume` (when
`enabled = true`) autorize automatically backfills summaries for *all* records
missing one. It reconstructs each record's "best so far" context, rebuilds the
same self-contained summary prompt from the persisted `iter-NNNN/` artifacts
(`changes.diff` + `agent.stdout`/`agent.stderr`, which survive `autorize
clean`), runs the summarize command from the project root, and rewrites
`iterations.jsonl` in place (atomic full rewrite under the run lock) plus the
per-iter `summary.md`. It is best-effort — it skips `noop`/`killed` records and
records whose `iter-NNNN/` artifacts are gone, and a single failure logs a
warning and continues without aborting the run. There is no flag; the first run
after enabling `[summarize]` may fire many one-time, independent model calls.

| Field     | Type     | Default | Notes                                                                                                                                                  |
|-----------|----------|---------|--------------------------------------------------------------------------------------------------------------------------------------------------------|
| `enabled` | bool     | `false` when the section is absent; `true` in the scaffolded template | Master switch. When `false`, the step is fully disabled (no behavior change beyond the existing `notes`). |
| `command` | string   | (required when enabled) | Shell command for the summarizer. Same `{prompt_file}` / `{workdir}` / `{iter}` substitutions as `[agent]`. Must contain `{prompt_file}` when `stdin = "none"`. Default template uses `claude --model haiku --print {prompt_file}` (overridable). |
| `timeout` | duration | `"60s"` | humantime duration; hard wall-clock budget for the summarize command, independent of `iteration.budget`.                                              |
| `stdin`   | enum     | `"none"` | `"none"` or `"prompt"`, mirroring `[agent].stdin`. `"none"` requires `{prompt_file}` in `command`.                                                     |

## 5. `objective.parse` variants

All three accept input from the scoring command's stdout.

```toml
# Raw float: the entire stdout (trimmed) is parsed as a float.
parse = { kind = "float" }
```

```toml
# Regex: the first capture group of the first match is parsed as a float.
# The pattern must be non-empty and must contain a capture group.
parse = { kind = "regex", pattern = "score=([0-9.]+)" }
```

```toml
# JSON path: stdout must be valid JSON; the value at the path must be a
# scalar number. Accepts jq-style leading dot (".foo.bar") or JSONPath
# ("$.foo.bar"). The path must be non-empty.
parse = { kind = "jq", path = ".metrics.bpb" }
```

## 6. `objective.fail_mode` semantics

| Value       | Behavior on scoring failure (non-zero exit, timeout, signal, parse error) |
|-------------|---------------------------------------------------------------------------|
| `"invalid"` | Record the iteration with `outcome = "invalid"`; no score, no best update. |
| `"worst"`   | Treat as the worst possible score: `f64::MAX` when `direction = "min"`, `f64::MIN` when `direction = "max"`. These finite sentinels round-trip through JSON (unlike `+inf` / `-inf`, which serde serializes as `null`). Counts as a real (terrible) score. |
| `"abort"`   | Stop the whole `autorize run` with an error.                              |

## 7. `boundaries.deny_paths` vs `boundaries.allow_paths`

- `deny_paths` is a list of glob patterns (globset syntax). After the agent
  runs, `git add -A` stages all changes (including new files) in the
  worktree, then `git diff <branch>` is computed. If any changed path
  matches any deny pattern, the outcome is `denied` and the iteration is
  thrown away — the tracking branch is **not** advanced and scoring is
  skipped.
- `allow_paths` is **prompt-only in v1**: the patterns are included in the
  agent's prompt as a constraint hint, but autorize does not enforce them
  via the diff. Use `deny_paths` if you need enforcement.

## 8. `schedule` grammar

`schedule.deadline` (when used instead of `schedule.total_budget`) accepts
three forms:

| Form                  | Example                              | Meaning                                       |
|-----------------------|--------------------------------------|-----------------------------------------------|
| humantime duration    | `"4h"`, `"30m"`, `"1d"`              | Equivalent to `total_budget`: now + duration. |
| RFC3339 absolute time | `"2026-05-21T09:00:00-07:00"`        | Parsed as an absolute UTC instant.            |
| natural language      | `"tomorrow"`, `"today 3pm"`, `"tomorrow 9am"`, `"tomorrow 14:30"`, `"9am"` | Local-time clock. A bare time like `"9am"` rolls to tomorrow if it is already past today. `"12am"` is midnight, `"12pm"` is noon. |

`schedule.total_budget` only accepts humantime durations.

## 9. `agent.command` substitutions, env expansion, stdin modes

**Command substitutions** (literal token replacement in the
`agent.command` string before it is handed to `bash -lc`):

| Token           | Replaced with                                    |
|-----------------|--------------------------------------------------|
| `{prompt_file}` | Absolute path to `iter-NNNN/prompt.md`.          |
| `{workdir}`     | Absolute path to the iteration's worktree.       |
| `{iter}`        | Decimal iteration number (1-based).              |

**Env expansion** for `agent.env` values:

- `$NAME` and `${NAME}` are expanded against the parent process
  environment. Names match `[A-Za-z_][A-Za-z0-9_]*`.
- Unset variables expand to the empty string.
- A literal `$` followed by a non-name character is preserved verbatim
  (so `"price $5"` stays `"price $5"`).
- The parent environment is passed through automatically; `agent.env`
  values overlay on top. The variable named by `agent.workdir_var`
  (default `AUTORIZE_WORKDIR`) is always injected with the worktree path.

**`agent.stdin` modes**:

- `"none"` (default): nothing is piped on stdin; the command **must**
  contain `{prompt_file}` so the agent can find its instructions. This is
  enforced by config validation.
- `"prompt"`: the contents of `iter-NNNN/prompt.md` are piped on stdin.
  `{prompt_file}` is not required in this mode.

**Wall-clock kill**: every agent invocation is run via `setsid` so it has
its own process group. On `iteration.budget` expiry the harness sends
`SIGTERM` to the whole group, waits up to 5 seconds, then `SIGKILL`s the
group. This reaches grandchildren that a plain `kill(pid)` would orphan.
The `IterationRecord.agent_killed_by_budget` field is set to `true` for
killed iterations.

## 10. On-disk layout

```
<repo>/
  logs/
    autorize.log             # central append-only run log (project-root relative):
                             #   autorize's own narrative (at `info`) plus every
                             #   subprocess's teed stdout/stderr. Append mode, so a
                             #   second run extends rather than truncates it.
  .autorize/<name>/
    config.toml              # the schema documented above
    program.md               # freeform agent instructions
    state.json               # atomic checkpoint of loop state
    iterations.jsonl         # durable append-only log, one JSON object per line
    guidance.jsonl           # operator guidance (see §15); one JSON object per line; hand-editable
    run.lock                 # advisory flock held by the active `autorize run`; contains its pid
    iter-0001/
      prompt.md              # full prompt the agent saw
      changes.diff           # captured diff vs autorize/<name>
      agent.stdout
      agent.stderr
      summary-prompt.md      # prompt sent to the [summarize] model (if enabled & non-noop)
      summary.md             # the model-written summary captured into the record
      wt/                    # the worktree (only if iteration.keep_worktrees = true)
    iter-0002/
    ...
```

`logs/` is created on startup and should be gitignored. Set `RUST_LOG`
(e.g. `RUST_LOG=debug`) to change verbosity; the default is `info`.

At the default `info` level the log is a forensic audit trail: every git
invocation (read-only queries and mutations alike, with argv + cwd), every
subprocess spawn (command + workdir + exit/timeout), and every filesystem
mutation (mkdir/write of prompt.md, agent.stdout/stderr, changes.diff,
state.json, iterations.jsonl) is recorded — dozens of lines per iteration.
`agent.env` values (e.g. `ANTHROPIC_API_KEY`) are **never** logged. Set
`RUST_LOG=warn` to quiet this, at the cost of also hiding the run narrative.

- `state.json` is written via tmp-file + fsync + atomic rename (and best-
  effort directory fsync). A torn write never corrupts the destination.
- `iterations.jsonl` is opened with `O_APPEND` and `fsync`'d after every
  record. The reader tolerates a torn last line (drops it silently); a
  corrupt non-last line is an error.
- `guidance.jsonl` is written the same way (`O_APPEND` + `fsync` per entry,
  so a concurrent `autorize tell` lands atomically while `run` loops) and read
  with the same torn-last-line tolerance. The run loop additionally treats *any*
  read error as non-fatal (logs a warning and proceeds with no guidance), so a
  malformed hand-edit can never kill a run.
- The tracking branch `autorize/<name>` records every merged iteration as
  a separate commit. `git log autorize/<name>` is the improvement history;
  `git diff <base>..autorize/<name>` is the cumulative change since the
  experiment started.

## 11. `IterationRecord` and `StateSnapshot` schemas

Each line in `iterations.jsonl` is one `IterationRecord` JSON object:

| Field                    | Type                  | Meaning                                                                            |
|--------------------------|-----------------------|------------------------------------------------------------------------------------|
| `iter`                   | integer (u64)         | 1-based iteration number. Strictly increasing.                                     |
| `started_at`             | RFC3339 timestamp     | When the iteration began.                                                          |
| `ended_at`               | RFC3339 timestamp     | When the iteration finished (regardless of outcome).                               |
| `outcome`                | string                | One of `"merged"`, `"discarded"`, `"noop"`, `"invalid"`, `"killed"`, `"denied"`.   |
| `score`                  | float or null         | Parsed score, when scoring ran and succeeded.                                      |
| `best_so_far`            | float or null         | Best score across all previous merged iterations (after this one updates it).      |
| `agent_exit`             | integer or null       | Exit code of the agent process. `null` when killed by signal or unable to spawn.   |
| `agent_killed_by_budget` | bool                  | `true` if the wall-clock budget killed the agent process group.                    |
| `diff_lines`             | integer (u64)         | Line count of `iter-NNNN/changes.diff`.                                            |
| `notes`                  | string                | Harness-derived reason for the outcome. Normal iterations are now annotated: `"improved: <score> from <prev best>"` / `"first valid score: <score>"` (merged), `"regressed: <score> vs best <best> (<direction>)"` (discarded), `"denied: touched <paths>"` (denied), `"invalid: <detail>"` (invalid), `"no changes produced"` (noop). Recovery records still use it (`"resumed after crash"`, `"reconciled from branch tip after crash"`). Shown in the next iteration's recent-iterations table and in `autorize status`. |
| `summary`                | string                | Optional model-written 1-2 sentence recap of what the iteration attempted and why the score moved, produced by the `[summarize]` step (§4). Empty when summarization is disabled, the iteration was a `noop`, or the step failed (best-effort). Surfaced in the next iteration's prompt under `## Recent attempt summaries` and in `autorize status` (`last summary`). `#[serde(default)]`, so pre-A2 logs without this field still load. |

`state.json` is a single `StateSnapshot` JSON object:

| Field                  | Type                  | Meaning                                                                                      |
|------------------------|-----------------------|----------------------------------------------------------------------------------------------|
| `experiment`           | string                | The experiment `name`.                                                                       |
| `branch`               | string                | The tracking branch (`autorize/<name>`).                                                     |
| `base_commit`          | string                | SHA at which the tracking branch was created. The loop refuses to continue if it is gone.    |
| `iter_in_progress`     | integer or null       | The in-flight iteration number, or `null` when idle between iterations.                      |
| `current_step`         | enum (string)         | One of the `CurrentStep` variants listed in section 3.                                       |
| `best_score`           | float or null         | Best score seen so far.                                                                      |
| `best_iter`            | integer or null       | Iteration number whose merge set `best_score`.                                               |
| `started_at`           | RFC3339 timestamp     | When the run loop first started this experiment.                                             |
| `deadline`             | RFC3339 timestamp     | Absolute UTC deadline computed from `schedule`.                                              |
| `iterations_completed` | integer (u64)         | Lifetime count of records in `iterations.jsonl` (every completed iteration, **including** reconciled `killed` records). Never reset. |
| `run_iterations_completed` | integer (u64)     | Iterations done in the *current* run; what `max_iterations` is checked against, and what `autorize run --fresh` resets to 0. A reconciled `killed` record does **not** bump it. State files predating this field migrate it to `iterations_completed` on load. |
| `consecutive_noops`    | integer (u32)         | Streak length of consecutive `noop` outcomes; resets on any non-noop.                        |

## 12. Pre-flight checks performed by `autorize run`

Before entering the loop, `autorize run`:

- Verifies the experiment directory exists (created by `autorize init`).
- Acquires an exclusive non-blocking advisory flock on
  `.autorize/<name>/run.lock`. A second concurrent `autorize run` on the
  same experiment is rejected immediately with the holder's pid for
  diagnostics. The kernel releases the lock automatically on process exit,
  so a crash leaves no stale lock to clean up.
- Verifies the current directory is a git repository.
- Verifies the working tree is clean (excluding the `.autorize/` directory
  and the `logs/` run log, which are always allowed to be dirty). Use
  `--allow-dirty` to bypass.
- If `state.json` exists, verifies `state.json.base_commit` is reachable
  in the current repo. If it is gone, the run aborts with an error.
- If `state.json` exists and has `iter_in_progress != null`, the run is
  refused with a message pointing at `autorize resume <name>`. `resume`
  records the in-progress iter as `outcome = "killed"`, clears the
  in-progress marker, and continues the loop.

`autorize run --allow-dirty <name>` overrides only the dirty-tree check.
All other pre-flight checks still apply. `autorize run --fresh <name>` resets
the run-level stop conditions on an existing, non-in-progress state (see §14);
it bypasses no pre-flight check — the in-progress refusal above still fires.

## 13. Walkthrough: `examples/pi-digits/`

A complete inline example. The fixture nudges the single floating-point
number in `value.txt` toward π (3.141592653589793) over a handful of
iterations.

### Scaffold

```sh
autorize init pi
```

Creates:

```
.autorize/pi/
  config.toml
  program.md
```

### Edited `config.toml`

```toml
[experiment]
name = "pi"
description = "Demo: nudge value.txt toward π."

[objective]
command = "bash score.sh"
direction = "min"
parse = { kind = "float" }
timeout = "30s"
fail_mode = "invalid"

[boundaries]
allow_paths = ["value.txt"]
deny_paths = [".autorize/**", "*.lock"]

[setup]
command = ""
timeout = "1m"

[teardown]
command = ""
timeout = "1m"

[iteration]
budget = "30s"
max_iterations = 6
keep_worktrees = false
max_consecutive_noops = 5

[schedule]
total_budget = "5m"

[agent]
command = "bash mock-agent.sh {iter}"
workdir_var = "AUTORIZE_WORKDIR"
stdin = "prompt"

[agent.env]
```

### `program.md`

```
# pi experiment

Your job is to nudge the single floating-point number in `value.txt` closer to
π (3.141592653589793).

Constraints:

- Only modify `value.txt`. Do not create or modify any other files.
- Do not touch anything under `.autorize/` — that is the harness's bookkeeping.
- Keep the file as a single line containing a decimal number followed by `\n`.

The harness scores each iteration by computing `|π − value|` (lower is better)
and keeps your edit only if the score improves over the best known so far.
```

### `autorize run pi` (sample output)

```
iter 1: merged    score=0.099201 best=0.099201
iter 2: merged    score=0.069441 best=0.069441
iter 3: merged    score=0.048608 best=0.048608
iter 4: discarded score=0.534008 best=0.048608
iter 5: merged    score=0.034025 best=0.034025
iter 6: merged    score=0.023818 best=0.023818
reached max_iterations=6; stopping.
---
experiment   pi
iterations   6
best         iter 6, score 0.023818
```

### Annotated `iterations.jsonl` line

```json
{
  "iter": 1,
  "started_at": "2026-05-20T08:00:00.000000Z",
  "ended_at":   "2026-05-20T08:00:01.234567Z",
  "outcome": "merged",
  "score": 0.099201,
  "best_so_far": 0.099201,
  "agent_exit": 0,
  "agent_killed_by_budget": false,
  "diff_lines": 4,
  "notes": "first valid score: 0.099201"
}
```

- `outcome: "merged"` means the diff was committed onto `autorize/pi`.
- `best_so_far` equals `score` because this is the first record.
- `agent_killed_by_budget: false` means the agent finished inside
  `iteration.budget`.
- `notes` carries the harness's reason for the outcome (here, the first valid
  score with no prior best to beat). Later iterations read like
  `"improved: 0.069441 from 0.099201"` or
  `"regressed: 0.534008 vs best 0.048608 (min)"`.

### `autorize status pi` (sample output)

```
experiment   pi
branch       autorize/pi
base_commit  abc1234deadbeef...
iterations   6
noop streak  0
last outcome merged
last reason   improved: 0.023818 from 0.034025
best         iter 6, score 0.023818
elapsed      1s
remaining    4m 58s
```

### Simulated crash + resume

Suppose the harness was killed mid-iter at iter 3. `state.json` looks like:

```json
{
  "experiment": "pi",
  "branch": "autorize/pi",
  "base_commit": "abc1234deadbeef...",
  "iter_in_progress": 3,
  "current_step": "InvokeAgent",
  "best_score": 0.069441,
  "best_iter": 2,
  "started_at": "2026-05-20T08:00:00Z",
  "deadline":   "2026-05-20T08:05:00Z",
  "iterations_completed": 2,
  "run_iterations_completed": 2,
  "consecutive_noops": 0
}
```

`autorize run pi` refuses with:

```
in-progress iteration found; use `autorize resume`
```

`autorize resume pi` records iter 3 as `outcome: "killed"`:

```json
{
  "iter": 3,
  "started_at": "2026-05-20T08:00:30.000000Z",
  "ended_at":   "2026-05-20T08:00:30.000000Z",
  "outcome": "killed",
  "score": null,
  "best_so_far": 0.069441,
  "agent_exit": null,
  "agent_killed_by_budget": false,
  "diff_lines": 0,
  "notes": "resumed after crash"
}
```

…and then continues the loop at iter 4 as if `autorize run` had been
invoked.

## 14. Starting another run with `autorize run --fresh`

Once a run finishes (deadline fired, `max_iterations` reached, or the
consecutive-noop streak hit), a plain `autorize run <name>` reloads the saved
`state.json` and immediately re-hits the same stop condition — it does no new
work. To start *another* run that builds on the prior best, pass `--fresh`:

```
autorize run <name> --fresh
```

`--fresh`:

- **Resets** the run-level stop conditions: recomputes `deadline` from
  `schedule` (a `total_budget` becomes `now + total_budget`; a relative or
  natural-language `deadline` recomputes), resets `run_iterations_completed`
  and `consecutive_noops` to 0, and refreshes `started_at`.
- **Preserves** everything else: `best_score`/`best_iter`, `base_commit`, the
  lifetime `iterations_completed`, the `autorize/<name>` branch and its tip,
  and every record in `iterations.jsonl`. New iterations keep comparing against
  the prior best and keep numbering strictly upward from the highest existing
  iter.

Rules:

- On a never-run experiment (no `state.json`) `--fresh` is a no-op: it behaves
  exactly like a normal first run.
- If an iteration is in progress (`iter_in_progress != null`), `--fresh` is
  refused with a pointer to `autorize resume` — it never discards real
  in-flight work.
- If the recomputed deadline is an already-past **absolute** RFC3339 instant,
  `--fresh` errors (`schedule.deadline "<value>" is in the past; …`) instead of
  entering a loop that exits immediately. Switch to `total_budget` or edit the
  deadline.

This is the supported way to "run it again"; deleting `state.json` by hand is
not (it would also drop `best_score`/`best_iter`, letting the next iteration
trivially "improve" against nothing).

## 15. Steering a live run with `autorize tell`

`autorize tell <name> <message>` lets an operator inject mid-run direction
without stopping the loop. It appends one structured entry to
`.autorize/<name>/guidance.jsonl`; because `tell` runs in a separate process
from the looping `autorize run`, coordination is purely file-based — the run
loop **re-reads `guidance.jsonl` at the top of every iteration** and renders all
entries into a prominent `## Operator guidance` section of the prompt (placed
just after the boundaries), framed as authoritative direction that takes
precedence over the general `program.md` instructions where they conflict.

```sh
autorize tell pi "stop tuning the Leibniz series — try a spigot algorithm"
```

The message appears in the **very next** iteration's prompt and persists in
every iteration thereafter. In v1 all entries are kept and shown every
iteration (no consumed/expiry/ack mechanism yet — the structured format leaves
room to add one later).

Each line of `guidance.jsonl` is one `GuidanceEntry` JSON object:

| Field           | Type              | Meaning                                                                                       |
|-----------------|-------------------|-----------------------------------------------------------------------------------------------|
| `ts`            | RFC3339 timestamp | When the guidance was recorded.                                                               |
| `added_at_iter` | integer or null   | Best-effort iteration the run was on when the entry was added (the in-flight iter, else the per-run completed count from `state.json`); `null` if the experiment had never run. Rendered as a `(since iter N)` hint. |
| `text`          | string            | The guidance shown to the agent.                                                              |

`guidance.jsonl` is also safe to **edit by hand** — add or remove lines and the
next iteration picks them up. A missing or empty file renders no section and is
not an error. (Unquoted trailing words on the `tell` command line are joined
with spaces, so `autorize tell pi do X` and `autorize tell pi "do X"` are
equivalent.)

---

End of `autorize llms` reference. Source-of-truth modules live under
`src/` (`src/config.rs`, `src/scoring.rs`, `src/schedule.rs`,
`src/agent.rs`, `src/storage.rs`, `src/iteration.rs`, `src/cli/run.rs`)
if you need to read the code.
