---
name: review-guide
description: |
  Write a guided review — a "tour" of a git diff — for the concats-app app:
  a markdown document whose lone-line links transclude the real diff, ordered
  by concern instead of by path, with context and review guidance at every stop.
  Works on commit ranges and on uncommitted work (INDEX/WORKTREE ranges).
  Triggers: "write a review guide", "guide me through this diff",
  "create a review tour for <base>..<head>", "organize this review",
  "guide me through my uncommitted/unstaged changes".
metadata:
  argument-hint: "[base] [head]"
---

# Review Guide — a tour of a diff

You are the tour guide, not the transcriptionist. Your job is to walk a
reviewer through a change in the order that makes it easiest to judge —
riskiest first, grouped by concern rather than by path — and to say, at every
stop, why the code exists and what deserves scrutiny. The app renders your
guide with the actual diff spliced in at your links; the reviewer ticks hunks
off and leaves line comments in place.

Your metric is the reviewer's **cognitive load**: at every moment they
should be holding one new idea, not a wall of diff. Everything below — the
ordering, the small links, the interleaved prose — serves that.

## Hard rules — read first, break nothing

You are REVIEWING this repo, not working on it. The diff — including any
uncommitted worktree state — IS the artifact under review. These rules are
absolute; breaking them destroys the thing you were asked to review:

1. **The repo is read-only to you.** Never run a git command that changes
   state: no `add`, `commit`, `reset`, `checkout`, `restore`, `stash`,
   `clean`, `merge`, `rebase`, `push` — not on any branch, not "to undo"
   something, not to "get a clean state". The only git you need is
   read-only: `git log`, `git diff`, `git show`. (`concats` commands
   are safe: they store review data under `.git/` and never touch the
   worktree.)
2. **Write your guide file OUTSIDE the repo** — `/tmp/review-guide.md` or
   your scratch directory. A file created inside the repo appears in the
   very diff you are reviewing.
3. **Never report success you did not see.** "Submitted" means the
   `concats submit` command printed its success output in front of you.
   If it errored or you never ran it, say exactly that.
4. **When a command fails: stop, quote the error verbatim, ask.** Do not
   improvise recovery, and above all do not touch git state to "fix" it.
   A failed submit is a sentence in your reply; a `git reset --hard` is a
   destroyed worktree.

## The contract (what makes a guide trustworthy)

1. **Never write out code.** Every code block is a *reference*: a markdown
   link, **alone on its own line**, copied verbatim from the manifest. The app
   renders the bytes from git — you cannot fabricate, tidy, or truncate a
   diff, only point at it. Do not paste snippets, not even short ones.
2. **You cannot hide anything.** Hunks you never reference are appended under
   "Not discussed", and `lint` measures your coverage in changed lines. You
   may reorder and explain a diff; you may not shrink it.
