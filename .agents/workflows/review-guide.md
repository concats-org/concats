# /review-guide — guided tour of a diff

Write a guided review — a "tour" — of a git diff for the concats-app review
app, following the repo's `review-guide` skill to the letter.

Steps:

1. Print the skill and read all of it before acting:
   `concats skill review-guide`
   If this fails ("command not found", or a usage dump), report the
   error verbatim and stop.
2. Follow the skill exactly, starting with its **Hard rules**: the repo is
   read-only to you (no git state changes, ever), the guide file is written
   outside the repo, and you only report "submitted" after `concats
   submit` printed success.
3. Range: arguments to this workflow are `[base] [head]` — pass them as
   `--base`/`--head` on every command. Without arguments, run every
   `concats` command with NO `--repo`/`--base`/`--head` flags: while
   the review app is open, bare commands target the diff its pane shows
   (the app publishes its live range), and anywhere else the CLI errors
   with exactly what to pass — relay that and ask; never invent a range.
