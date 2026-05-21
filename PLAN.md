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
Parallel iterations · Pareto scoring · web/TUI · allow-path *enforcement* (allow_paths is
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
| Platform | Linux + macOS (aarch64) supported |

---

## Next Up

- **[consistency/low-medium] `iterations_completed` incremented for resumed `killed` records** — `record_killed` does `state.iterations_completed += 1`, so a `max_iterations = 10` budget loses one slot per crash. Either don't count killed records, or document the semantics. (src/cli/run.rs:235)
- **[consistency/low] `iter_in_progress` not cleared on `fail_mode = "abort"`** — When `apply_fail_mode` returns `ScoreDecision::Abort`, `run_iteration` returns `Err` while state still has `iter_in_progress = Some(N)`. Resume then masks the deliberate abort as a crash (`outcome: killed`, `score: null`, `notes: "resumed after crash"`). Record an `aborted` outcome before propagating the error. (src/iteration.rs:128-130)
- **[consistency/low] `CurrentStep::Discard`, `CheckDeadline`, `Done` declared and documented but never written** — The enum variants exist and `src/llms.md` documents them as valid `current_step` values an agent might observe, but no code path ever assigns them. Either wire them up or remove from the enum + docs. (src/storage.rs:35-41, src/llms.md:50-75)
- **[clarity/low] `max_consecutive_noops = 0` exits immediately** — `state.consecutive_noops (0) >= 0` is true at loop entry, so the loop exits without running any iteration. Either reject 0 in `Config::validate()` or treat as "disabled". (src/cli/run.rs:141-147, src/config.rs)
- **[security/low-medium] Shell injection via project path in templated commands** — `agent::substitute()` does literal `str::replace` of `{prompt_file}`, `{workdir}`, `{iter}` into a string that's then run via `bash -lc`. If `project_root` contains shell metacharacters (spaces, backticks, `$(...)`), bash re-parses them. Shell-quote substituted paths. (src/agent.rs:28-58, src/subproc.rs:44)
- **[performance/low] `bash -lc` (login shell) sourced on every subprocess** — Every setup/agent/scoring/teardown spawn uses `bash -lc`, sourcing `~/.bash_profile`/`~/.profile` per invocation. Also a reliability hazard: any rc-file output leaks into captured stdout/stderr. Use `bash -c` unless login behavior is intentional. (src/subproc.rs:44)
- **[correctness/low] Tracking branch not re-verified on resume** — Only `base_commit` is checked reachable on resume; if the user deleted `refs/heads/autorize/<name>`, the next `git worktree add` fails with a confusing error. Add `git.branch_exists(&branch)` to the resume pre-flight. (src/cli/run.rs:102-119)
- **[data-integrity/low] Newly-created `iterations.jsonl` is not directory-fsynced** — `append_iteration` opens with `create(true).append(true)` and `f.sync_all()`s the file but never fsyncs the parent directory after the file is first created. Power loss after the first append can lose the dirent. (src/storage.rs:86-93)
- **[operational/low] Corrupt mid-file line in `iterations.jsonl` is unrecoverable** — Only the final line is droppable on parse error; a corrupt mid-file line makes the whole experiment unreadable for `run` / `status` / `resume`. Add a skip+warn or `.bak` quarantine path. (src/storage.rs:95-112)
- **[data-integrity/low] Stale `iterations.jsonl` reused after manual `state.json` delete** — If the user deletes only `state.json` to "start over", `read_iterations` still loads old records, `next_iter_number` picks up from the old max, and `best_score = None` causes the next iter to trivially "improve". Either check both files for consistency on fresh-state init or document the rm-rf recipe. (src/cli/run.rs:72-101, 124)
- **[operational/low] `keep_worktrees = true` accumulates `.git/worktrees/` entries forever** — No built-in pruning or `git worktree prune` ever issued; long experiments end up with hundreds of entries slowing every git operation. Document a housekeeping recipe or add a prune step. (src/iteration.rs:158-160, src/worktree.rs)
- **[clarity/low] Final summary suppressed on any iteration error** — If `run_iteration` returns `Err`, `?` propagates and `print_final_summary` is never reached, so the user loses visibility into completed iters and best score so far. Catch, print summary, then propagate. (src/cli/run.rs:169-188)
