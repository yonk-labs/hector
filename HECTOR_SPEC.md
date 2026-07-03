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

## Orchestrator Input Contract

Hector should ask the orchestrator for only the fields needed to create an executable gate:

- desired observable behavior
- allowed editable areas
- forbidden changes
- verification command, if known
- whether dependency or lockfile changes are allowed
- UX/product decisions that must be settled before tests are written

If the orchestrator provides a full PRD, Hector breaks it down by proof, not by section. Each slice should answer: "What is the smallest behavior a command can prove?" A single PRD requirement can become multiple Bob slices when it needs multiple independent gates.

## Guardrail Layers

Hector uses layered guardrails:

- Input guardrails: block ambiguous behavior, missing proof, or unsettled product decisions.
- Campaign guardrails: require `verify_cmds`, `editable_paths`, scope caps, and non-empty tasks.
- Path guardrails: reject absolute paths, parent traversal, test paths, and dependency/lockfile paths unless explicitly allowed.
- Handoff guardrails: pass tests as `reference_paths`, implementation as `editable_paths`, and keep Bob's `max_changed_*` caps narrow.
- Result guardrails: compare Bob's `changed_files`, `scope`, `verify`, `judge`, `status`, and `next_action` against the original campaign before accepting work.

Abe-before-Bob is conditional. Run local `hector check` first. Ask Abe to review a campaign before Bob when the slice is broad, multi-file, touches tests, allows dependencies, or has ambiguous acceptance criteria.

## CLI Surface

```text
hector plan --task "..." [--spec file.md] [--out campaign.yaml]
hector check --file campaign.yaml
hector review --campaign campaign.yaml --bob-result result.json
hector frontier-brief
hector init
hector mcp
```

`plan` emits a campaign, but may return `human_questions` instead of slices when the desired behavior is ambiguous.

`check` validates an existing campaign before Bob sees it.

`review` compares Bob's result to Hector's intended campaign and decides whether to accept, revise, split, or ask a human.

`frontier-brief` prints the contract frontier orchestrators should follow when writing Hector-ready slices.

`frontier-brief --compact` prints a low-token version for repeated orchestration prompts.

`init` writes a starter `hector.yaml`. It refuses to overwrite an existing file unless `--force` is passed.

`mcp` exposes the same operations to a frontier orchestrator.

## MCP Tools

### `frontier_brief`

Output: the same frontier-orchestrator instructions as `hector frontier-brief`.

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

### `review_result`

Input:

```json
{
  "campaign": { "...": "original Bob campaign" },
  "bob_result": {
    "status": "needs_review",
    "next_action": "review_candidate",
    "changed_files": ["src/planner.rs"],
    "scope": { "within": true },
    "verify": { "passed": true },
    "judge": { "verdict": "uncertain", "critique": "..." }
  }
}
```

Output:

```json
{
  "decision": "accept_for_human_review",
  "revised_campaign": null,
  "findings": []
}
```

Allowed decisions:

- `accept`
- `accept_for_human_review`
- `revise_campaign`
- `split_task`
- `ask_human`

Review rules:

- If Bob changed files outside `editable_paths`, reject and revise scope/spec.
- If Bob edited frozen tests, fix the Hector test/spec first, then rerun Bob.
- If Bob returns `scope_exceeded`, split or tighten the slice.
- If Bob returns repeated verify failure, make the verify failure the next spec detail.
- If Bob returns `needs_review`, compare Abe's critique to the final diff and either accept for human review or write a narrower follow-up.
- If Bob adds dependency churn not explicitly allowed, reject.

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

Config values are config defaults consumed by the CLI. Per-command flags override them, and `--no-auto-commit` always forces `auto_commit: false`.

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

Config is searched at `./hector.yaml` first, then `~/.config/hector/config.yaml`.
`hector doctor` reports which file is in effect; `--probe` liveness-checks each
configured model endpoint.

## Failure Contract

Hector never signals success through exit code 0 when its artifact was not
produced. Specifically:

- `plan` that emits `needs_input` exits 2 and refuses to write `--out`.
- LLM planning rotates through configured models (default first); looping
  output, unparseable JSON (after one re-ask), and tests that cannot load
  (after infra retries) drop the model for the next one. All models failing
  fails the plan, and any test files a failed model wrote are removed.
- `dispatch` exits non-zero when any slice failed or an integration gate broke.
- `dispatch` pre-flights each slice's verify gates: already green on base
  means `already_landed` — skipped, counted as done, dependents proceed.

## Implementation Plan

1. Define campaign and finding structs.
2. Implement `hector check` for static campaign validation.
3. Add repo convention discovery: package manager, test runner, language, existing test folders.
4. Add `plan` as a conservative planner that can emit `human_questions`.
5. Add MCP wrappers over `plan` and `check`.
6. Add `review` over Bob structured results.
7. Add optional red/green probe support for newly written tests.

Do not implement production-code generation in Hector.
