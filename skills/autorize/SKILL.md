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

If `autorize` is not on PATH (`command -v autorize` fails), tell the user
how to install it by pointing at the **Install** section of this repo's
`README.md` (the prebuilt-binary instructions and `cargo install --path .`
are both documented there) and **stop**. Do not try to install `autorize`
for the user.

## 2. Quick repo scan

Once `autorize llms` is loaded, take a short look at the repo so your
later proposals are grounded in what's actually there — not boilerplate.
Aim for one or two minutes of looking, not a deep audit:

- `ls` the top level; `git ls-files | head -50` for the shape.
- Read top-level `README.md`, plus whichever of
  `Cargo.toml` / `package.json` / `pyproject.toml` / `go.mod` exists.
- Note any obvious test or CI command (`cargo test`, `pytest`, `npm test`,
  a `Makefile` target, `.github/workflows/*.yml`).

The point of the scan is to *inform* the interview, not pre-decide it.

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
  offer to draft a `score.sh` modeled on `examples/pi-digits/score.sh`.
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
- Goal is improving a numeric metric in a tracked file? Mirror the
  `examples/pi-digits/score.sh` pattern (small POSIX shell script that
  reads the file and prints a single float).

For `program.md`, draft content tailored to the stated goal — **not
boilerplate**. Follow the tips embedded in `src/templates/program.md.tmpl`:
be specific about what counts as an improvement, call out files to focus
on / leave alone, mention that `boundaries.deny_paths` is enforced (a diff
touching them discards the iteration), keep it short and dense.

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

- Schema & semantics — `autorize llms` (canonical) or `src/llms.md`.
- Templates being overwritten — `src/templates/config.toml.tmpl`,
  `src/templates/program.md.tmpl`.
- Worked example to mirror for `score.sh` — `examples/pi-digits/score.sh`.
