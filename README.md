# Concats

> [!WARNING]
> This is a proof of concept and not yet production ready! Expect missing security features, rough edges, lack of tests, and breaking changes.

When you pair with a coworker — whiteboarding an approach, talking through tradeoffs, or debug an issue — the solution isn't the only thing that comes out of it. It also builds understanding: about a problem-space, the decision for one solution over another, about different approaches and concepts. That knowledge compounds across a team, and it's one of the ways to grow individually and together.

An increasing share of that work now happens in conversations with AI. One describes a problem, iterates on an approach, arrives at a solution but then the conversation goes into the void. The code lands in a commit, but the thinking behind it is lost. But it belongs to the code, not the chat log.

Software engineering was always more than syntax. In a world where more of the writing-code part is handled by AI, the thinking matters *more*, not less — and right now, we're losing most of it. This is an attempt to fix that. Concats records agent sessions — prompts, responses, and repository state at each step — and stores them as part of the project's history, where they can be reviewed, learned from, and built upon.

## Getting Started

### Claude Code Hooks

If you're already using [Claude Code](https://docs.anthropic.com/en/docs/claude-code), the fastest way to start is via hooks:

```sh
concats hooks install
```

This registers Concats with Claude Code's hook system. From here on, every session is recorded automatically — prompts, responses, and repository snapshots at each step. Just use Claude Code as you normally would.

> **Note:** This currently overrides existing hook settings in `.claude/settings.json`.

### Terminal Interface (ACP)

Concats also ships a terminal UI that connects to agents via the [Agent Client Protocol](https://agentclientprotocol.com):

```sh
concats run
```

This gives you an interactive session with an agent while recording everything in the background. You can also browse past sessions, inspect individual checkpoints, and fork from any point to try a different approach.

## How Recording Works

At each checkpoint, Concats captures a full repository snapshot (respecting `.gitignore`) alongside the agent conversation — the prompt, the response, and what changed on disk.

Everything is stored under a dedicated Git ref per session (`refs/agent/sessions/<session-id>`), where each checkpoint is a commit. Concats **never touches your working branch**. It doesn't stage files, it doesn't commit to your branch. Your branches, your PRs, your commits — all yours. 

When you fork from a checkpoint, Concats restores the working directory to that point, starts a new session, and appends a short note linking back to the original when you prompt the agent. That's enough for an agent to look up what happened before, so you can say "continue, but watch out for X" and it picks up where things left off.

## Status

This is early. There are basically no tests, some parts were _generated_ into existence from clear descriptions rather than carefully engineered, and there are critical security relevant features still missing. Check the [Issues](https://github.com/concats-org/concats/issues) to get a sense of direction.

That said; active development continues, and if you believe this is a problem worth solving, you're more then welcome to join me in this effort. 

Let's see where this goes.
