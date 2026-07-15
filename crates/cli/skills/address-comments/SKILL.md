---
name: address-comments
description: |
  Work through the review comments on a diff for the concats-app app: read
  every open thread (local ones, and ones imported from a GitHub pull request
  with `concats comments import`), verify each against the CURRENT code,
  fix what is still valid, and answer every thread in place with
  `concats comments reply` — so the reviewer meets your answer under
  their own comment, in the app and in the exported review.
  Works on commit ranges and on uncommitted work (INDEX/WORKTREE ranges).
  Triggers: "address the review comments", "fix the review feedback",
  "answer the PR comments", "work through the comments on this diff",
  "respond to the review".
metadata:
  argument-hint: "[base] [head]"
---

# Address Comments — answer the review, in the review

You are the author responding to a review. Every comment on this diff is a
question someone asked about the code; your job is to answer all of them —
by changing the code, or by saying why not — and to leave the answer **in
the thread**, not in your reply to the user. A thread whose last comment is
yours has been addressed; one that ends on someone else's comment has not.
That is the only "done" there is, and it is the same rule a human reader
applies.

Unlike this repo's other two review skills, you ARE working on the code
here. That makes the rules different, not looser.

## Hard rules — read first, break nothing

1. **You may edit the files under review. You may not touch git state.**
   No `add`, `commit`, `reset`, `checkout`, `restore`, `stash`, `clean`,
   `merge`, `rebase`, `push` — not to "save progress", not to "undo"
   something, not to get a clean tree. The reviewer is looking at the
   worktree you are editing; committing it out from under them, or resetting
   it, destroys the review. Leave the changes uncommitted and say what you
   changed. (`concats comments` is safe: it stores review data under
   `.git/` and never touches the worktree.)
2. **Write no new file inside the repo except the fix itself.** Scratch
   notes, plans and drafts go to `/tmp` or your scratch directory — a stray
   file inside the repo shows up in the very diff under review.
3. **Verify before you fix.** A comment describes the code as it was when it
   was written. Read the lines it anchors to *now*: it may already be fixed,
   or the code may have moved. An unfounded "fixed" is worse than "still
   valid, here is why I disagree".
4. **Never report success you did not see.** A thread is answered when its
   `comments reply` printed success. If a command errored, quote the error
   verbatim, stop, and ask — do not improvise recovery, and above all do not
   touch git state to "fix" anything.
5. **Answer every thread, including the ones you reject.** Silence reads as
   "missed it". A comment you will not act on gets a reply saying so and
   why.

## Rationalizations (do not skip)

| Rationalization | Why it's wrong |
|-----------------|----------------|
| "I'll summarize the fixes in my reply" | The reviewer reads the thread, not your chat log. Reply in the thread |
| "This nit isn't worth a reply" | An unanswered thread is an open thread. One line is enough |
| "The comment is obviously right, just fix it" | Comments go stale; check the current lines first |
| "I'll commit so the fixes aren't lost" | The reviewer is reading that worktree. Never commit |
| "I disagree, so I'll skip it" | Disagreement is an answer. Write it down |

## Workflow

Run from the repo under review. `<BASE> <HEAD>` is the range; pass the same
values to every command. The CLI resolves the range itself — you only pass
through what the user gave: (1) explicit arguments — e.g.
`/address-comments INDEX WORKTREE` — go on every command as
`--base`/`--head`; (2) no arguments — run every `concats` command
**without any range flags**: when the review app is open, the bare commands
target exactly the diff its pane shows (a running app publishes its live
range, following the pane across switches; the terminal's `CONCATS_APP_*`
env is the fallback); anywhere else the CLI errors and tells you what to
pass — relay that and ask. Never invent a range.

```sh
# 0. The conversation. If this errors ("command not found", or a usage
#    dump), the installed binary is missing or stale — report that verbatim
#    and STOP. (`concats --help` lists every command it has.)
concats comments export --repo . --base <BASE> --head <HEAD>
```

