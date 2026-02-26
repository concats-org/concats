# AGENTS.md

## Required Checks

All code MUST pass before committing:

```bash
cargo +nightly fmt --check    # Formatting
cargo clippy -- -D warnings   # No lint warnings
cargo test                    # All tests pass
```

## Coding

Keep changes minimal and focused. Only do what was requested — no drive-by refactors, no speculative features, no extra abstractions.

- Clarity over cleverness: Readable, straightforward code wins.
- No premature abstraction: No helpers or utilities for one-time operations, no design for hypothetical future requirements. Three similar lines are better than a premature abstraction.
- Error handling: Structured errors with actionable context via `thiserror` and `miette` (rust).
- Documentation: Document public APIs and complex logic. Never add docstrings, comments, or type annotations to code you did not change.

## Comments

Add comments only for difficult-to-understand or critical code. Explain *why*, not *what*.

- Prefix with `NOTE:` or `TODO:`, one topic per comment.
- Before modifying complex code, search for existing comments to understand context.
- Update or remove comments when the code they describe changes.

```rust
/// NOTE: `.claude/worktrees/` is explicitly filtered out as it may contain nested
/// git state and may be modified by other agents which leads to trouble.
///
/// NOTE: Never call `index.write()`, so the on-disk index (the user's staged
/// changes) is left untouched.
fn build_tree_from_workdir(&self, repo: &git2::Repository) -> Result<git2::Oid> {
    // ...
}
```

## Commit Messages

Messages MUST follow [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/) and explain the reasoning behind the change. Include the correct co-author trailer:

- Claude: `Co-Authored-By: Claude <noreply@anthropic.com>`
- Gemini: `Co-Authored-By: Gemini <google@users.noreply.github.com>`
- ChatGPT: `Co-Authored-By: ChatGPT <openai@users.noreply.github.com>`
