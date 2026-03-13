# Agent Session Storage

This RFC defines how to record interactive agent sessions in Git. It covers five related pieces of the design: the checkpoint commit message format, the ref namespace used to store session tips, the snapshot rules for repository state, and checkpoint parent, linking session checkpoints to ordinary branch commits without modifying the user's branch history.

## Motivation

We want to preserve a different kind of history than a hand-crafted branch. A branch is where a user stages specific hunks, splits work into reviewable commits, rewrites messages, and shapes a final narrative. Session recording serves a different purpose. It is an automatic log of a human and a tool iterating on a repository over time.

Those histories must stay separate. Session recording should not stage files, should not write commits onto the user's working branch, and should not make ordinary Git workflows harder. A user must still be able to make clean manual commits whenever they want.

At the same time, sessions should travel with the repository and remain inspectable with normal Git tooling. Reviewers should be able to see not only the final code but also the prompts, responses, tool calls, and intermediate repository states that led there. Reusing Git commits, trees, and refs gives this design a storage model that is local-first, deduplicated by Git's object model, and compatible with normal transport and inspection tools.

Finally, session history should connect back to ordinary branch history. Given a branch commit, a user should be able to discover the relevant session checkpoints. Given a session, a user should be able to see which branch commits it was adjacent to. That linkage must be one-directional: session refs may point at branch history, but the user's branch should remain untouched.

## Design

### Data Model

A recorded session consists of:

- a session identifier
- a dedicated Git ref whose tip represents the latest known session state
- zero or more checkpoint commits

A checkpoint commit contains:

- a full Git tree snapshot of the repository worktree
- a structured transcript payload stored in the commit message body
- one or two parents that connect the checkpoint to prior session history and, optionally, to ordinary branch history

A session ref may exist before the first checkpoint is written. In that state, the ref points at the session base commit rather than at a checkpoint commit.

### Ref Namespace

Sessions are stored under:

```text
refs/agent/sessions/<session-id>
```

This keeps session history out of `refs/heads/`. Custom refs are required here. Session refs must not appear as ordinary branches, and they must not participate in the user's normal branch workflow.

### Repository Snapshot

Every checkpoint stores a full tree snapshot of the repository worktree at the time the checkpoint is written.

Snapshot construction follows these rules:

- Read from the working tree, not from the Git index.
- Respect repository-local ignore rules from `.gitignore`.
- Respect `.git/info/exclude`.
- Respect the user's global Git ignore configuration.
- Include dotfiles unless they are ignored.
- Exclude `.git`.
- Exclude nested Git roots such as sub-repositories, linked worktrees, and directories like `.claude/worktrees/` that contain nested Git metadata.

Because snapshots are stored as normal Git trees and blobs, unchanged content is reused by Git. This design therefore gets full-snapshot semantics without inventing a second storage layer.

### Checkpoint Commit Format

A checkpoint commit message consists of a summary line, zero or more tagged body blocks, and Git trailers:

```text
{summary}

<prompt>{...}</prompt>
<response>{...}</response>
<tool kind="{kind}" />
<response>{...}</response>
<response>{...}</response>
...

Session: {session-id}
Agent: {agent-name}
```

This is intentionally a small XML-ish format, not general XML. Tags are fixed, non-nested, and line-oriented so they can be parsed with a simple tag parser.

The following requirements apply.

#### Subject

- The subject line MUST be present.
- The subject line is informational only and MUST NOT be used for parsing or session boundary detection.
- Writers SHOULD use a short human-readable summary.

#### Body Structure

- The body consists of zero or more top-level tagged blocks.
- At most one `<prompt>` block MAY be present.
- If a `<prompt>` block is present, it MUST be the first tagged block.
- Zero or more `<response>` blocks MAY be present after the prompt.
- Zero or more `<tool>` blocks MAY be present after the prompt.
- The order of the blocks is significant and MUST be preserved.

#### Recorded Content