That is the whole input: every comment, grouped by file, in thread order,
each with its `id`, its author, and its lines. A reply repeats its root's
anchor and carries `reply-to=<id>` in the machine comment:

```markdown
## `src/lib.rs`

### L63-L68
<!-- concats-app id=7 author="octocat" at=1785402900 ref="github:2181234567" -->
this leaks on the error path

### L63-L68
<!-- concats-app id=12 author="claude" at=1785410000 reply-to=7 -->
fixed — the early return drops the guard now
```

**Thread 7 is closed** (its last comment is the agent's). A thread with no
reply, or whose last reply is not yours, is **open** — that is your work
list. `ref="github:…"` marks a comment that came from a pull request; it
reads and answers exactly like a local one.

```sh
# 1. For each OPEN thread, in file order. Read the anchored lines first —
#    the manifest's links give you the current hunk around them.
concats manifest --repo . --base <BASE> --head <HEAD>

# 2. The intent behind the code being questioned, when it matters: the
#    captured sessions say WHY the change was made.
concats turns --repo . --base <BASE> --head <HEAD>
```

Then, per thread: verify → act → answer.

```sh
# 3. Answer in the thread. <id> is any comment in it — replying to a reply
#    answers the thread, like GitHub. --author is your agent name.
concats comments reply <id> -m "<what you did, or why not>" --author <your-name>

# 3b. IF YOU CHANGED THE COMMENTED LINES, say where they are now. A comment is
#     anchored to content, so your fix changes the blob and strands the whole
#     conversation — it vanishes from the diff exactly because you resolved it.
#     A reply anchored on the fixed lines brings the thread there: every
#     comment keeps the lines it was written on, and a thread renders under
#     its newest comment the diff can place. Same grammar as `comments add`,
#     and the lines are the manifest's, never computed by hand.
concats comments reply <id> <path>:<start>[-<end>] \
    -m "fixed: <what you changed>" --author <your-name>
```

A thread you moved keeps reading in place. One you did not — because the lines
were deleted outright, or you declined the comment — stays where it was
written; the file card offers it under "N outdated conversations". Neither is
lost, but only the moved one is in front of the next reviewer.

Write the reply the way you would want to read it — what changed and where,
or what you are declining and why:

```
fixed: <what you changed, and where>. <why that closes it>
declined: <why the code is right as it stands> — <the evidence>
stale: already handled by <commit/line> before this review.
question: <what you need from the author before you can act>
```

Keep changes minimal and scoped to what the comment asked for. A comment
about error handling is not a licence to restructure the module — that is a
new review, not this one.

```sh
# 4. Close the loop: re-export and confirm every thread now ends on you.
concats comments export --repo . --base <BASE> --head <HEAD>
```

Then run the project's checks on what you changed (this repo:
`cargo +nightly fmt --check`, `cargo clippy -- -D warnings`, `cargo test`)
and report their real output. A fix that does not build is not a fix.

## Comments from a pull request

Review comments living on GitHub come in through the same store, so they
thread and answer like any other:

```sh
gh api repos/{owner}/{repo}/pulls/{n}/comments --paginate \
  | concats comments import - --repo . --base <BASE> --head <HEAD>
```

Import is idempotent — run it again to pick up new comments without
duplicating the ones already there. Threads GitHub can no longer place in
the diff (outdated ones) are reported and skipped; they are not in your work
list. Your replies stay local: nothing is pushed back to the pull request.

## Your reply to the user

Short, and honest about coverage:

- how many threads there were, how many you fixed, declined, or asked back on
- the files you changed, and the check output you actually saw
- anything you could not resolve, and what you need to resolve it

The threads carry the detail. Do not restate them.

## When NOT to use this skill

- **Explaining a diff** to a human reviewer — that is
  `concats skill review-guide`.
- **No comments on the range yet.** Say so and stop; there is nothing to
  address.
