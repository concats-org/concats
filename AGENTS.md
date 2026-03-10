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

## Design


### Actions, Calculations, Data

Classify every piece of code:

- **Data** — inert values with no behavior or timing dependency.
- **Calculations** — pure functions: same inputs → same outputs, no side effects.
- **Actions** — anything that depends on when or how many times it's called (I/O, time, randomness, mutation of external state).

Actions are infectious: if `send_email()` is an action, then `notify_user()` which calls it is also an action, and `process_order()` which calls that is an action too. One side effect poisons the entire call chain above it. Structure code so pure calculations live in the interior of the call graph and actions form a thin shell at the top — the shell calls calculations, collects return values, then performs effects. This is the "functional core, imperative shell" pattern (also known as "ports and adapters" or "hexagonal architecture").

```typescript
// BAD: calculation mixed with action
function addItemToCart(name: string, price: number): void {
  cart.push({ name, price });                                // implicit I/O
  const total = cart.reduce((s, i) => s + i.price, 0);
  document.getElementById("total")!.innerText = `$${total}`; // action: DOM
}

// GOOD: extract calculations, action is a thin shell
function addItem(cart: Item[], item: Item): Item[] {
  return [...cart, item];
}
function calcTotal(cart: Item[]): number {
  return cart.reduce((sum, i) => sum + i.price, 0);
}
function handleAddToCart(name: string, price: number): void {
  cart = addItem(cart, { name, price });
  updateTotalDom(calcTotal(cart));
}
```

To extract a calculation from an action: replace implicit inputs (globals, closed-over mutable state) with parameters, replace implicit outputs (mutations, DOM writes) with return values. This process is mechanical and always safe.

### Immutability

Default to immutable. Treat writes as "copy, modify copy, return copy." This turns reads into calculations — they always return the same value, making code easier to test and reason about.

Use shallow copy-on-write for code you control. Use deep defensive copies at boundaries with untrusted or legacy code. Add mutation (`mut`, reassignment) only when clearly needed.

### Stratified Design

Organize code into layers where each layer uses the vocabulary of exactly one level of abstraction. Higher layers express business intent; lower layers provide general-purpose utilities.

```
┌─────────────────────────────────┐
│  BUSINESS RULES (actions)       │  ← changes often, project-specific
│  handleAddToCart, processOrder  │
├─────────────────────────────────┤
│  DOMAIN LOGIC (calculations)    │  ← changes sometimes
│  freeTieClip, calcShipping      │
├─────────────────────────────────┤
│  DATA OPERATIONS (barrier)      │  ← changes rarely
│  addItem, removeItem, calcTotal │
├─────────────────────────────────┤
│  GENERIC UTILITIES              │  ← changes almost never
│  update, withCopy, map, filter  │
└─────────────────────────────────┘
```

Signals you're on the right track: a function reads like a description at one level ("if there's a tie but no tie clip, add a free one"), you can change the data structure without touching business logic, and adding a new feature means adding code in one layer rather than touching every file.

Signal you're not: a function mixes business rules with low-level data manipulation, or you find yourself reaching through two layers to call a utility.

### First-Class Abstractions

When you see repeated code that differs only in one part, make the varying part a parameter. When the varying part is behavior, pass a function.

```typescript
// BAD: implicit argument in function name
function setPriceByName(cart, name, price) { ... }
function setQuantityByName(cart, name, quantity) { ... }

// GOOD: make the varying part explicit
function setFieldByName(cart, name, field, value) { ... }
```

When two functions share the same wrapper but differ in the body, extract the wrapper and pass the body as a callback. This eliminates duplication and creates reusable tools (error-handling wrappers, retry logic, copy-on-write utilities).

### Shared Mutable State

Shared mutable state across concurrent timelines creates bugs. Prefer local state and return values over globals. When sharing is necessary, coordinate explicitly: use queues to serialize access, use join/gather to wait for parallel work, and keep the mutable surface area as small as possible.

## Code Style

Clarity over cleverness. Readable, straightforward code wins.

**Abstraction threshold:** Three or more call sites sharing >50% of their body is a refactor signal. Two similar sites are fine — three means extract. Three similar lines are better than a premature abstraction.

**Constants and protocols:** Define wire formats, ref patterns, message tags, and protocol pieces as constants or types in a single location. When one module writes a format and another reads it, they must share a schema — a shared type with `serialize`/`parse` methods, a Zod schema, a const enum, whatever fits the language. Ad-hoc string matching between writer and parser is a bug waiting to happen.

**Documentation:** Document public APIs and complex logic. Never add docstrings, comments, or type annotations to code you did not change.

## Error Handling

Use structured errors with actionable context. Core logic must not silently swallow errors. When intentionally ignoring a fallible operation, log a diagnostic and add a `NOTE:` comment explaining why. Silent error suppression hides operational issues.

Exception: fire-and-forget channel sends on shutdown paths may be silently ignored.

## Dead Code

Remove dead code rather than suppressing it. Remove unused parameters, fields, and accessor methods rather than keeping them for hypothetical future use. If both branches of a conditional return the same value, collapse them.

## Module Boundaries

One file, one concern. Functions that do N things should be N functions.

Multi-step workflows must have a single orchestration owner. Shared helpers are fine; duplicated orchestration paths are not. When the same lifecycle sequence (load state → operate → save state) appears in two modules with divergent fallback behavior, consolidate into a shared orchestrator to prevent behavior drift.

Read/query logic stays separate from state-mutating logic. A module that lists or reads data should not also perform destructive side effects. APIs with side effects must make that explicit in naming and docs.

## Iteration Style

Prefer declarative transforms (map, filter, reduce, flatMap, filter_map, collect) over imperative loops with manual match/continue/break when the loop body is "unwrap or skip" repeated across elements. The declarative style is more readable and harder to get wrong.

## Constructor / Factory Patterns

Avoid duplicating initialization logic. Chain specialized constructors to a primary one, or use builder/factory patterns, so default values and computed fields are defined in exactly one place.

## Dependencies

Use established libraries instead of reimplementing standard logic. Date/time formatting, diff parsing, structured serialization — these have well-tested solutions. Hand-rolled alternatives are maintenance liabilities.

## Comments

Add comments only for difficult-to-understand or critical code. Explain *why*, not *what*.

- Prefix with `NOTE:` or `TODO:`, one topic per comment.
- Before modifying complex code, search for existing comments to understand context.
- Update or remove comments when the code they describe changes.

## Commit Messages

Follow [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/) and explain the reasoning behind the change. Include the correct co-author trailer:

```
Co-Authored-By: Claude <noreply@anthropic.com>
Co-Authored-By: Gemini <google@users.noreply.github.com>
Co-Authored-By: ChatGPT <openai@users.noreply.github.com>
```
