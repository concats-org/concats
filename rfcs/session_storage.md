# Agent Session Storage

This RFC defines how Concats records interactive agent sessions in Git using
two linked histories: transcript-only turns and full worktree snapshots.

## Motivation

We want to preserve more than a final branch narrative. A recorded session
should keep the prompts, responses, tool calls, and repository states that led
to the result, while staying inspectable with normal Git tooling.

The storage model also needs to keep branch history separate from session
history. Concats must not write commits onto the user's branch or stage files
for them. Session refs may point at branch history, but the branch itself stays
untouched.

## Data Model

A recorded session consists of:

- a session identifier
- a turn ref under `refs/agent/sessions/<session-id>`
- zero or more turn commits
- a snapshot ref under `refs/agent/snapshots/<session-id>`
- zero or more snapshot commits

A turn commit contains:

- an empty tree
- a structured transcript payload stored in the commit message body
- one or two parents that connect the turn to prior session history and,
  optionally, to ordinary branch history

A snapshot commit contains:

- a full Git tree snapshot of the repository worktree
- a small commit message with a `Session: <session-id>` trailer
- one or two parents that connect the snapshot to prior snapshot history and to
  the corresponding turn

A session ref may exist before the first turn is written. In that state, the
ref points at the session base commit rather than at a turn commit. A snapshot
ref does not exist until the first turn is recorded.

## Ref Namespace

Turns are stored under:

```text
refs/agent/sessions/<session-id>
```

Snapshots are stored under:

```text
refs/agent/snapshots/<session-id>
```

This keeps session history out of `refs/heads/` while still making both chains
addressable by normal Git commands.

## Turn Commit Format

A turn commit message consists of a summary line, zero or more tagged body
blocks, and Git trailers:

```text
{summary}

<prompt>{...}</prompt>
<response>{...}</response>
<tool kind="{kind}" />
...

Session: {session-id}
[Agent: {agent-name}]
```

The format is intentionally a small XML-ish structure, not general XML. Tags
are fixed, non-nested, and line-oriented so they can be parsed with a small
parser.

The following requirements apply:

- The subject line MUST be present.
- The subject line is informational only and MUST NOT be used for parsing or
  session boundary detection.
- The body consists of zero or more top-level tagged blocks.
- `<prompt>` and `<response>` bodies are UTF-8 text.
- `<tool>` entries record only the ACP tool kind.
- `Session: <session-id>` MUST appear as a trailer and MUST match the ref name.
- `Agent: <agent-name>` MAY appear as a trailer.

## Snapshot Commit Format

Snapshot commit messages are intentionally small:

```text
snapshot

Session: {session-id}
```

The subject line is informational. The `Session` trailer is required so
first-parent snapshot walks have an explicit boundary.

## Snapshot Construction

Every snapshot stores a full tree snapshot of the repository worktree at the
time the snapshot is written.

Snapshot construction follows these rules:

- Read from the working tree, not from the Git index.
- Respect repository-local ignore rules from `.gitignore`.
- Respect `.git/info/exclude`.
- Respect the user's global Git ignore configuration.
- Include dotfiles unless they are ignored.
- Exclude `.git`.
- Exclude nested Git roots such as sub-repositories, linked worktrees, and
  directories that contain nested Git metadata.

## Parent Structure

### Turn Parents

Each turn commit may have one or two parents.

Parent 0 is the session parent. It defines the first-parent turn chain and the
default diff base for the turn. Parent 0 is:

- the previous turn in the same session for later turns
- the current branch `HEAD` for the first turn in a fresh session
- the selected source turn for the first turn in a forked session

Parent 1, when present, is the current branch `HEAD` at the moment the turn is
written. Parent 1 links the turn to ordinary branch history without modifying
the branch itself.

If parent 0 and the current branch `HEAD` are the same commit, writers MUST
omit the duplicate second parent.

### Snapshot Parents

The first snapshot in a session has one parent:

- parent 0 is the corresponding turn

Later snapshots have two parents:

