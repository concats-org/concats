# CLI Interface: init, sessions, and log

This RFC proposes reshaping the concats CLI to follow git's command structure. It introduces `concats init` for project setup with hook installation, `concats sessions` for listing and filtering recorded sessions, and `concats log` for inspecting session checkpoints by commit or file path. It also specifies the empty-state TUI behavior that guides new users toward initialization.

## Motivation

Today the CLI has three commands: `run` (launch TUI), `hook` (handle Claude Code events), and `hooks install` (write `.claude/settings.json`). A user who clones a repository or starts a new project must know to run `concats hooks install` before session recording works. The TUI's sessions browser shows "No sessions found" with no guidance on what to do next. There is no way to query session history from the terminal without launching the full TUI.

Git succeeds partly because its CLI is predictable: `git init` sets up a repository, `git log` shows history, `git blame` attributes lines to commits. Concats should follow the same pattern. A developer should be able to:

1. Run `concats init` in a repository to install everything needed for session recording.
2. Run `concats sessions` to list recorded sessions, optionally filtered by path.
3. Run `concats log` to inspect checkpoints for a session, optionally scoped to a commit or file.
4. See clear guidance in the TUI when no sessions exist yet.

This makes concats discoverable for new users and useful for CI pipelines, code review, and scripting.

## Design

### Command: `concats init`

```
concats init [--path <dir>]
```

`concats init` prepares a repository for session recording. It is idempotent: running it twice produces the same result.

#### Behavior

1. Resolve the target directory (defaults to cwd). Verify it is inside a git repository. Error if not.
2. Create `.git/concats/` if it does not exist. This directory holds hook state and future local metadata.
3. Install Claude Code hooks into `.claude/settings.json` (the existing `hooks install` logic).
4. Print a summary of what was created or confirmed.

```
$ concats init
initialized concats in /home/user/myproject
  hooks installed in .claude/settings.json
  state directory created at .git/concats/
```

If already initialized:

```
$ concats init
concats already initialized in /home/user/myproject
  hooks up to date in .claude/settings.json
```

#### Relation to `hooks install`

`concats hooks install` remains available as a lower-level escape hatch. `concats init` calls into the same code path but also handles the `.git/concats/` directory and any future initialization steps (git config, remote setup, etc.). New users should use `init`; `hooks install` is for targeted reinstallation.

#### CLI definition

```rust
#[derive(Subcommand)]
pub enum Commands {
    /// Initialize concats in a git repository.
    Init {
        /// Project root directory (defaults to current directory).
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
    // ... existing commands
}
```

### Command: `concats sessions`

```
concats sessions [--path <glob>] [--commit <rev>] [--format <format>] [--limit <n>]
```

`concats sessions` lists recorded sessions from the terminal without launching the TUI. This is the equivalent of `git branch -v` or `git log --oneline` for session history.

#### Default output

```
$ concats sessions
  8d6e2dc4  implement the session browser       2025-03-10T14:22:00Z  (3 checkpoints)
  a1b2c3d4  fix auth token refresh              2025-03-09T09:15:00Z  (7 checkpoints)
  f0e1d2c3  add dark mode support               2025-03-08T16:44:00Z  (5 checkpoints)
```

Each line shows: short session ID, session label (first prompt or fork label), timestamp of last checkpoint, and checkpoint count.

#### Filtering by path

```
concats sessions --path "src/auth/**"
```

When `--path` is given, only sessions whose checkpoints touched files matching the glob are included. "Touched" means the checkpoint's diff (against its session parent) includes at least one file matching the pattern.

Implementation: for each session, walk its checkpoint chain and check the diff file paths against the glob. This is O(sessions x checkpoints) but acceptable for typical repository sizes. A future optimization could index touched paths in a sidecar file.

#### Filtering by commit

```
concats sessions --commit abc1234
```

When `--commit` is given, only sessions whose checkpoints reference the given commit as parent 1 (the branch-history link described in the session storage RFC) are included. This answers: "which agent sessions were active around this commit?"

#### Output formats

- `--format table` (default): human-readable aligned columns
- `--format json`: machine-readable NDJSON, one object per session
- `--format short`: one-line per session, just ID and label

JSON output enables CI integration and scripting:

```json
{"id":"8d6e2dc4-77b2-4f9b-944a-6cd7a0c2c4db","label":"implement the session browser","modified_at":"2025-03-10T14:22:00Z","checkpoint_count":3}
```

#### CLI definition

```rust
/// List recorded sessions.
Sessions {
    /// Filter to sessions that touched files matching this glob.
    #[arg(long)]
    path: Option<String>,

    /// Filter to sessions linked to this commit.
    #[arg(long)]
    commit: Option<String>,

    /// Output format: table, json, short.
    #[arg(long, default_value = "table")]
    format: OutputFormat,

    /// Maximum number of sessions to show.
    #[arg(long)]
    limit: Option<usize>,
},
```

### Command: `concats log`

```
concats log <session-id> [--path <glob>] [--commit <rev>] [--format <format>]
```

`concats log` shows the checkpoint history of a single session. It is the session equivalent of `git log` — a chronological walk through the session's checkpoint chain.

#### Default output

```
$ concats log 8d6e2dc4
checkpoint 0a1b2c3d
  > implement the session browser
  tool read
  tool execute
  I added a sessions tab.
  M src/components/sessions.rs
  A src/components/sessions_browser.rs

checkpoint 1b2c3d4e
  > also add keyboard navigation
  tool read
  tool edit
  Done. Arrow keys and j/k now navigate the list.
  M src/components/sessions.rs
```

