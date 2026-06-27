# Hector

Hector is the TDD/spec planner for Bob campaigns.

It turns product intent into small, deterministic slices with focused gates and frozen editable scope. Hector writes or identifies tests/specs; Bob writes production code.

Status: feature-complete MVP. See [HECTOR_SPEC.md](HECTOR_SPEC.md).

## Lifecycle Role

Use Hector before Bob when a request needs to be converted from "build this" into exact executable proof. A frontier orchestrator owns product judgment, Hector turns that judgment into a tight campaign, Bob implements it, and Abe or a human reviews uncertain results.

Hector does not write production code. Its job is to make the implementation task boring enough for a cheaper builder model to run safely:

- define one observable behavior per slice
- write or identify the deterministic verify command
- keep test/spec files as reference-only
- freeze Bob's editable paths and scope caps
- review Bob's result against the original contract

## Quick Start

```sh
cargo run -- frontier-brief
cargo run -- plan \
  --task "Add a focused Bob slice" \
  --verify "cargo test focused_slice" \
  --editable-path src/lib.rs \
  --reference-path tests/focused_slice.rs \
  --out campaign.yaml
cargo run -- check --file campaign.yaml
cargo run -- review --campaign campaign.yaml --bob-result result.json
cargo run -- mcp
```

`frontier-brief` prints the exact contract frontier models should follow when asking Hector for a campaign. The same text is also exposed as a repo skill at [skills/hector-frontier/SKILL.md](skills/hector-frontier/SKILL.md).

## Commands

- `hector plan` emits Bob-compatible campaign YAML, or `needs_input` JSON when required proof/scope is missing.
- `hector check` statically rejects weak or dangerous campaigns before Bob sees them.
- `hector review` compares Bob's result JSON against the original campaign and returns `accept`, `accept_for_human_review`, `revise_campaign`, `split_task`, or `ask_human`.
- `hector frontier-brief` gives orchestrators a compact handoff prompt.
- `hector mcp` exposes `frontier_brief`, `plan_campaign`, `check_campaign`, and `review_result` over stdio MCP.

## Campaign Shape

```yaml
name: focused-behavior
auto_commit: true
slices:
  - name: focused-behavior
    task: Implement the smallest code change that proves the focused behavior.
    spec: |
      Include exact formulas, edge cases, invalid states, and examples here.
    verify_cmds:
      - cargo test focused_behavior
    editable_paths:
      - src/domain.rs
    reference_paths:
      - tests/domain_behavior.rs
    judge_policy: retry_on_fail
    max_iters: 4
    max_changed_files: 1
    max_changed_lines: 120
```
