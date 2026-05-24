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

- **[usability/medium] Clean end-of-run repo state: non-mutating diff capture, `autorize clean`, and full file/child logging** — A real completed run (`~/src/ls-py-run-handler/.autorize/runs-perf/`) left the repo confusing-looking even though the result was actually correct, and exposed three genuine defects. **Forensic context the implementer must understand first (the end-state was benign, so don't "fix" the parts that work):** the `autorize/<name>` branch already ends as a clean linear *stack* of one real commit per accepted iteration (commit in a detached worktree via `commit_all_in`, then `git update-ref` advances the branch — src/iteration.rs:142-153, src/worktree.rs:105-109,195-210); the branch tip already equals `state.json`'s `best_iter`; and v0.2.4+ already creates every iteration worktree with `git worktree add --detach` (src/worktree.rs:90-97), so new runs never check the tracking branch out in a worktree. The observed "branch checked out in iter-0001/wt + staged-but-uncommitted diff" was residue from a pre-v0.2.4 binary (no `--detach`) combined with the ref being advanced out from under that stale checkout; the staged diff there was byte-identical to the already-committed iter-1 and represented **no** lost work. With that understood, the three things to actually fix:

  **Part A — diff capture must not leave a misleading staged index on non-merged worktrees.** Today the CaptureDiff step calls `stage_all_in` = `git add -A` unconditionally (src/iteration.rs:91-95, src/worktree.rs:176-179) purely so the diff/deny-path scan can see untracked files, but it is **never unwound** for `discarded`/`invalid`/`noop`/`killed` outcomes. When `keep_worktrees = true`, those kept worktrees (the user's intended "records of in-progress work") are therefore left with a fully-staged `git add -A` index instead of a clean dirty checkout — the real source of the "why are there staged diffs that aren't committed?" confusion. **Locked design:** decouple capture-staging from commit-staging. After capturing the diff/deny-scan, **unstage** (`git reset` mixed, or capture via intent-to-add `git add -N` so content is never staged in the first place) so any non-merged worktree reads as an ordinary *unstaged* dirty checkout. This is provably safe because the accept path (`commit_all_in`, src/worktree.rs:195-210) does its **own** `git add -A` immediately before committing — it does not depend on the capture-time staging — so a merged iteration still commits the full agent diff. (This decoupling was the user's explicit concern: "what if the work does get committed?" → it still does; the commit re-stages independently.)

  **Part B — add `autorize clean <name>` (a.k.a. finalize) subcommand.** New `src/cli/clean.rs`, wired into the clap dispatch in `src/cli.rs` (follow the existing `init`/`run`/`status`/`resume` pattern). It tidies a finished/abandoned experiment without destroying the historical record:
    - If `autorize/<name>` is currently checked out in *any* worktree (the pre-v0.2.4 residue case), detach that worktree (`git worktree add --detach`-style re-point, or remove it) so the branch becomes freely checkout-able from the main repo. This is the one place that still must handle the "branch held by a worktree" condition (the run/resume pre-flight guard for it is **intentionally deferred** — see below).
    - Unstage (`git reset`) any kept iteration worktree that still carries a stale `git add -A` index from older binaries (the Part A residue for pre-existing runs).
    - `git worktree prune` to clear registrations for `wt/` dirs that no longer exist.
    - **Default: preserve the kept worktree checkouts and all per-iter artifacts** (`iter-NNNN/{prompt.md,agent.stdout,agent.stderr,changes.diff}`) — they are the "records of ongoing work" the user wants to keep. Provide an opt-in flag (e.g. `--remove-worktrees`) to also delete the `wt/` checkouts and reclaim disk. Never touch `iterations.jsonl` / `state.json`.
    - Running it on `~/src/ls-py-run-handler` must free `autorize/runs-perf` (so plain `git checkout autorize/runs-perf` works) and prune the 15 dangling worktree registrations, while leaving the commit stack and records intact.

  **Part C — logging: `println!` → `tracing`, append to `logs/autorize.log`, and tee child stdio.** Today the `tracing` subscriber is initialized for stderr only (src/main.rs:18-21) but **nothing emits through it** — every user-facing line is `println!` (src/cli/run.rs, src/cli/init.rs, src/subproc.rs), so a file sink alone would be near-empty. **Locked design:** (1) replace the `println!` call sites with appropriate `tracing` macros (`info!`/`warn!`/`error!`) so the run narrative (iteration start, capture, score, merge/discard, cleanup, final summary) flows through `tracing`; (2) layer a file sink alongside the existing stderr layer that **appends** to a project-root-relative `logs/autorize.log` — use `tracing-appender` (`cargo add tracing-appender`; `rolling::never("logs","autorize.log")` + `non_blocking`, holding the `WorkerGuard` for the lifetime of `main`) if that's the idiomatic route, otherwise a `tracing_subscriber` custom `MakeWriter` over `OpenOptions::append`; (3) the child processes spawned in `src/subproc.rs` (setup/agent/scoring/teardown) must **effectively `tee -a`** their stdout+stderr into `logs/autorize.log` in addition to the existing per-iter `agent.stdout`/`agent.stderr` capture files, so the central log contains the complete picture including subprocess output. Create `logs/` with `create_dir_all` on startup; add `logs/` to the project's `.gitignore` and to the `init` template's generated `.gitignore` if applicable.

  **Explicitly deferred (user decision):** a run/resume **pre-flight guard** that detects/auto-heals a tracking branch held by a worktree is *not* in scope — `--detach` already prevents the condition for new runs, and `autorize clean` (Part B) handles the legacy residue.

  **Acceptance:** (a) after a run with `keep_worktrees = true`, a discarded/killed iteration's kept worktree shows its work as **unstaged** working-tree changes with an empty `git diff --cached` (no stray staged index); (b) an *accepted* iteration still produces a commit containing the full agent diff (Part A's unstaging does not regress merge content — verify the merged commit's tree matches the agent's changes); (c) `autorize clean runs-perf` run in `~/src/ls-py-run-handler` makes `git checkout autorize/runs-perf` succeed, prunes dead worktree registrations, and leaves the 11-commit stack + records intact; (d) `--remove-worktrees` additionally deletes the `wt/` checkouts; (e) after any run, `logs/autorize.log` exists at the project root, is opened in append mode (a second run extends rather than truncates it), and contains both autorize's own iteration narrative and the teed child-process stdout/stderr; (f) no remaining `println!` for user-facing output in `src/` (grep clean); (g) `logs/` is gitignored.

- **[feature/correctness-high] Multi-dimensional scoring with baseline normalization** — autorize today scores only agent-modified worktrees and never the pristine tree, so `best_score` starts `None` and the decision `(None, _) => true` (src/iteration.rs:138, mirrored at src/cli/run.rs:332) unconditionally merges the *first* valid iteration even if it regresses against the starting state — contradicting "keeps improvements, discards regressions" (README.md:18, PLAN.md:6). Separately, real objectives are multi-dimensional (e.g. four latency benchmarks) and must be normalized against a per-dimension baseline so each dimension is optimized equally. Users currently hand-roll this entirely in `objective.command` (see the reference pattern at `~/src/ls-py-run-handler/.autorize/runs-perf/`: `bench-baseline.sh` writes a flat `baseline.json` map once on pristine code, deny-enforced; `score.sh` emits the scalar `Σ (median+2·IQR)/baseline`). Make this first-class:

  **Design (locked with user):**
  - **Multi-dimensional benchmark output, treated as such.** Add a parse kind that yields a *map* of named dimensions (`name → f64`), e.g. `parse = { kind = "json_map", path = ".benchmarks" }`, alongside the existing scalar `float`/`regex`/`jq` kinds (which must keep working unchanged). Touch `src/scoring.rs`, `src/config.rs` (`Objective`/parse enum).
  - **Baseline captured once against the pristine tree, stored, and deny-enforced.** Add an `autorize baseline <name>` subcommand (new `src/cli/baseline.rs`, wired in `src/cli.rs`) that builds a worktree at `base_commit`, runs `objective.command`, and writes the dimension map to `.autorize/<name>/baseline.json`. Auto-add `baseline.json` to enforced deny-paths so the agent can't tamper with it. `autorize run` pre-flight **aborts** if the baseline is missing or fails to score (user decision), rather than falling back to `None`.
  - **autorize provides stock scalarizers** selecting how the dimension vector + baseline collapse to the single comparable `f64` the decision/log use. Add `[objective.scalarize] = { kind = "..." }` with at least:
    - **S1 — per-dimension average improvement** (reduces to a single ratio for 1-D): mean over dimensions of the direction-aware improvement ratio vs baseline.
    - **S2 — baseline-normalized sum**: `Σ (value_i / baseline_i)` (the ls-py pattern; pristine ≈ N).
    - **S3 — TBD** (candidates to evaluate: weighted sum, geometric mean, worst-dimension/`max`). Leave the enum extensible.
  - Per-dimension data and the scalarized value flow into `IterationRecord` and `state.json` (src/storage.rs) and are surfaced by `autorize status` (src/cli/status.rs), which should also show the baseline distinctly from current best.

  **Open questions to resolve during implementation:**
  1. **Direction granularity** — single global `objective.direction` (assumes all dims share a direction, as ls-py does) vs. per-dimension direction (needed for mixed latency-down/throughput-up objectives). This determines what "improvement" means in S1 and whether S2's `value/baseline` is sound.
  2. Final choice for **S3** (and whether per-dimension **weights** are a separate concern layered on any scalarizer).
  3. Whether a missing/extra dimension at score time is **invalid** (ls-py treats a missing baseline benchmark as invalid) vs. another policy.
  4. Baseline **staleness**: should `run` detect that `baseline.json` was captured at a different `base_commit` and refuse/warn?

  **Acceptance:** (a) a multi-dim `objective.command` + a captured baseline run end-to-end; (b) a first iteration that is worse than pristine under the chosen scalarizer is `discarded`, not merged; (c) `autorize run` aborts with a clear error when no baseline exists; (d) `baseline.json` is deny-enforced (an iteration touching it is `denied`); (e) existing scalar-`parse` experiments keep working with no config change; (f) `status` shows baseline vs. best; (g) the ls-py `runs-perf` normalization is reproducible via stock S2 with no hand-written aggregation in `score.sh`; port it (or a slimmed copy) to `examples/` and document the facility in `README.md` + `src/llms.md`.

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
