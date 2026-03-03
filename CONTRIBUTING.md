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

Install the stable toolchain for building and the nightly toolchain for formatting:

```sh
rustup toolchain install stable
rustup toolchain install nightly --component rustfmt
```


## Components

The project is organized into multiple components, each with destinct purpose and responsibilities.

Most components are released under **AGPL-3.0-or-later**, but individual components MAY use a different license. Each component's manifest and `LICENSE` file are authoritative — always check them.

All dependencies MUST be license-compatible with the component they are added to. Use [SPDX identifiers](https://spdx.org/licenses/) (e.g., `AGPL-3.0-or-later`) in `Cargo.toml` and other manifests. If you are uncertain about which license applies or whether a dependency is compatible, please reach out to the maintainers before merging.

### Adding Components

New components SHOULD be added using `cargo new crates/${NAME}`.

After creating a new crate:

- add the crate to workspace members,
- link the appropriate license file: `ln -s ../../LICENSE-${TYPE} crates/${NAME}/LICENSE-${TYPE}`,
- set the correct `license` field in the crate's `Cargo.toml` using the SPDX identifier,
- document the crate purpose and integration points.

If you are uncertain about where to put things or which license to use for a new component, please consult the maintainers.


## AI Assistants

> [!TIP]
> For _AI Agent_ guidance on effective collaboration on this project, please refer to the `AGENTS.md` file in the repository root.

We actively encourage the use of AI assistants (e.g., Claude, Gemini, ChatGPT) to boost productivity. We also encourage you to share your AI sessions using _concats_ — making collaboration visible helps the whole team learn, iterate faster, and build on each other's work.

That said, the human contributor is always ultimately accountable for the code they commit. AI assistance does not diminish that responsibility. Contributors MUST review, understand, and test all AI-generated code before submitting it.

### Principles

- **Accountability** — You own every line you commit, whether you wrote it or an AI did. Review it, test it, and make sure it meets the project's standards.
- **Transparency** — When an AI materially contributes to a commit, acknowledge it with a `Co-Authored-By:` trailer. Share your sessions via _concats_ so reviewers have full context.
- **Quality** — AI-generated code MUST pass the same bar as human-written code. Run the required checks and make sure the contribution is correct, secure, and well-documented.

### Attribution

Add the appropriate `Co-Authored-By` trailer in the commit message body:

- Claude: `Co-Authored-By: Claude <noreply@anthropic.com>`
- Gemini: `Co-Authored-By: Gemini <google@users.noreply.github.com>`
- ChatGPT: `Co-Authored-By: ChatGPT <openai@users.noreply.github.com>`

> [!NOTE]
> Not all AI assistants have an official GitHub account. Use the organization name with the default GitHub noreply email to avoid mentioning actual users.

## Documentation

All user-facing features MUST be documented. Quality documentation lowers the barrier to entry and helps users effectively understand and work with the system.

Each crate MUST include comprehensive front-page documentation in `lib.rs` covering an introduction, quick start example, feature overview, and integration guide. Every public function, struct, enum, trait, and module MUST have `///` doc comments describing their purpose and SHOULD include usage examples.

Documentation SHOULD be clear, concise, complete, up to date, and consistent with project conventions. Missing or poor documentation for user-facing features will block pull requests — when in doubt, document more rather than less.

For detailed guidance on how to write good documentation, see the [rustdoc book](https://doc.rust-lang.org/rustdoc/how-to-write-documentation.html).

## Version Control

Changes SHOULD be committed frequently in small logical chunks that MUST be consistent, work independently of any later commits, and pass the linter plus the tests. Doing so eases rollback and rebase operations. Commits MUST not include any customer data.

Commit messages SHALL follow the [Conventional Commits specification](https://www.conventionalcommits.org/en/v1.0.0/). This provides a framework for explicit, readable messages and enables automated changelog generation.

## Code Quality

### Formatting

All Rust code MUST be formatted using the nightly toolchain:

```sh
cargo +nightly fmt
```

To verify formatting without modifying files:

```sh
cargo +nightly fmt --check
```

### Linting

All code MUST pass clippy with no warnings:

```sh
cargo clippy --all-targets --all-features -- -D warnings
```

Address clippy suggestions rather than suppressing them. If a lint is genuinely inapplicable, add an `#[allow(...)]` with a comment explaining why.

### Testing

Run the full test suite before submitting changes:

```sh
cargo test --workspace
```

New functionality MUST include tests. Bug fixes MUST include a regression test that fails without the fix. Tests SHOULD be placed in a `#[cfg(test)]` module within the same file as the code under test. Integration tests that exercise cross-crate behavior belong in the `tests/` directory of the relevant crate.
