# PLAN.md

## v1 Design (locked, shared context for all phases)

`autorize` is a generic iterative-improvement harness that runs an external agent CLI in sandboxed
git worktrees against a user-defined scoring command, keeping improvements and discarding regressions
until a deadline fires. Generalizes Karpathy's `autoresearch` pattern
(https://github.com/karpathy/autoresearch) so any project can drop one in.

### Scope (v1)

**In scope**
- Single binary crate, four subcommands: `init`, `run`, `status`, `resume`.
- Git-worktree-per-iteration sandboxing against an `autorize/<name>` tracking branch.
- Generic subprocess agent invocation via a templated `agent.command`.
- Per-iteration wall-clock kill + total deadline (duration or absolute time).
- Score parsing: raw float, regex capture group, or `jq`-style JSON path.
- Durable append-only iteration log (`iterations.jsonl`) + atomic `state.json` checkpoint.
- Deny-path enforcement: reject iterations whose diff touches `boundaries.deny_paths`.
- End-to-end demo example under `examples/pi-digits/`.

**Deferred (NOT in v1)**
Parallel iterations · Pareto scoring · web/TUI · macOS · allow-path *enforcement* (allow_paths is
prompt-only in v1) · token accounting · retry/backoff · remote storage · hot reload · log pruning
· README/docs site.

### Crate layout (single binary, `module.rs` + `module/submodule.rs`)

```
autorize/
  Cargo.toml
  src/
    main.rs              # entry point, wires clap -> cli dispatcher
    cli.rs               # clap derive structs, subcommand dispatch
    cli/
      init.rs            # `autorize init <name>`
      run.rs             # `autorize run <name>`
      status.rs          # `autorize status <name>`
      resume.rs          # `autorize resume <name>`
    config.rs            # TOML schema, serde structs, validation
    experiment.rs        # top-level Experiment struct, paths, lifecycle
    iteration.rs         # single iteration state machine
    scoring.rs           # run objective.command, parse score (float/regex/jq)
    worktree.rs          # git worktree create / merge / discard / cleanup
    schedule.rs          # per-iter budget + total deadline, time math
    agent.rs             # spawn agent subprocess, capture stdio, enforce budget
    storage.rs           # iterations.jsonl append + state.json atomic write
    prompt.rs            # build the agent prompt from program.md + history
    templates.rs         # embedded default config.toml + program.md templates
    error.rs             # crate-wide Error / Result via anyhow + thiserror
  examples/
    pi-digits/
      value.txt
      score.sh
      .autorize/pi/
  tests/
    e2e_pi.rs
```

### Dependencies (use `cargo add`, latest versions)

`clap` (derive) · `serde` · `serde_json` · `toml` · `anyhow` · `thiserror` · `tracing` ·
`tracing-subscriber` · `humantime` · `chrono` · `regex` · `jaq-core` + `jaq-std` (or
`serde_json_path`) · `tempfile` · `nix` (Linux signals/process groups) · `globset` · `which`.
Shell out to `git` via `std::process::Command` — no full git library.

### Config schema (`.autorize/<name>/config.toml`)

```toml
[experiment]
name = "pi"
description = "..."

[objective]
command = "bash score.sh"
direction = "min"                    # "min" | "max"
parse = { kind = "float" }           # or regex/jq variants below
timeout = "60s"
fail_mode = "invalid"                # "invalid" | "worst" | "abort"

# parse variants:
#   { kind = "float" }
#   { kind = "regex", pattern = "score=([0-9.]+)" }
#   { kind = "jq", path = ".metrics.bpb" }

[boundaries]
allow_paths = ["src/**/*.py", "README.md"]   # prompt-only in v1
deny_paths  = [".autorize/**", "*.lock"]     # ENFORCED in v1

[setup]
command = ""
timeout = "5m"

[teardown]
command = ""
timeout = "1m"

[iteration]
budget = "5m"
max_iterations = 0
keep_worktrees = false
max_consecutive_noops = 5

[schedule]
total_budget = "4h"
# OR (exactly one):
# deadline = "2026-05-21T09:00:00-07:00"

[agent]
command = "claude --print {prompt_file}"   # {prompt_file}, {workdir}, {iter} substituted
workdir_var = "AUTORIZE_WORKDIR"
env = { ANTHROPIC_API_KEY = "$ANTHROPIC_API_KEY" }
stdin = "none"                              # "none" | "prompt"
```

`program.md` lives alongside as freeform agent instructions.

**Prompt built per iteration:** full `program.md` · boundaries section (human-readable) ·
compact table of last 10 iter records · best iteration's full diff (if any) · current iter
number + budget.

### Iteration state machine

```
Idle
 -> AllocateIter      mkdir iter-NNNN/
 -> CreateWorktree    git worktree add ... autorize/<name>
 -> RunSetup          setup.command in wt
 -> BuildPrompt       render -> iter-NNNN/prompt.md
 -> InvokeAgent       spawn agent.command; HARD KILL at iteration.budget
                      (SIGTERM process group, 5s grace, SIGKILL)
 -> CaptureDiff       git -C wt diff autorize/<name> > iter-NNNN/changes.diff
                      empty -> "noop", discard; touches deny_paths -> "denied", discard
 -> RunTeardown       teardown.command
 -> Score             objective.command -> parse -> Option<f64>; on fail -> fail_mode
 -> Decide            improved? Merge : Discard
 -> Merge             commit on autorize/<name>, ff to tracking branch
 -> Discard / Cleanup remove worktree (unless keep_worktrees)
 -> Record            append IterationRecord (fsync) + atomic state.json rewrite
 -> CheckDeadline     deadline | max_iterations | consecutive_noops >= max => Done
                      else AllocateIter
```

**IterationRecord (jsonl):**
```json
{"iter":7,"started_at":"...","ended_at":"...",
 "outcome":"merged|discarded|noop|invalid|killed|denied",
 "score":3.14159,"best_so_far":3.14159,
 "agent_exit":0,"agent_killed_by_budget":false,
 "diff_lines":42,"notes":""}
```

**Wall-clock kill (Linux):** `setsid`/`setpgid` per spawn; on timeout `killpg(SIGTERM)`, 5s
grace, `killpg(SIGKILL)`.

### Durability & resume

`state.json` (atomic tmp+rename):
```json
{"experiment":"pi","branch":"autorize/pi","base_commit":"abc...",
 "iter_in_progress":7,"current_step":"InvokeAgent",
 "best_score":3.14159,"best_iter":5,
 "started_at":"...","deadline":"...","iterations_completed":6,
 "consecutive_noops":0}
```

Resume: in-progress iter treated as abandoned -> remove worktree, append `outcome:"killed"`,
continue at `iter+1`. `iterations.jsonl` is source of truth; rebuild cached state from it on
disagreement. Tolerate torn last line. Refuse if `base_commit` missing.

### Pre-flight checks at `autorize run`

- Repo must be git.
- Repo must be clean (or `--allow-dirty`).
- `autorize/<name>` branch + `base_commit` reachable.
- `objective.command` parses as shell.
- Exactly one of `total_budget` / `deadline` set.

### Resolved design decisions (locked)

| Decision | Resolution |
|---|---|
| Sandboxing | Git worktree per iteration off `autorize/<name>` |
| Agent invocation | Generic subprocess via templated `agent.command` |
| Work area | `.autorize/<name>/` inside the project |
| Scheduling | Hard per-iter wall-clock + total deadline (duration or absolute) |
| Prior-iter context | Last 10 records (compact) + best iter's full diff |
| Boundary visibility | Prompt-only (no in-tree boundary file) |
| Objective failure | Default `"invalid"`: discard, don't count |
| Boundary enforcement | `deny_paths` enforced via diff; `allow_paths` prompt-only |
| Agent templating | Both `{prompt_file}` and stdin piping (`agent.stdin`) |
| Branch base | `HEAD` at `init`; record `base_commit`; refuse if gone |
| Dirty tree | Refuse; `--allow-dirty` overrides |
| Noop policy | Record `"noop"`; abort after 5 consecutive (configurable) |
| Log retention | Retain all in v1; pruning deferred |
| Platform | Linux-only v1 |

---

## Next Up

### Phase 6 — Examples + e2e tests + polish

Build `examples/pi-digits/` (a `value.txt` with `"3.0"`, a `score.sh` printing `abs(pi - value)`,
a pre-populated `.autorize/pi/` config, and a mock "agent" script that inches `value.txt` toward
π using iter env as a hint, occasionally regressing). Add `tests/e2e_pi.rs` running the full
loop. Add deny-path enforcement test and dirty-tree refusal test.

**Acceptance:** all of the v1 acceptance criteria from the original spec, run headlessly:
- ≥3 iterations, ≥1 merge, ≥1 discard, final value closer to π than start.
- Mid-run kill + resume continues cleanly.
- Mock agent editing `.autorize/state.json` -> iteration outcome `"denied"`, file unchanged on
  tracking branch.
- `autorize run` on dirty repo exits non-zero; `--allow-dirty` succeeds.

---
