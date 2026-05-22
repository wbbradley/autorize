---
name: autorize
description: Set up and scaffold an autorize experiment in this repo — interview the user for the objective, scoring command, agent CLI, schedule, and boundaries, then draft `.autorize/<name>/{config.toml,program.md}` + any helper scoring script for review before writing. Use whenever the user wants to start, configure, or bootstrap an autorize run.
---

## 1. Load context first

Before asking the user anything, load the canonical reference:

```sh
autorize llms
```

Read the full output. **This is the single source of truth** for the
config schema, parse-kind variants, schedule fields, iteration knobs, and
on-disk layout. This skill body intentionally does not restate it — if
something in the schema is unclear, re-read the `autorize llms` output, not
this file.

If `autorize` is not on PATH (`command -v autorize` fails), tell the user how to install it by
pointing at the **Install** section of the https://github.com/wbbradley/autorize repo's `README.md`
(the prebuilt-binary instructions and `cargo install --path .` are both documented there) and
**stop**. Do not try to install `autorize` for the user.

## 2. Quick repo scan

Once `autorize llms` is loaded, take a short look at the repo so your
later proposals are grounded in what's actually there — not boilerplate.
A minute or two, not a deep audit: `ls` the top level + `git ls-files |
head -50`; read `README.md` and whichever of `Cargo.toml` /
`package.json` / `pyproject.toml` / `go.mod` exists; note any obvious
test/CI command (`cargo test`, `pytest`, `npm test`, a `Makefile` target,
`.github/workflows/*.yml`). The scan *informs* the interview, doesn't
pre-decide it.

## 3. Interview the user

Drive every structured/enum choice through `AskUserQuestion`. Free-text
answers (experiment name, improvement goal, scoring command) can be
gathered conversationally.

`AskUserQuestion` allows at most 4 questions per call. Suggested batches:

- **Batch A — objective shape**
  1. `objective.direction` — min vs max.
  2. `objective.parse.kind` — float / regex / jq.
  3. `objective.fail_mode` — invalid (default) / worst / abort.
  4. Does a scoring script already exist, or should one be drafted?

- **Batch B — schedule + iteration**
  1. `schedule` — `total_budget` (duration) vs `deadline` (absolute time).
  2. `iteration.budget` (per-iter wall-clock, humantime).
  3. `iteration.max_iterations` — `0` (unbounded) vs a cap.
  4. `iteration.max_consecutive_noops` — default `5` vs override.

- **Batch C — agent + boundaries**
  1. `agent.command` — default `claude --print {prompt_file}` vs custom.
  2. `agent.stdin` — `none` (use `{prompt_file}`) vs `prompt` (pipe stdin).
  3. `boundaries.deny_paths` — additions beyond the template default
     (`.autorize/**`, `*.lock`)?
  4. `iteration.keep_worktrees` — keep per-iter dirs for debugging?

Gather these free-text (not via `AskUserQuestion`):

- Experiment **`name`** — must match `[A-Za-z0-9_-]+`. Validate the user's
  answer before continuing.
- One-paragraph **improvement goal** — seeds `program.md`.
- **`objective.command`** shell string. If the user doesn't have one,
  offer to draft a small POSIX `score.sh` (see the pattern in §4).
- For `parse.kind = "regex"`, the **`pattern`** (first capture group is
  the score). For `parse.kind = "jq"`, the **`path`** (e.g. `.metrics.loss`).
- Either `schedule.total_budget` (humantime, e.g. `"4h"`) or
  `schedule.deadline` (RFC3339, humantime offset, or natural language —
  see `autorize llms` §8 for the accepted forms).

## 4. Propose, don't dictate

Use your own reasoning to suggest defaults grounded in what you saw in
the scan. Examples:

- Found `cargo test`? Propose `objective.command = "cargo test --quiet 2>&1
  | tail -1"` with a regex parse on the pass count.
- Found `pytest`? Propose `pytest -q | tail -1` with an analogous parse.
- Goal is improving a numeric metric in a tracked file? Draft a small
  POSIX `score.sh` that reads the file and prints a single float on
  stdout. Reference pattern (substitute the user's file/metric):

  ```sh
  #!/bin/sh
  set -eu
  v=$(cat value.txt)
  awk -v x="$v" 'BEGIN { target=3.141592653589793; d=x-target;
    if (d<0) d=-d; printf "%f\n", d }'
  ```

For `program.md`, draft content tailored to the stated goal — **not
boilerplate**. Guidelines: be specific about what counts as an
improvement (the objective command defines the score; this file is where
you tell the agent *why* that score exists and what kinds of changes tend
to move it). Call out files / directories to focus on or leave alone —
and note that `boundaries.deny_paths` is enforced (a diff touching any
deny pattern discards the iteration). Keep it short and dense; edit as
you learn what the agent keeps getting wrong.

When you offer a default, say whether it's your recommendation or a hard
requirement of the schema.

## 5. Draft and confirm before writing

Show drafts in chat as code blocks, in this order:

1. `config.toml`
2. `program.md`
3. Any helper script (e.g. `score.sh`)

Wait for a **single** explicit acceptance — don't re-confirm per file. On
acceptance:

1. Run `autorize init <name>` to create `.autorize/<name>/config.toml`
   and `.autorize/<name>/program.md` from the templates.
2. Overwrite both files with the accepted drafts (use `Write`, not `Edit`,
   since the template content is being replaced wholesale).
3. If a helper scoring script was drafted, write it at the repo root (or
   wherever the user prefers) and `chmod +x` it.

Validate by running `autorize status <name>` — it parses `config.toml`
and surfaces schema errors. **Do not** run `autorize run`. If validation
errors out, report the error verbatim to the user and stop.

## 6. Stop here

Tell the user the next steps explicitly:

- Commit the new `.autorize/<name>/` (or plan to use
  `autorize run --allow-dirty <name>`).
- `autorize run <name>` in another shell to start the loop.
- `autorize status <name>` for a one-shot summary;
  `autorize resume <name>` to recover after a crash or `Ctrl-C`.

The skill **does not** start the loop. End the conversation here.

---

**References used by this skill:**

- Schema & semantics — `autorize llms` (the only source of truth; the
  agent does not need to clone the autorize repo).
- Upstream repo (for the user, if they want install instructions, the
  worked `examples/pi-digits/` end-to-end demo, or the on-disk templates):
  <https://github.com/wbbradley/autorize>.
