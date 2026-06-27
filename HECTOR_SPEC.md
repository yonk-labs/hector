# Hector Spec

## TLDR

Hector is the TDD/spec agent in the coding-agent lifecycle. Hector turns product intent into Bob-sized executable slices: focused tests or verify gates, frozen editable scope, reference context, and a campaign file Bob can run.

Hector does not implement production code. Bob implements. Abe judges. Greta reviews UX/design. A frontier orchestrator owns product decisions and sequencing.

## Product Boundary

Hector owns:

- finding the smallest objectively verifiable slice
- writing or identifying the focused test/gate
- proving new tests fail before implementation when practical
- freezing editable paths so Bob cannot edit the test to pass
- emitting Bob campaign YAML/JSON
- linting a slice for scope drift, dependency creep, and weak gates
- carrying forward project lessons from `.bob/lessons.md` and `.hector/lessons.md`

Hector does not own:

- production implementation
- broad architecture decisions
- UX decisions that need Greta or a human
- running Bob autonomously without an orchestrator decision
- creating dependency or lockfile churn unless explicitly requested
- replacing the repo's existing test framework

## Core Workflow

1. Read user intent, referenced plans, local conventions, and project lessons.
2. Decide whether the request is testable. If not, return `human_questions`.
3. Split work into the smallest deterministic slices.
4. For each slice, choose or create one focused gate.
5. Freeze `editable_paths` separately from `reference_paths`.
6. Produce a Bob campaign.
7. Optionally run `hector check` to reject weak or dangerous slices.

The output should be boring enough for a cheap builder model to execute and strict enough that cheating is hard.

## CLI Surface

```text
hector plan --task "..." [--spec file.md] [--out campaign.yaml]
hector check --file campaign.yaml
hector init
hector mcp
```

`plan` emits a campaign, but may return `human_questions` instead of slices when the desired behavior is ambiguous.

`check` validates an existing campaign before Bob sees it.

`init` writes a starter `hector.yaml`.

`mcp` exposes the same operations to a frontier orchestrator.

## MCP Tools

### `plan_campaign`

Input:

```json
{
  "task": "Add a roster-plan API client",
  "spec": "optional longer context",
  "files": ["src/routes/api/roster-plan.js"],
  "mode": "campaign"
}
```

Output:

```json
{
  "status": "planned",
  "campaign": { "...": "Bob campaign object" },
  "human_questions": [],
  "warnings": []
}
```

### `check_campaign`

Input:

```json
{ "campaign": { "...": "Bob campaign object" } }
```

Output:

```json
{
  "status": "pass",
  "findings": []
}
```

Findings use `error` for blockers and `warning` for orchestrator review.

## Campaign Output

Hector emits Bob-compatible campaign YAML:

```yaml
name: roster-plan-client
auto_commit: true
slices:
  - name: roster plan client
    task: Add a tiny framework-free roster-plan API client.
    spec: |
      Create only public/js/roster-plan-client.js.
      Do not add dependencies.
      All helpers must call the injected fetchImpl.
    verify_cmds:
      - node -e "..."
    editable_paths:
      - public/js/roster-plan-client.js
    reference_paths:
      - src/routes/api/roster-plan.js
    judge_policy: retry_on_fail
    max_iters: 4
    max_changed_files: 1
    max_changed_lines: 120
```

## Slice Rules

Every slice must have:

- `task`: self-contained implementation instruction
- `verify_cmds`: deterministic command Bob can run
- `editable_paths`: files or directories Bob may change
- `reference_paths`: files Bob may read for conventions
- `max_changed_files` and `max_changed_lines`
- `judge_policy`, usually `retry_on_fail`

Default rules:

- Test files are reference-only unless the slice is explicitly "write the test."
- Dependency and lockfile changes are forbidden unless the slice explicitly allows them.
- A focused gate should prove product behavior, not incidental implementation shape.
- If a new test is written, Hector should prove it fails before Bob starts.
- If the gate fails for a spec mistake, fix Hector's gate before blaming Bob.

## Lessons From The First Bob/Hector Field Run

- Scope caps are not optional. They blocked `jsdom`, `package.json`, `package-lock.json`, and mock-file churn from a tiny client slice.
- Gates should assert contracts, not arbitrary call-object equality. Rejecting harmless `method: "GET"` was a Hector error.
- Verify failures should become sharper spec text. When Bob used global `fetch` instead of injected `fetchImpl`, the next spec named that exact failure.
- Inline `node -e` gates are acceptable for tiny helper slices. Do not add a test dependency just to test a one-file utility.
- `needs_review` from Bob means the objective gate passed but judge confidence stayed low. The orchestrator decides whether to apply or send a narrower follow-up slice.

## Refusal Boundaries

Hector returns `human_questions` instead of slices when:

- the desired product behavior is ambiguous
- the requested change depends on a UX/design decision
- no deterministic verification command exists
- the only useful test would lock in a bad design
- the task is a broad refactor instead of a bounded behavior
- the slice needs dependency churn that the user did not explicitly allow

## Config

`hector.yaml`:

```yaml
verify:
  prefer_focused: true
scope:
  forbid_dependency_churn: true
  default_max_changed_files: 2
  default_max_changed_lines: 160
judge:
  default_policy: retry_on_fail
bob:
  campaign_auto_commit: true
```

Config is project policy. Per-plan CLI/MCP inputs may narrow scope, but should not silently loosen it.

## Implementation Plan

1. Define campaign and finding structs.
2. Implement `hector check` for static campaign validation.
3. Add repo convention discovery: package manager, test runner, language, existing test folders.
4. Add `plan` as a conservative planner that can emit `human_questions`.
5. Add MCP wrappers over `plan` and `check`.
6. Add optional red/green probe support for newly written tests.

Do not implement production-code generation in Hector.
