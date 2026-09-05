# Implementation work ledger

## Verified baseline

- Repository initially empty on unborn main, no pre-existing user changes.
- User authorized full plan, Paseo implementer subagents, independent testing,
  primary review/integration, isolated worktrees and coherent local commits.
- Configured profile: GPT-5.6-Sol-Implementer → codex/gpt-5.6-sol, full-access.
- Rust was not found on the initial shell PATH; toolchain setup is primary-owned.
- No implementation milestone is complete yet.

## Initial ownership

| Package | Owner | Owned paths | Gate |
|---|---|---|---|
| Plan/contracts/integration | Primary | docs/, AGENTS.md, README.md, root orchestration | Review all integration |
| M1A capture foundation | Paseo core implementer | Cargo.toml, Cargo.lock, crates/lvu-core/, .gitignore | Lossless recovery + file/command fixture tests |
| M1B Polars compatibility | Paseo expression implementer | spikes/polars-interop/, python/ | Real differential Python/Rust execution |
| M1C Paseo bridge | Paseo bridge implementer | bridge/ | Protocol tests + one local provider proof |

Each assignment must return actual test evidence and a coherent topic commit.
Primary alone marks gates verified and integrates. Later chunks are assigned only
after dependencies and shared interfaces have been reviewed.
