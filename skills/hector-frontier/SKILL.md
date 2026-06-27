---
name: hector-frontier
description: Use when a frontier model or orchestrator needs to write Hector-ready Bob campaign slices with exact behavior contracts, verification commands, editable/reference paths, scope caps, and review guardrails.
---

# Hector Frontier

Use this skill when converting product intent, a PRD, a game rule, a tool idea, or a bug report into Hector-ready work.

Hector wants executable proof, not broad intent. Bob implements production code. Hector freezes the test/spec and editable scope. Abe or a human reviews ambiguous results.

## Required Output

Produce either a `hector plan` command or a Bob campaign YAML with:

- `task`: one self-contained observable behavior
- `spec`: exact rules, formulas, edge cases, expected failures, and examples
- `verify_cmds`: deterministic command(s) that prove the behavior
- `editable_paths`: production paths Bob may change
- `reference_paths`: tests/specs/docs Bob may read but not edit
- `judge_policy`: usually `retry_on_fail`
- `max_iters`, `max_changed_files`, and `max_changed_lines`

## Slice Rules

Use one proof per slice. Split broad features by observable outcome, not by PRD section.

For rules-heavy domains, specify the decisions Hector cannot infer safely: modifier order, stacking rules, rounding, target classes, invalid states, persistence wiring, and API/UI boundaries.

Do not let Bob edit the gate that proves the behavior. Tests, specs, and expected-output fixtures are reference-only unless the slice is explicitly "write the test."

## Ask For Input When

Return `needs_input` with `human_questions` if behavior is ambiguous, no deterministic verify command exists, UX/product decisions are unsettled, dependency churn is not explicitly allowed, or the request is too broad for one bounded proof.

## Handoff

```sh
hector plan --name NAME --task TASK --verify CMD --editable-path PATH --reference-path PATH --out campaign.yaml
hector check --file campaign.yaml
bob campaign --file campaign.yaml
hector review --campaign campaign.yaml --bob-result result.json
```
