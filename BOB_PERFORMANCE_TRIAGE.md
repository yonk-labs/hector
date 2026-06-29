# Bob Performance Triage

## Findings

- Bob is not using Minimax by default here. The primary builder is `qwen-193`; fallbacks were empty.
- Raw qwen-193 HTTP latency is fine: a tiny `/v1/chat/completions` request returned in about 0.07s.
- Raw gemma HTTP latency is also fine: about 0.09s.
- gemma-133 works through opencode: a trivial one-file edit completed in about 6.6s.
- Bob fallback to gemma-133 works: forcing an invalid primary model retried with gemma-133 and converged on a trivial edit in about 15.3s.
- qwen-140 can lazy-load MLX models. Keep it in the roster for explicit tests, but do not use it as automatic fallback until we choose which model/load behavior is acceptable.
- Direct `opencode run` with qwen-193 on a trivial one-file edit completed in about 8.4s.
- `bob build` with qwen-193 on the same trivial one-file edit completed in about 16.2s, including Abe advisory.
- Direct `opencode run` with qwen-193 on the Hector config-loader prompt took about 31.6s and produced a compile-broken diff.
- `bob build` with qwen-193 on the same Hector config-loader prompt took about 41.0s and failed verification.

## Diagnosis

The slowdown is not Minimax, and it is not raw model reachability.

The slow path is opencode/qwen taking multiple tool loops on non-trivial Rust edits, then producing a bad cross-slice diff. Bob adds some overhead, but the same prompt is already slow and wrong when run directly through opencode.

Bob also makes this feel worse because artifacts only record labels like `verify-failed`; they do not preserve verify stdout/stderr or per-stage timing.

## Immediate Config

This repo now has `bob.yaml` with:

- qwen-193 as primary
- gemma-133 as first fallback
- no Minimax fallback
- qwen-140 listed but not used as automatic fallback
- `cargo test` as the default verify gate
- shorter timeouts
- advisory Abe
- tighter default scope caps

For focused runs, still pass explicit `--verify`, `--allow-path`, and scope caps.

## Bob Fixes Needed

1. Print progress with elapsed time for builder, scope, verify, and judge.
2. Persist verify stdout/stderr in artifacts.
3. Persist per-stage timing in JSON artifacts.
4. Detect opencode stalls by lack of file changes or log activity, not just process timeout.
5. Treat fallback models as disabled unless explicitly configured for the run.
6. For campaign mode, print each slice start/end immediately instead of returning only final JSON.

## Hector/Bob Handoff Fixes

Hector should keep Bob slices smaller than the failed config slice:

- one slice for config loader
- one slice for CLI arg optionality
- one slice for main wiring
- one slice for docs

If a slice requires changing CLI argument types, the spec must say that explicitly and include `src/cli.rs` in `editable_paths`.
