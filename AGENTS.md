# AGENTS.md

## Required Checks

All code must pass the project's formatter, linter, and test suite before committing.

```bash
cargo +nightly fmt --check    # Formatting
cargo clippy -- -D warnings   # No lint warnings
cargo test                    # All tests pass
```

## Philosophy

Keep changes minimal and focused. Only do what was requested — no drive-by refactors, no speculative features, no extra abstractions. Always read existing code and comments before proposing changes!

## Current Phase

Transitioning from proof-of-concept to product. Breaking changes are acceptable when they simplify the system. Compatibility is not a default goal.

## Do Not

- Do NOT add wrapper types, DTOs, adapter structs, or intermediate layers unless they isolate a real crate or protocol boundary.
- Do NOT create helper methods referenced only once.
- Do NOT add trait-object indirection or dependency injection solely for testing. Restructure so the testable part is a pure function.
- Do NOT add new `impl` blocks when a free function would suffice. A struct must carry invariants or state to justify its existence.
- Do NOT duplicate orchestration. If a load → operate → save sequence exists, use it; do not create a parallel path.
- Do NOT add builder patterns, factory methods, or configuration structs for things that have one call site.
- Do NOT add error handling, fallbacks, or validation for scenarios that cannot occur in practice.
- Do NOT suppress dead code with `#[allow(dead_code)]` or rename unused items with `_` prefixes. Delete them!
- Do NOT add comments, docstrings, or type annotations to code you did not change.
- Do NOT invent vocabulary. Use Git-native terms (`commit`, `tree`, `ref`, `parent`) and project terms (`session`, `checkpoint`, `entry`) consistently.

## Design Principles

**Simplicity first.** Prefer deletion over addition. Ask: "can we remove the need for this by changing something adjacent?" Abstractions must earn their keep through real pressure, not hypothetical reuse.

**Functional core, imperative shell.** Pure calculations (same inputs → same outputs) live in the interior. Actions (I/O, mutation) form a thin shell at the top. If a function is hard to test, extract the pure logic and push the effect to the caller.

**Immutability by default.** Add `mut` only when clearly needed. Treat writes as copy-modify-return.

**Direct representation.** When two concepts are directly related (e.g. a `Checkpoint` derived from a Git commit), make that relationship transparent. Do not introduce intermediate types to hide it.

**Parameterize the varying part.** When repeated code differs in one place, make that place a parameter. When it differs in behavior, pass a function.

## Code Style

- Clarity over cleverness. The cleanest code is the code that was deleted.
- Three similar call sites sharing most of their body is a refactor signal. Two is fine.
- Wire formats, ref patterns, and message tags must be defined as constants or types in a single location. Ad-hoc string matching between writer and parser is a bug.
- Prefer declarative transforms (`map`, `filter`, `filter_map`, `collect`) over imperative loops with manual match/continue/break.
- Functions: aim for under 40 lines, at most 4 parameters. Over 60 lines or 5+ parameters is a split signal.
- One file, one concern. Read/query logic stays separate from state-mutating logic.
- Prefer data coupling (pass values) over control coupling (pass flags that switch behavior). A boolean parameter that makes a function do two different things should be two functions.

## Error Handling

Structured errors with actionable context. Do not silently swallow errors. When intentionally ignoring a fallible operation, log a diagnostic and add a `NOTE:` comment. Exception: fire-and-forget channel sends on shutdown paths.

## Dependencies

Use established libraries for standard problems (date/time, diff parsing, serialization). Do not hand-roll what a well-tested crate already does.

## Comments

Only for *why*, not *what*. Prefix with `NOTE:` or `TODO:`. One topic per comment. Update or remove comments when the code they describe changes.

## Commit Messages

Follow [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/) and explain the reasoning behind the change. Include the correct co-author trailer:

```
Co-Authored-By: Claude <noreply@anthropic.com>
Co-Authored-By: Gemini <google@users.noreply.github.com>
Co-Authored-By: ChatGPT <openai@users.noreply.github.com>
```
