# Code Quality Principles

Write code that is easy to understand, safe to change, and simple to test. Each principle below describes what good code looks like and which coland rules catch the opposite.

## Separate Actions from Calculations

Every piece of code is an **action** (depends on when or how many times it runs), a **calculation** (pure: same inputs, same outputs, no side effects), or **data** (inert values). Actions are infectious: a function that calls even one action becomes an action.

Maximize calculations and data. Minimize and isolate actions. When a function both decides what to do (branching) and does it (calling), split it: a calculation that returns a decision, and an action that dispatches on it.

**Coland catches:** `action_calculation_tangle` fires when a function has high branching and high callee count -- it is tangling decision logic with delegation. `mixed_abstraction_callees` fires when a function calls at mismatched abstraction levels.

## Keep Units Small and Focused

A function should do one thing, fit on a screen, and be nameable in a few words. Small units are easier to understand, test, reuse, and replace. When a function grows, extract.

Identify distinct responsibilities inside a long function -- validation, transformation, I/O, formatting -- and extract each into its own function. Replace deep conditional nesting with guard clauses. Replace long match chains with lookup tables or data-driven dispatch.

**Coland catches:** `function_too_large` and `function_large` fire on body size. `complex_unit` fires on branch point count. `high_branch_complexity` fires on high branching relative to size. `complex_private_bottleneck` fires on complex private functions called by many.

## Minimize Coupling

Loosely coupled code can be understood, tested, and modified in isolation. A function that calls many distinct functions knows too much about the system. Modules that depend on each other bidirectionally cannot be changed independently.

Group related calls into sub-orchestrators. Pass data (values), not control flags. Break bidirectional dependencies by extracting shared logic into a third module, or by having one side return data instead of calling back.

**Coland catches:** `god_function` and `hub_function` fire on functions with too many callees. `coupled_to_many_modules` and `cross_module_coupling` fire on excessive inter-module dependencies. `bidirectional_call_dependency` fires on mutual call relationships. `feature_envy` fires when a function uses another module's data more than its own.

## Delete Dead Code

Unused code is noise. It misleads readers, complicates refactors, and accumulates silently. Delete it -- version control preserves history.

**Coland catches:** `dead_private_function` and `dead_type` fire on unreferenced private items. `unreferenced_public_function` flags public functions with no callers. `single_caller_private_function` and `single_caller_public_function` flag functions that may be candidates for inlining. `middle_man` and `private_trivial_forwarder` flag functions that only forward to another.

## Keep Interfaces Narrow

Functions with many parameters are hard to call, hard to test, and hard to understand. A wide interface signals that the function is doing too many things or that its inputs need restructuring.

**Coland catches:** `excessive_parameters` and `too_many_parameters` fire on parameter count. `wide_module_interface` fires on modules exposing too many public functions.

## Use Idiomatic Patterns

Follow the idioms of the language. In Rust: propagate errors with `?`, not `.unwrap()`. Accept `&str` / `&[T]` in parameters, not `&String` / `&Vec<T>`. Prefer references over clones -- restructure ownership instead.

**Coland catches:** `unwrap_usage` fires on `.unwrap()` calls. `clone_heavy_function` fires on functions with excessive cloning.

## Immutability by Default

Never mutate data that was passed to you. Copy, modify the copy, return the copy. This turns writes into reads, and reads are calculations. Add `mut` only when clearly needed.

## Stratified Design

Organize code into layers where each layer only calls the layer below. Higher layers are closer to the business domain; lower layers are general utilities. Each layer provides an abstraction barrier that hides implementation details.

**Coland catches:** `deep_call_hierarchy` fires when call chains are excessively deep, suggesting missing intermediate layers.

## Explicit Dependencies

Parameters in, return values out. No hidden global reads, no mutation of shared state, no side effects invisible to callers. Explicit wiring makes code predictable and searchable.