- Recorded `<prompt>` and `<response>` blocks SHOULD be sanitized to avoid persisting privacy-relevant or security-sensitive information.
- `<prompt>` and `<response>` bodies are UTF-8 text.
- Every `<tool>` block MUST include a `kind` attribute.
- The `kind` attribute MUST use the corresponding [Agent Client Protocol tool kind](https://agentclientprotocol.com/protocol/tool-calls#param-kind).
- `<tool>` blocks MAY be omitted entirely.
- Recorded `<tool>` blocks MUST record only the tool kind and MUST NOT include tool-specific arguments, paths, payloads, or outputs.
- `<tool>` blocks MUST be self-closing.

#### Trailers

- `Session: <session-id>` MUST appear as a trailer and MUST match the `<session-id>` portion of the ref name.
- `Agent: <agent-name>` MUST appear as a trailer.

Example:

```text
implement the session browser

<prompt>Show recorded sessions in the TUI and let me inspect a previous turn.</prompt>
<response>I added a sessions tab.</response>
<tool kind="read" />
<tool kind="execute" />
<response>I wired turn inspection to the existing diff view.</response>
<response>The diff panel now loads from the selected checkpoint.</response>

Session: 8d6e2dc4-77b2-4f9b-944a-6cd7a0c2c4db
Agent: Claude
```

### Message Format Rationale

The outer format uses XML-ish tags because the primary requirement is simple, reliable structure that remains readable in raw Git output and easy for agents to follow. Fixed top-level tags give both humans and tools an obvious shape without requiring a full XML parser.

The format stays intentionally small:

- the subject remains a plain human-readable summary for `git log`
- the body uses a tiny fixed vocabulary of tags: `prompt`, `response`, and `tool`
- tool entries stay deliberately narrow by recording only the Agent Client Protocol tool kind
- selective recording is straightforward because tool-specific details are omitted entirely
- Git trailers carry session metadata in a form that is already familiar and easy to parse

This keeps the message easy to scan while still making ordering and message types explicit.

## Checkpoint Parents

Each checkpoint commit may have one or two parents.

Parent 0 is the session parent. It defines the first-parent session chain and the default diff base for the checkpoint. Parent 0 is:

- the previous checkpoint in the same session for ordinary later checkpoints
- the current branch `HEAD` for the first checkpoint in a fresh session
- the selected source checkpoint for the first checkpoint in a forked session

Parent 1, when present, is the current branch `HEAD` at the moment the checkpoint is written. Parent 1 links the session checkpoint to ordinary branch history without modifying the branch itself.

If parent 0 and the current branch `HEAD` are the same commit, writers MUST omit the duplicate second parent.

This yields the following graph:

```text
User branch:   A --------- B --------- C
                \            \            \
Session ref:    CP1 -------- CP2 -------- CP3
```

With parentage:

- `CP1` parents: `[A]`
- `CP2` parents: `[CP1, B]`
- `CP3` parents: `[CP2, C]`

For a forked session, the first checkpoint instead looks like:

- `FP1` parents: `[source-checkpoint, current-HEAD]`

The first parent remains the session lineage. This matters for traversal and for diffs: "show me what this checkpoint changed" should continue to mean "diff this checkpoint against the previous session checkpoint," not "diff it against the branch parent."

When a checkpoint is rewritten in place during an active turn, writers MUST preserve parent 0 and SHOULD refresh parent 1 to the current branch `HEAD` if `HEAD` has changed.

## Session Loading and Boundaries

Forking makes session boundaries explicit. A reader cannot safely infer session membership by "walk until the first non-checkpoint commit" because a fork may deliberately start from another checkpoint commit.

For checkpoints, session loading works as follows:

1. Resolve `refs/agent/sessions/<session-id>`.
2. If the ref tip is not a checkpoint payload whose `Session: <session-id>` trailer matches `<session-id>`, the session currently has zero checkpoints.
3. Otherwise, start at the tip checkpoint commit.
4. Walk only the first-parent chain toward older commits.
5. Keep commits only while the decoded checkpoint payload exists and its `Session: <session-id>` trailer matches `<session-id>`.
6. Stop at the first parent that fails either check, then reverse the collected commits for chronological presentation.

This makes the session boundary explicit, keeps forked sessions separate from their source sessions, and preserves cheap first-parent traversal.

## Linking Ordinary Commits to Session Refs

Ordinary branch commits are linked to session history by parentage on the session side only. The branch is never rewritten and never gains extra parents.

This gives the design three useful properties:

- Given a session ref, Git's normal graph rendering can show branch commits and checkpoints interleaved.
- Given a checkpoint, the linked branch commit is available directly as parent 1 when present.
- Given an ordinary branch commit, a reader can discover related sessions by scanning session refs for checkpoints that name that commit as parent 1.

In Git terms, these checkpoint commits are merge commits on the session ref side. Their trees are not expected to be the textual merge of their parents. The tree is simply the recorded worktree snapshot for that checkpoint.

This is intentionally one-directional. Session refs point to branch history. Branch history does not point back and remains safe for normal user workflows, rebases, and review.

## Drawbacks

Full snapshots can consume more storage than a patch-only format, especially in repositories with large generated files that are not ignored.

Two-parent checkpoint commits are more complex than a simple linear side history. Low-level tooling must preserve first-parent semantics for session traversal and diff rendering.

The namespace and format migration introduces a heterogeneous historical state. Readers must tolerate both legacy and canonical layouts for some period of time.

Custom refs remain less visible than ordinary branches. Some Git hosting surfaces and default tooling will not show `refs/agent/sessions/*` unless explicitly requested.

## Alternatives

- Keep single-parent checkpoint commits and store branch linkage only in commit trailers or message fields.
  This was rejected because the Git graph would no longer express the relationship directly. The resulting history is harder to query and less useful in normal Git visualization tools.
- Store session checkpoints on ordinary branches.
  This was rejected because it would mix generated session history with user-authored branch history and clutter branch-oriented workflows.
- Store session metadata in SQLite or another sidecar database.
  This was rejected because it introduces extra infrastructure, a separate sync problem, and another artifact that does not travel naturally with Git.
- Use Git notes on ordinary commits.
  This was rejected because a session is not just metadata attached to one commit. It is an ordered sequence of repository snapshots that may begin before the user makes any relevant manual commit.
- Use a JSON envelope as the canonical checkpoint body.
  This was rejected because the primary requirement is a simple tagged format that agents and the session viewer can parse with minimal machinery and without escape headaches.
