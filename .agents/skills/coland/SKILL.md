---
name: coland
description: >
  Use Coland for structural code verification and spec-first implementation.
  Activate on any code quality, planning, implementation, refactoring, or
  review task in this repository. Run `coland check` as a must-pass gate
  before shipping. Use coland rules as a deterministic worklist during
  design and refactoring. Use `coland graph` to navigate and understand
  code structure before editing.
---

# Coland

Structural code linter that combines syntactic analysis with semantic cross-file data. Use it on every code task: planning, implementation, review, and handoff.

Run `coland --help` for available commands and options.

## Verification

Run `coland check <target>` from the repository root. It must pass before shipping, like `cargo test` or `cargo clippy`.

- Clean check = shippable. Dirty check = not shippable.
- Handle one finding at a time. Rerun after each fix.
- Use `--format json` for machine-readable output.
- Stop if validation fails, findings overlap, or the structure is ambiguous.

For the full shipping loop, rule modes, cleanup recipes, and stop rules, see [references/verification.md](references/verification.md).

## Spec-First Implementation

Define verifiable requirements as coland rules before writing code. Write a rule that matches the structural pattern you want to enforce or eliminate. Run it. Get a complete, deterministic list of every non-compliant location. Implement until the rule passes clean.

This is like TDD, but for structural properties: the rule is the spec, `coland check` is the test runner, and a clean run means the spec is met.

For guidance on authoring rules, the workflow, and a concrete example, see [references/design-rules.md](references/design-rules.md).

## Code Navigation

Use `coland graph` to find relevant code before editing. Search for symbols, explore call neighborhoods, rank functions by centrality, and visualize module architecture.

Run `coland graph --help` for available subcommands.

## Code Quality

Write code that is easy to understand, safe to change, and simple to test. Coland enforces structural quality principles: small focused units, minimal coupling, dead code elimination, narrow interfaces, and separation of actions from calculations.

For the full set of principles and which rules enforce them, see [references/code-quality.md](references/code-quality.md).

## When Not To Use This Skill

- Do not use Coland output as a reason to invent speculative abstractions.
- Do not auto-apply public API cleanup from Coland findings.
- Do not synthesize shared test helpers from duplication alone.