3. **Never author coordinates.** Line arithmetic is where reviews go subtly
   wrong. Copy link lines from `manifest` (or from `lint`'s misses list);
   never compose a `#L12-34` range by hand.
4. A link **inside a sentence, list item, or table cell is just a link** — it
   transcludes nothing, and a repo `file://` link that is not alone on its
   line is flagged by lint. Keep transclusions on their own lines; keep
   tables and prose free of `file://` links.

## Where the range comes from

The CLI resolves the range itself — your job is only to pass through what
the user gave you:

1. **Arguments.** Invoked with them (`/review-guide INDEX WORKTREE`,
   `/review-guide HEAD~5 HEAD`), pass exactly those as `--base <BASE>
   --head <HEAD>` on every command.
2. **No arguments.** Run every `concats` command **without any
   `--repo`/`--base`/`--head` flags**. When the review app is open, the
   bare commands target exactly the diff its pane shows (a running app
   publishes its live range; the terminal's `CONCATS_APP_*` env is the
   fallback); anywhere else the CLI errors and tells you what to pass. If
   it errors: relay the message and ask the user for the range. Never
   invent one.

This is the tightest loop: the reviewer opens the app's terminal, starts an
agent there, and says `/review-guide` — the bare commands pick up the open
range, and a `submit` lands in the app within a second. The running app
republishes its open range on every load, so bare commands follow the pane
even when the reviewer switches ranges mid-session.

`<BASE>`/`<HEAD>` take any rev — plus two sentinels for work in progress:
`INDEX...WORKTREE` is the unstaged changes, `HEAD...WORKTREE` everything
uncommitted (see "Uncommitted changes" below).

## Workflow

Run from the repo under review. The commands below show explicit
`--base <BASE> --head <HEAD>` for the with-arguments case; without
arguments, drop the range flags entirely (per above) and let the CLI
resolve or complain. If ANY step fails, apply Hard rule 4: stop, quote,
ask.

```sh
# 0. Preflight — prove the CLI answers before investing any work.
#    "command not found", or a usage dump naming commands this skill does
#    not use, means the installed binary is missing or stale. Report that
#    and STOP; nothing below can succeed, and improvising around it is how
#    worktrees die. (`concats --help` lists every command it has.)
concats manifest --repo . --base <BASE> --head <HEAD> >/dev/null && echo PREFLIGHT-OK

# 1. The hunks to choose from — every link, ready to copy, with previews
concats manifest --repo . --base <BASE> --head <HEAD>

# 2. The captured intent — concats sessions/turns linked to this range
concats turns --repo . --base <BASE> --head <HEAD>

# 3. Write the guide OUTSIDE the repo (Hard rule 2), structure below, then
#    close the loop until clean. lint tells you exactly what you broke and
#    what you missed — with ready-made links for the misses, so a fix is a
#    paste, not arithmetic.
concats lint /tmp/review-guide.md --repo . --base <BASE> --head <HEAD> --min-coverage 80

# 4. Submit — reruns the lint (and refuses to store a failing guide), then
#    saves the guide locally under .git/ (ephemeral, never pushed). A review
#    app open on this range picks it up within a second; otherwise it loads
#    the next time this range is opened. Newest submission wins.
#    --author is your agent name (claude, gemini, codex, …).
concats submit /tmp/review-guide.md --repo . --base <BASE> --head <HEAD> --min-coverage 80 --author <your-name>

# 5. Only after step 4 printed success: tell the user it is submitted.
#    If it failed instead: quote the error, keep the guide file path handy,
#    and stop — the guide is not lost, and git is not the fix (Hard rule 1).

# Local iteration alternative: open a guide file directly — an explicit
# --script always wins over submitted guides.
#    concats-app --script /tmp/review-guide.md <repo> <BASE> <HEAD>
```

Read the manifest previews and the `turns` output before writing anything:
the plan of the tour — what groups with what, what leads — is the actual
work. When transcripts exist, they are your source for *intent*; treat them
like the linked ticket in a classic PR review.

## Uncommitted changes (WORKTREE ranges)

The same workflow reviews work that has not been committed yet:

- `--base INDEX --head WORKTREE` — the unstaged changes (what `git diff` shows).
- `--base HEAD --head WORKTREE` — everything uncommitted (`git diff HEAD`).
  Any commit rev works as the base.

`manifest`, `lint`, `submit`, and `comments add` all work unchanged. Three
differences to respect:

1. **No commits, no sessions.** `turns` has nothing to link (sessions link
   to commits). Skip the intent table unless you have another source of
   intent — the conversation you are in often is one.
2. **The diff moves under you.** The worktree changes as the author works.
   Run `manifest` fresh, write the guide, and `lint`/`submit` immediately.
   A link into content that has since changed is flagged loudly in the app —
   never silently dropped — and a comment anchored to edited lines stops
   rendering until the content matches again. If `lint` reports broken links
   you did not break, re-run `manifest`: the file moved.
3. **Newest worktree guide wins.** Worktree guides are not pinned to
   resolved commits; the app applies the newest one submitted for this
   repo's worktree and reloads the review automatically as files change.

The seen ticks are actionable here: the app's **Share → "Stage seen hunks"**
is `git add -p` driven by the tick boxes — only fully seen hunks reach the
index. A tour over `INDEX...WORKTREE` therefore doubles as a staging
checklist: order the stops so what belongs in the next commit is seen first.

Hard rule 1 matters most exactly here: on a WORKTREE range the uncommitted
changes ARE the review subject, and none of them exist anywhere else. Only
the human (or the app's own staging button) changes git state — you never
do, no matter what goes wrong.

## Leaving review comments

You can leave line comments the same way a human reviewer does — they appear
in every view that renders those lines, attributed to you:

```sh
concats comments add <path>:<start>[-<end>] -m "<comment>" --author <your-name> \
                            --repo . --base <BASE> --head <HEAD>
```

- Lines are **1-based, new-side** — exactly the numbers in the manifest's
  `#Lstart-end` links. Take them from `manifest` or `lint` output, **never
  compute them** (the no-line-arithmetic rule applies here too). Pure
  deletions have no new-side numbers and cannot be commented from the CLI.
- The anchor is validated against the diff: exit 1 means it missed, and the
  error lists the file's valid ranges to pick from.
- `concats comments` lists stored comments; `--delete <id>` removes
  one. A running review app shows an added comment within a second.

Push your tour's concrete `issue:`/`suggestion:` findings as comments too —
a finding anchored to its lines survives; one buried in prose gets scrolled
past.

## Didactic method — teach, don't dump

You have three sources: the captured decisions (`turns`), the commit
messages, and the diff itself. The craft is dissolving all three into one
narrative of very small steps:

- **Sequence like a lesson.** Order stops so each builds on the last:
  foundation first (new types, data shapes, config), then the behavior
  built on them, then the wiring and UI, then docs. A reader at stop N
  must need nothing from stop N+1.
- **Zoom rhythm: orient → show → point.** One or two sentences of where we
  are and why → one small link → one sentence on what to notice in it →
  the next fragment. Never stack links back-to-back without prose between
  them; never write a paragraph that references no code.
- **One link, one idea.** The hunk is your atom — a link renders whole
  hunks, so pick the smallest hunk that carries the point rather than
  narrowing line spans. A stop is typically 2–5 links in reading order,
  often hopping files mid-thought: the three lines in `main.rs` that
  consume the field you just saw, then the README line that documents it.
- **A hunk with several ideas** keeps its single link, and your prose
  walks its parts instead ("first the guard; then note the fallback at
  the bottom") — the transclusion is the visual, your sentences are the
  pointer.
- **Anchor code to decisions.** When a turn or a commit message explains a
  choice, say it at the exact link where the choice lands ("Turn #3 chose
  JSON in ~/.config over sqlite — this hunk is that decision"), not in a
  distant summary.
- **Refer back, never re-explain.** Later stops say "the `Recents` type
  from Stop 1", not a second introduction. One new concept per link; if a
  link needs two, it wants two links or two stops.

## Document shape

```markdown
# <One-line title: what this change is>

## Big Picture & Motivation

<Start with the high-level motivation and big-picture context. Explain *why* this change is needed and what problem it solves. Provide sufficient background and explain any domain concepts or jargon so that even someone completely new to the codebase can understand the context and purpose before reading any code.>

## Risk Profile & Decisions

<Identify the areas where risk concentrates. Extract and document key decisions, architecture choices, and trade-offs made during development by referencing the Concats session data (turns, prompts, responses, tool calls).>

## Intent vs implementation        <!-- only when turns exist -->

| Asked for (from Concats Session) | Status | Key Decision / Pivot / Notes |
|-----------|--------|-------|
| <requirement from the prompts> | implemented / partial / missing | <what was decided and where it differs> |

## The tour

| # | Stop | Priority | Files |
|---|------|----------|-------|
| 1 | <concern> | RIGOROUSLY REVIEW | `a.rs`, `b.rs` |
| 2 | <concern> | Notice | `c.md` |

### Stop 1: <Descriptive title of the concern>

**Big Picture:** How this fits into the broader system architecture and motivation for this specific block.

**Key Decision:** The design choice or trade-off made here, referencing session intent/discussion.

<One or two sentences orienting the first fragment — what we are about to look at and why it comes first.>

[<link line copied from the manifest>]

<One sentence: what to notice in the fragment above — then orient the next one, which often lives in another file.>

[<the next manifest link>]

**Review:**
- issue: <a problem you found — say what breaks and when>
- question: <something only the author can answer>
- praise: <a pattern worth keeping>

### Stop 2: …

## Summary checklist

| Area | Status | Action |
|------|--------|--------|
| Core logic | reviewed at stops 1–2 | — |
| Error handling | gap at stop 3 | author: handle the empty case |
| Tests | missing for stop 1 | request before merge |
```

## Stop guidelines

- **Start with the Big Picture.** Always introduce stops by establishing the motivation, explaining why the code exists, and translating any domain concepts. Write so that a newcomer feels oriented immediately.
- **One concern per stop**, presented in the zoom rhythm from the Didactic
  method: orient → small link → point, repeated. If a stop wants more than
  ~5 links, it is two stops.
- **Utilize Concats session data.** Mine the transcripts (`concats turns`) to document key decisions, alternative designs considered, and explanations given by the agent during creation. Do not just summarize *what* changed; document *why* it was decided that way.
- **Order by what the reviewer needs, not by path.** Lead with the change that would hurt most if wrong. The Files tab already exists for path order — a guide that mirrors it is adding nothing.
- **Priority markers:** `RIGOROUSLY REVIEW` (security, data integrity, breaking changes) · `Notice` (important but straightforward) · `Warning` (edge cases, risks) · `Context` (background). Lead the tour with the RIGOROUSLY REVIEW stops.
- **Conventional labels** in Review bullets: `issue:` `suggestion:` `question:` `thought:` `nit:` `praise:`. One label per bullet; make every `issue:` and `suggestion:` actionable — a concern without a suggested action is homework, not review.

## Do / Don't

**Do**
- Always start with the big picture and motivation before diving into details.
- Provide domain explanations to help newcomers understand the codebase.
- Group and present code in small, bite-sized, sequential chunks.
- Document design decisions using captured Concats turn history.
- Compare implementation against captured intent when turns exist; call out scope changes (asked-for things missing, unasked-for things added).
- Aim coverage at the changed lines that matter; a guide that clears `--min-coverage` on new-file bulk while skipping the edits to existing code has not reviewed the change (lint calls this out).
- End with the summary checklist and clear next actions.

**Don't**
- Paste code, ever. Reference it.
- Present large, complex blocks of code at once. Break them up.
- Walk every line. Skipping the trivial is the point of a guide — the "Not discussed" section keeps you honest.
- Assume the reviewer read the transcript or knows the codebase history.
- Use alarming language where neutral works: "consideration", not "risk", unless it is one.

## Example stop

### Stop 1: The right stick now orbits the camera  — RIGOROUSLY REVIEW

**Big Picture:** Until now the camera only followed the mouse. To match the session's prompt asking for "stick control that feels identical to the mouse drag", we reuse the existing drag translation logic.

**Key Decision:** We decided in Turn #3 to reuse the deadzone-rescaled curve to keep responsiveness uniform across mouse and stick inputs, rather than adding a separate scaling factor.

The stick input lands here first — nine lines that translate the raw axis
through the same deadzone curve the mouse path uses (that Turn #3 decision,
landing):

[game_view.rs (1790:1798)](file:///…/examples/gamemaker/src/game_view.rs#L1790-1798)

Note `dz` rescaling past the deadzone rather than clamping at it. The
result feeds the existing orbit call three lines further down — same units
as the mouse path, which is the invariant to check:

[game_view.rs (1800:1802)](file:///…/examples/gamemaker/src/game_view.rs#L1800-1802)

**Review:**
- question: `dz` rescales past the deadzone instead of clamping — intended so a slow push stays usable?
- suggestion: the magic 0.15 deadzone constant appears in both input paths; extract it before a third copy shows up.

