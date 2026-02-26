# Contributing

This guide will help you understand the overall organization of the project. It's the single source of truth for how to contribute to the code base.

> [!NOTE]
> The key words "MUST", "MUST NOT", "REQUIRED", "SHALL", "SHALL NOT", "SHOULD","SHOULD NOT", "RECOMMENDED", "MAY", and "OPTIONAL" in this document are to be interpreted as described in [RFC 2119](https://tools.ietf.org/html/rfc2119).

The project focuses on preserving intentm, automatic checkpointing to enable rollbacks, and enabling collaboration, while staying compatible with existing developer tooling.

## Principles

These principles guide design and implementation decisions:

1. **Collaborative**

Design for human-human, human-machine, and machine-machine collaboration with clear checkpointing, handoff paths, and branchable session histories.

2. **Compatible**

Design it such that it integrates well with existing tools and workflows to be useful today.

3. **Intuitive**

Prefer designs that reduce cognitive load and make collaboration flows clear.


## Architecture and Scope

Concats is built around a core-plus-interfaces model:

- **Core runtime (Rust-first)**: agent protocol integration, checkpointing/session persistence, automation.
- **Interfaces (language-flexible)**: CLI/TUI/GUI/Web clients that consume core APIs.
- **Boundary rule**: interface-specific concerns SHOULD stay out of the core unless explicitly approved.

Session/code coupling rule:

- Code snapshots and session context SHOULD be linkable as one artifact.
- Contributors SHOULD preserve the ability to branch from checkpoints instead of forcing linear histories.

## Setup

Install required Rust toolchains:

```sh
rustup toolchain install stable
rustup toolchain install nightly --component rustfmt
```

## Testing Requirements

Testing rigor is mandatory.

For Rust core contributions, you SHOULD use:

- Unit tests for local behavior.
- Integration tests for end-to-end core workflows.
- `mockall` for mocking trait boundaries.
- `proptest` for property-based coverage of invariants.

### AI Contribution Rule

When using an AI assistant, keep tasks scoped to either:

- implementation changes, or
- test authoring

Do not do both in one task unless explicitly requested by the human reviewer.

### Required Checks (Rust Core)

```sh
cargo +nightly fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace
```

## Licensing

The core project direction is open source under **AGPL**. Use SPDX identifiers in `Cargo.toml` and other manifests.

If you are unsure about licensing implications for a component, consult the project lead before merging.

## Documentation

All user-facing behavior MUST be documented.

Documentation should explain:

- What changed.
- Why it changed.
- How checkpoint/session semantics are affected.

For Rust crates, prefer complete crate-level docs in `lib.rs` and clear `///` docs for public APIs.

## Adding Components

For new Rust components:

```sh
cargo new crates/${NAME}
```

Then:

- add the crate to workspace members,
- apply the correct license metadata,
- document the crate purpose and integration points.

For non-Rust interfaces, place them in clearly named top-level directories and document ownership and build/test commands.

## Version Control

- Commit frequently in small, logical chunks.
- Ensure each commit is coherent and reviewable.
- Do not commit secrets, tokens, or private user data.

Commit messages MUST follow [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/).

## AI Assistants

AI assistance is encouraged with guardrails. Human contributors remain fully accountable for all committed code.

### Rules

- Review and understand AI-generated changes before committing.
- Validate behavior with appropriate tests/checks.
- Add attribution when AI materially contributes.

### Attribution

Use a `Co-Authored-By` trailer when applicable:

- Claude: `Co-Authored-By: Claude <noreply@anthropic.com>`
- Gemini: `Co-Authored-By: Gemini <google@users.noreply.github.com>`
- ChatGPT: `Co-Authored-By: ChatGPT <openai@users.noreply.github.com>`