- parent 0 is the previous snapshot in the same session
- parent 1 is the corresponding turn

This yields a fully parent-linked DAG:

```text
refs/heads/main:              A <- B <- C
                              ^    ^    ^
refs/agent/sessions/s:        t1 <- t2 <- t3
                              ^    ^    ^
refs/agent/snapshots/s:       s1 <- s2 <- s3
```

Turn refs reach branch commits. Snapshot refs reach snapshots, turns, and the
branch commits that those turns reference.

## Writing Turns and Snapshots

When recording a new turn:

1. Create the turn commit object first with its final parent list and turn
   message, but do not move refs yet.
2. Create the snapshot commit object from the current worktree tree, parented
   to the new turn and, when present, the previous snapshot.
3. Advance `refs/agent/snapshots/<session-id>`.
4. Advance `refs/agent/sessions/<session-id>`.

When rewriting the active turn:

- preserve turn parent 0
- refresh turn parent 1 to the current branch `HEAD` when `HEAD` has changed
- preserve snapshot parent 0 when a previous snapshot exists
- repoint the snapshot's turn parent to the rewritten turn

## Loading and Traversal

Turn loading works as follows:

1. Resolve `refs/agent/sessions/<session-id>`.
2. If the ref tip is not a turn payload whose `Session: <session-id>` trailer
   matches `<session-id>`, the session currently has zero turns.
3. Otherwise, start at the tip turn commit.
4. Walk only the first-parent chain toward older commits.
5. Keep commits only while the decoded turn payload exists and its `Session`
   trailer matches `<session-id>`.
6. Stop at the first parent that fails either check, then reverse the collected
   turns for chronological presentation.

Snapshot loading uses the snapshot ref and first-parent traversal. Because
parent 0 on later snapshots is always the previous snapshot, standard Git
commands such as `git log --ancestry-path` can walk the snapshot chain.

Turn-to-snapshot lookup works by resolving `refs/agent/snapshots/<session-id>`
and walking the first-parent snapshot chain until the snapshot whose turn
parent matches the requested turn.

## Diff and Restore

Restoring a turn uses the matching snapshot tree, not the turn tree.

Diffing a turn works like this:

- if turn parent 0 is another turn, diff the current turn's snapshot tree
  against the previous turn's snapshot tree
- otherwise, diff the current turn's snapshot tree against turn parent 0's
  commit tree

This keeps "show me what this turn changed" aligned with the recorded worktree
state.

## Git Tooling Implications

Because structure is expressed through parent pointers, standard Git traversal
works directly on the recorded history:

- `git log --graph --all --oneline`
- `git log --ancestry-path <turn>..refs/agent/snapshots/<session-id>`
- `git for-each-ref --contains <commit>`
- `git merge-base`

NOTE: `git branch --contains` is not sufficient here because it enumerates branches,
not arbitrary custom refs such as `refs/agent/sessions/*`.

## Transport Implications

Pushing only `refs/agent/sessions/<session-id>` transfers the turn commits and
any branch commits they reach, but not the snapshot ref or the snapshot commits
that descend from those turns.

Pushing `refs/agent/snapshots/<session-id>` transfers the full connected graph
reachable from the snapshot tip: snapshots, turns, and the branch commits
reachable from those turns.

This means snapshot sync is intentional and separable, while still keeping the
graph structure native to Git.

## Drawbacks

- Session refs can keep rewritten branch commits reachable, which is useful for
  auditability but means those commits are not immediately GCible by Git.
- Custom refs remain less visible than ordinary branches in some hosting and UI
  surfaces.

## Alternatives

- Store snapshot OIDs in commit messages or trailers.
  This was rejected because Git would not understand the relationship as graph
  structure.
- Store snapshot linkage in a turn tree manifest.
  This was rejected because parentage already expresses the structure more
  directly.
- Store session turns on ordinary branches.
  This was rejected because it would mix generated session history with user
  branch history.
- Store session metadata in SQLite or another sidecar database.
  This was rejected because it adds extra infrastructure and loses the
  transport and inspection benefits of Git-native objects.
