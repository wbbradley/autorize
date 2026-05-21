# pi experiment

Your job is to nudge the single floating-point number in `value.txt` closer to
π (3.141592653589793).

Constraints:

- Only modify `value.txt`. Do not create or modify any other files.
- Do not touch anything under `.autorize/` — that is the harness's bookkeeping.
- Keep the file as a single line containing a decimal number followed by `\n`.

The harness scores each iteration by computing `|π − value|` (lower is better)
and keeps your edit only if the score improves over the best known so far.
