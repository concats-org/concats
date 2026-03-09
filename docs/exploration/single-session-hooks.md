# Exploration: Single-Session-Per-Directory via Hooks & Session ID in Commits

## Status: Exploration / Research

## Context

We want to:
1. Ensure only one Claude Code session runs per directory at a time
2. Automatically add session ID as a git commit trailer (e.g., `Claude-Session: <id>`)
   to tie commits to the session that created them

## Findings

### Session ID Availability in Hooks

All Claude Code hooks receive JSON on stdin with a `session_id` field:

```json
{
  "session_id": "abc123",
  "cwd": "/path/to/project",
  "hook_event_name": "SessionStart",
  ...
}
```

**However**, `session_id` is NOT exposed as an environment variable (`$CLAUDE_SESSION_ID`)
to Bash commands or MCP servers. This is an open feature request:
- https://github.com/anthropics/claude-code/issues/25642

The `CLAUDE_ENV_FILE` mechanism (write `export VAR=val` lines in a `SessionStart` hook)
exists but has known reliability issues:
- https://github.com/anthropics/claude-code/issues/15840
- https://github.com/anthropics/claude-code/issues/24775

### Single-Session-Per-Directory: NOT Enforced by Claude Code

Claude Code does **not** enforce one session per directory. Multiple concurrent sessions
in the same directory cause known problems:
- Plan file overwrites: https://github.com/anthropics/claude-code/issues/27311
- Config corruption (Windows): https://github.com/anthropics/claude-code/issues/29036
- OAuth token refresh races: https://github.com/anthropics/claude-code/issues/24317

A lock file mechanism has been requested but is not yet implemented:
- https://github.com/anthropics/claude-code/issues/19364

### Available Hook Events (Relevant Subset)

| Event            | When                        | Can Block? |
|------------------|-----------------------------|------------|
| `SessionStart`   | Session begins/resumes      | Yes (exit 2) |
| `SessionEnd`     | Session terminates          | No         |
| `PreToolUse`     | Before any tool executes    | Yes (exit 2) |
| `PostToolUse`    | After tool succeeds         | No         |

## Approach Options

### Option A: SessionStart Hook + Lock File (Single-Session Enforcement)

```
SessionStart hook:
  1. Read session_id from JSON stdin
  2. Check for /tmp/claude-lock-<encoded-cwd> (or .claude-session.lock)
     - If exists and PID alive → exit 2 (block session)
     - Otherwise → write lock file { pid, session_id, started_at }
  3. Write session_id to CLAUDE_ENV_FILE (best-effort)
  4. Also write to .claude-session-id as fallback

SessionEnd hook:
  1. Remove lock file
```

**Risks**: SessionEnd may not fire on crashes/kills → stale lock files. Mitigate with
PID liveness checks.

### Option B: prepare-commit-msg Git Hook (Session ID Trailer)

```
prepare-commit-msg git hook:
  1. Read .claude-session-id (written by SessionStart hook)
     OR read $CLAUDE_SESSION_ID (if/when exposed)
  2. If running under Claude Code ($CLAUDECODE=1):
     Append trailer: "Claude-Session: <session_id>"
```

This is a standard git hook, not a Claude Code hook. It works because:
- `$CLAUDECODE=1` is already set in all Claude Code Bash subprocesses
- The session ID file is written at session start

### Option C: PreToolUse Hook on Bash (Intercept git commit)

Instead of a git hook, use a Claude Code `PreToolUse` hook matching `Bash`:
- Parse `tool_input.command` for `git commit`
- Inject `--trailer "Claude-Session: <id>"` into the command

**Downside**: Fragile command parsing, easy to bypass.

## Recommendation

**Combine Options A + B:**
- `SessionStart` hook writes lock file + `.claude-session-id`
- `SessionEnd` hook cleans up lock file
- `prepare-commit-msg` git hook reads `.claude-session-id` and appends trailer
- Add `.claude-session-id` and `.claude-session.lock` to `.gitignore`

This approach:
- Works today (no dependency on `$CLAUDE_SESSION_ID` env var)
- Uses standard git hooks for the commit trailer (reliable, well-understood)
- Handles the single-session constraint via Claude Code hooks
- Degrades gracefully (if session ID file missing, trailer is just omitted)

## Integration with Concats

Since this repo (concats) is about recording agent sessions, the `concats hooks install`
command could set up both:
1. The Claude Code hooks (SessionStart/SessionEnd for lock + session ID file)
2. The git hooks (prepare-commit-msg for trailer injection)

This would be a natural extension of the existing `concats hooks install` functionality.

## Open Questions

- Should the lock file live in `/tmp/` (survives `.gitclean`) or in `.claude/`?
- Should we also record the session URL (e.g., `https://claude.ai/code/session_<id>`)
  in the trailer, or just the raw session ID?
- How to handle `--amend` commits (should trailer be updated or preserved)?
- What happens with `git rebase` — should trailers be stripped or preserved?
