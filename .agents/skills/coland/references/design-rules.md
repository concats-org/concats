# Spec-First Implementation

Use coland rules as executable specifications. Before writing code, define the structural requirement as a rule. Run it. Get a deterministic list of every location that does not comply. Implement until the rule passes clean.

This is like TDD, but for structural properties: the rule is the spec, `coland check` is the test runner, and a clean run means the spec is met.

## When to Write a Rule

Write a rule when you need a complete, deterministic list of code locations matching (or violating) a structural pattern.

- **Migration**: changing how a library is called (e.g., auth, logging, database access). Write a rule matching callers of the old API. Run it. Work through the list.
- **Convention enforcement**: requiring a specific pattern in specific contexts (e.g., all handler functions must call `authorize()`). Write a rule matching handlers that lack the call.
- **Refactoring**: restructuring function signatures, module boundaries, or call patterns. Write a rule matching the old shape. Fix sites until the rule reports nothing.
- **Dependency cleanup**: finding all transitive callers of a function you want to remove. Write a rule that walks the call graph.

## When to Use Graph Commands Instead

Use `coland graph` when you need to understand structure, not enforce it:

- `coland graph search <name>`: find a symbol and its immediate neighborhood (callers, callees, references)
- `coland graph rank`: find the most central or connected functions
- `coland graph diagram`: visualize module or file-level architecture

Graph commands are for exploration and orientation. Rules are for verification and worklist generation.

## Workflow

1. Identify the structural property you want to enforce or the pattern you want to find.
2. Run `coland schema` to see available relations and columns.
3. Write a `.clr` rule file. The query matches locations that violate the property.
4. Run `coland validate --rule <your-rule>` to check syntax.
5. Run `coland check <target> --rule <your-rule> --format json` to get the hit list.
6. Work through findings one at a time, fixing each location.
7. Rerun after each batch of fixes until clean.

## Rule Authoring Quick Reference

A rule file (`.clr`) has four required parts:

```text
title "Human-Readable Title"
severity WARNING

description {
What this rule detects and why it matters.
}

query {
  ?[file, name, start_line] :=
      *function{file, name, start_line, ...},
      <your conditions>
}
```

The query is a read-only CozoScript subset. Use `*relation{column, ...}` to reference stored relations. The final output rule (`?[...]`) must include `file` and `start_line`.

Run `coland schema` for the full list of relations and columns. If a `rules/AGENTS.md` or `rules/CLAUDE.md` exists in the repository, it contains the complete DSL specification.

## Example: Migrating an Authentication Pattern

Goal: find all handler functions that call `check_session()` directly instead of using the new `authenticate()` middleware.

```text
title "Direct check_session Call in Handler"
severity WARNING

description {
Handler functions should use the authenticate() middleware
instead of calling check_session() directly. Direct calls
bypass rate limiting and audit logging.
}

query {
  handler[file, name, start_line] :=
      *function{file, name, start_line},
      starts_with(name, "handle_")

  ?[file, name, start_line] :=
      handler[file, name, start_line],
      *call{caller_name: name, callee_name: "check_session"}
}
```

Save as `rules/direct_check_session.clr`, then:

```bash
coland check ./src --rule rules/direct_check_session.clr --format json
```

Every reported location is a handler that needs updating. Fix them, rerun, and the rule will confirm when migration is complete.
