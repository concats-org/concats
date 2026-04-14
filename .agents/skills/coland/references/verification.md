# Verification

## Shipping Loop

1. From the repository root, run `coland check <target> --format json`.
2. Inspect the findings and handle one finding at a time.
3. Apply automatic cleanup only for rules marked `safe_apply` below and only after local validation passes.
4. Treat `guided_apply` rules as proposals to explain, not automatic edits.
5. Rerun the originating rule and the listed follow-up rules after each applied fix.
6. Stop if validation fails, findings overlap, or the structure is ambiguous.

## Rule Modes

- `dead_private_function`: `safe_apply`
- `private_trivial_forwarder`: `safe_apply`
- `single_caller_private_zero_param_function`: `guided_apply`
- `middle_man`: `detect_only`
- `single_caller_private_function`: `detect_only`
- `single_caller_public_function`: `detect_only`
- `unreferenced_public_function`: `detect_only`
- `coupled_to_many_modules`: `detect_only`
- `cross_module_coupling`: `detect_only`
- `mixed_abstraction_callees`: `detect_only`

## Stop Rules

- Do not batch overlapping edits.
- Do not synthesize shared test helpers from duplication alone.
- Do not auto-apply public API cleanup.
- Stop on failed validation, unclear call-site rewriting, or ambiguous ownership.

## Recipe: delete_function_definition

Mode: `safe_apply`

Required finding shape:

- `file`
- `name`
- `start_line`

Local validation:

- private function definition
- zero incoming uses in the current codebase view
- not referenced from tests
- not referenced through macro or reflection-like surfaces detectable in source

Rewrite:

- delete the private function definition only
- do not remove adjacent items speculatively

Rerun rules:

- `dead_private_function`
- `unreferenced_public_function`

## Recipe: collapse_private_forwarder

Mode: `safe_apply`

Required finding shape:

- `file`
- `name`
- `start_line`
- `start_col`
- `end_line`
- `end_col`
- `param_count`
- `body_lines`
- `num_callees`

Local validation:

- private function definition
- single forwarding call body
- arguments forwarded 1:1
- no validation, branching, mutation, or extra statements

Rewrite:

- rewrite validated call sites to the callee
- remove the wrapper only after the call-site rewrite is complete

Rerun rules:

- `private_trivial_forwarder`
- `middle_man`
- `dead_private_function`

## Recipe: inline_zero_param_helper

Mode: `guided_apply`

Required finding shape:

- `file`
- `name`
- `start_line`
- `start_col`
- `end_line`
- `end_col`
- `body_lines`
- `branch_count`
- `num_callers`

Local validation:

- same-file private helper
- zero parameters
- single call site
- small body without control-flow hazards

Rewrite:

- explain the proposed inline rewrite at the single call site
- do not apply automatically

Rerun rules:

- `single_caller_private_zero_param_function`
- `single_caller_private_function`
- `dead_private_function`
