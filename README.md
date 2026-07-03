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

## Install And Setup

```sh
cargo install --path .
hector init
hector frontier-brief
hector plan \
  --task "Add a focused Bob slice" \
  --verify "cargo test focused_slice" \
  --editable-path src/lib.rs \
  --reference-path tests/focused_slice.rs \
  --out campaign.yaml
hector check --file campaign.yaml
bob campaign --file campaign.yaml
hector review --campaign campaign.yaml --bob-result result.json
hector mcp
```

`hector init` writes a starter `hector.yaml`. If the file already exists,
Hector refuses to overwrite it unless you pass `hector init --force`.

`hector.yaml` supplies config defaults for `hector plan`:

```yaml
scope:
  default_max_changed_files: 2
  default_max_changed_lines: 160
judge:
  default_policy: retry_on_fail
bob:
  campaign_auto_commit: true
```

CLI flags override config defaults. `--no-auto-commit` always wins.

Config is searched at `./hector.yaml` first, then `~/.config/hector/config.yaml`
(same shape), so LAN model endpoints don't need copying into every repo.
`hector doctor` shows which file is in effect; `hector doctor --probe` also
curls each model endpoint and exits non-zero if any are dead.

In a Hector context, "run it" means run `hector check` and the repo tests.
Do not run Bob unless implementation or campaign execution is explicitly
requested. For Bob multi-slice campaigns, `auto_commit: true` requires a clean
checkout; stop on a dirty tree or run Bob from a clean throwaway worktree.

`frontier-brief` prints the exact contract frontier models should follow when asking Hector for a campaign. The same text is also exposed as a repo skill at [skills/hector-frontier/SKILL.md](skills/hector-frontier/SKILL.md).

## Commands

- `hector plan` emits Bob-compatible campaign YAML, or `needs_input` JSON when required proof/scope is missing (exit code 2 — no campaign was written). With `--spec` and no `--verify`, a configured model writes the focused test; models are tried in rotation order and a model whose output loops, won't parse, or produces a test that can't load is dropped for the next one. Planning that produces no campaign always exits non-zero.
- `hector check` statically rejects weak or dangerous campaigns before Bob sees them.
- `hector dispatch` runs a campaign's slices as parallel `bob build` processes. Slices whose verify gates are already green on base are marked `already_landed` and skipped (safe re-dispatch after a partial landing). A dispatch with any failed slice or a red integration gate exits non-zero. With bob ≥0.4.0, unpinned slices sharing a tier in the same parallel batch are round-robined across the tier's member models (`bob models --json`) so parallel builds spread across endpoints instead of saturating the stats-best one; a slice-level `model:` pin opts out.
- `hector review` compares Bob's result JSON against the original campaign and returns `accept`, `accept_for_human_review`, `revise_campaign`, `split_task`, or `ask_human`.
- `hector doctor` reports the config in effect and configured models; `--probe` checks each endpoint is alive.
- `hector frontier-brief` gives orchestrators the full handoff prompt.
- `hector frontier-brief --compact` gives orchestrators a low-token handoff.
- `hector init` writes a starter `hector.yaml`.
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
