# /address-comments — answer the review, in the review

Work through the review comments on a git range: verify each against the
current code, fix what is still valid, and answer every thread in place,
following the repo's `address-comments` skill to the letter.

Steps:

1. Print the skill and read all of it before acting:
   `concats skill address-comments`
   If this fails ("command not found", or a usage dump), report the
   error verbatim and stop.
2. Follow the skill exactly, starting with its **Hard rules**: you may edit
   the files under review but never touch git state (no `add`, `commit`,
   `reset`, `checkout`, `stash` — the reviewer is reading that worktree),
   scratch files live outside the repo, every comment is verified against
   the current lines before you act on it, every thread gets an answer
   including the ones you reject, and a thread counts as answered only
   after `concats comments reply` printed success.
3. Range: arguments to this workflow are `[base] [head]` — pass them as
   `--base`/`--head` on every command. Without arguments, run every
   `concats` command with NO `--repo`/`--base`/`--head` flags: while
   the review app is open, bare commands target the diff its pane shows
   (the app publishes its live range), and anywhere else the CLI errors
   with exactly what to pass — relay that and ask; never invent a range.
