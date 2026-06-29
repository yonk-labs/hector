# Hector Install/Setup Plan

## Delivered State

- Hector already has `plan`, `check`, `review`, `frontier-brief`, `init`, and `mcp`.
- `init` writes a starter `hector.yaml` and refuses to overwrite without `--force`.
- `hector.yaml` provides partial config defaults for `hector plan`; omitted fields use built-in defaults.
- Install/setup docs are in `README.md`.

## Goal

Make Hector easy to install, initialize, configure, and hand to Bob without spending frontier tokens re-explaining the workflow.

Bob should run `hector-install-setup.campaign.yaml` in order. It covers:

1. A small config loader for `hector.yaml`.
2. Wiring `hector plan` to config defaults, including malformed-config failure.
3. Safe `hector init` plus install/setup/config docs.
4. A compact frontier brief for cheaper orchestrator prompts.

## Execution Preflight

In a Hector context, "run it" means run Hector validation: `hector check` and
the repo test suite. Do not invoke Bob unless the user explicitly asks for Bob,
implementation, or campaign execution.

Before any Bob multi-slice campaign, require a clean checkout. Bob requires
`auto_commit: true` for multi-slice campaigns, and that refuses dirty trees.
If the primary checkout has uncommitted work, either stop and report that, or
run Bob from a clean throwaway worktree and return the final diff.

## Config Contract

`hector.yaml` remains optional. If absent, current defaults stay unchanged.

Supported defaults for this pass:

```yaml
scope:
  default_max_changed_files: 2
  default_max_changed_lines: 160
judge:
  default_policy: retry_on_fail
bob:
  campaign_auto_commit: true
```

CLI flags override config values. Malformed config should exit with a clear error.

## Non-Goals

- No package-manager installer yet; `cargo install --path .` is enough.
- No release automation.
- No new dependencies.
- No autonomous Bob runner inside Hector.
- No new agent in this pass.

## Automation Recommendation

Do not create a new agent yet. First ship config defaults and `frontier-brief --compact`; those remove the repeat-token cost without adding another moving part.

Create a small orchestration agent later only if the same manual loop keeps recurring: gather intent, ask Hector for a campaign, run `hector check`, launch Bob, run `hector review`, and call Abe only when Hector flags uncertainty.

## Abe Review Notes

Abe validated the campaign shape and recommended two changes now included here:
test malformed `hector.yaml` handling, and keep `frontier-brief --compact` out of
the repo skill file so the tool behavior changes without mutating agent skill
instructions in the same slice. Bob then reported the config slice was too big,
so Hector split config loading from CLI wiring instead of raising scope caps.
