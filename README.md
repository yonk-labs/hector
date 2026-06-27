# Hector

Hector is the TDD/spec planner for Bob campaigns.

It turns product intent into small, deterministic slices with focused gates and frozen editable scope. Hector writes or identifies tests/specs; Bob writes production code.

Status: spec-first scaffold. See [HECTOR_SPEC.md](HECTOR_SPEC.md).

```sh
cargo run -- plan --task "Add a focused Bob slice"
cargo run -- check --file campaign.yaml
cargo run -- mcp
```