This is the same information shown in the TUI's detail panel, but rendered for the terminal.

#### Filtering by path

```
concats log 8d6e2dc4 --path "src/components/**"
```

When `--path` is given, only checkpoints whose diffs include files matching the glob are shown. This answers: "what did the agent do to these files in this session?"

#### Filtering by commit

```
concats log 8d6e2dc4 --commit abc1234
```

When `--commit` is given, only the checkpoint(s) that reference the given commit as parent 1 are shown. This narrows to "what was happening in this session at the time of this branch commit?"

#### Output formats

Same as `concats sessions`: `table` (default), `json`, `short`.

JSON output includes full transcript entries and diff summaries per checkpoint.

#### CLI definition

```rust
/// Show checkpoint history for a session.
Log {
    /// Session ID (prefix match accepted).
    session: String,

    /// Filter to checkpoints that touched files matching this glob.
    #[arg(long)]
    path: Option<String>,

    /// Filter to checkpoints linked to this commit.
    #[arg(long)]
    commit: Option<String>,

    /// Output format: table, json, short.
    #[arg(long, default_value = "table")]
    format: OutputFormat,
},
```

### TUI empty state

When the sessions browser tab is opened and no sessions exist, the current behavior is:

```
┌─Sessions (0)─────────────────────────┐
│  No sessions found. Press 'r' to     │
│  refresh.                            │
└──────────────────────────────────────┘
```

This should change to detect whether concats is initialized and show contextual guidance.

#### Not initialized (no `.claude/settings.json` with concats hooks)

```
┌─Sessions─────────────────────────────┐
│                                      │
│  No sessions recorded yet.           │
│                                      │
│  To start recording agent sessions,  │
│  run:                                │
│                                      │
│    concats init                      │
│                                      │
│  This installs hooks that            │
│  automatically capture sessions      │
│  when you use a coding agent.        │
│                                      │
└──────────────────────────────────────┘
```

#### Initialized but no sessions yet

```
┌─Sessions─────────────────────────────┐
│                                      │
│  No sessions recorded yet.           │
│                                      │
│  Hooks are installed. Sessions will  │
│  appear here after your first agent  │
│  conversation.                       │
│                                      │
│  Press 'r' to refresh.              │
│                                      │
└──────────────────────────────────────┘
```

#### Detection logic

Check whether concats hooks are installed by reading `.claude/settings.json` and verifying it contains a `hooks.UserPromptSubmit` entry whose command includes `concats hook`. This reuses the same settings path that `install()` writes to. The check should be a pure read with no writes.

### CI integration pattern

These commands compose into CI workflows. A GitHub Actions step might look like:

```yaml
- name: Check agent session coverage
  run: |
    # List sessions that touched files in this PR
    changed_files=$(git diff --name-only ${{ github.event.pull_request.base.sha }})
    for file in $changed_files; do
      concats sessions --path "$file" --format json
    done

- name: Annotate PR with session links
  run: |
    sessions=$(concats sessions --commit ${{ github.sha }} --format json)
    # Post session summaries as PR comment
```

The JSON output format is the key enabler here. CI tools parse JSON; they do not parse TUI output.

### Summary of CLI surface after this RFC

```
concats init [--path <dir>]                    # initialize repository
concats run [<agent>] [--workspace <dir>]      # launch TUI (existing)
concats sessions [--path <glob>] [--commit <rev>] [--format <fmt>] [--limit <n>]
concats log <session> [--path <glob>] [--commit <rev>] [--format <fmt>]
concats hook <event>                           # handle hook event (existing)
concats hooks install [--path <dir>]           # install hooks (existing)
```

### Implementation order

1. **`concats init`** — small surface, subsumes existing `hooks install`, immediate user value.
2. **TUI empty state** — small UI change, improves onboarding alongside `init`.
3. **`concats sessions`** — core listing with `--format json`. Path and commit filtering can follow as separate PRs.
4. **`concats log`** — builds on sessions listing, adds per-checkpoint detail.

Each step is independently useful and shippable.

## Drawbacks

Path filtering requires walking every checkpoint's diff for every session. In repositories with many long sessions this could be slow. An index or cache would help but adds complexity. The initial implementation should measure before optimizing.

Adding `init` as a required step introduces friction for users who just want to try concats. The current `hooks install` works without any ceremony. However, `init` is a well-understood pattern (every git user has run `git init`) and the overhead is one command.

The `log` command name collides conceptually with `git log`. Users might confuse the two. However, the required session ID argument makes the context clear, and the analogy is intentional — `concats log` is the session equivalent of `git log`.

## Alternatives

- **Single `concats show` command instead of `sessions` + `log`.** A single command with subcommand-like arguments (`concats show sessions`, `concats show 8d6e2dc4`) was considered. Two separate commands are clearer and follow git's pattern of distinct verbs (`git branch` vs `git log`).

- **Store a `.concats.toml` marker file in the repository root instead of relying on `.claude/settings.json` detection.** This would make initialization detection simpler but adds a committed file to every repository. The hooks check is sufficient and avoids repository pollution.

- **Skip `init` entirely and auto-initialize on first hook invocation.** This is what happens today (hooks create `.git/concats/` on demand). The problem is discoverability: the user never sees confirmation that recording is active, and the TUI has no way to distinguish "not set up" from "set up but unused." An explicit `init` creates a clear state transition.

- **Use `concats blame <path>` instead of `concats log --path`.** A dedicated blame command was considered for mapping file lines to sessions, similar to `git blame`. This is a richer feature that deserves its own RFC. The `--path` filter on `log` and `sessions` covers the simpler "which sessions touched this file" use case first.
