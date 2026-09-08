# Design documents

One line each. Read the `README.md` at the repository root first; it says what
lvu does for a user. These say how and why.

## Orientation

- [`architecture.md`](architecture.md) — the implementation map: crates, data flow, what is published and what is only on main.
- [`development.md`](development.md) — building, validating and repository conventions; mise tasks and the PTY harness.
- [`contracts.md`](contracts.md) — the shared semantic boundaries between crates: identities, records, capture, queries, JSON shapes.
- [`implementation-plan.md`](implementation-plan.md) — the full requested scope, including work not delivered; not evidence that a feature exists.
- [`work-ledger.md`](work-ledger.md) — verified progress by assignment, owned by the primary agent.
- [`supervisor-handoff.md`](supervisor-handoff.md) — an operational snapshot for whoever supervises the implementers next.
- [`module-partition.md`](module-partition.md) — how `crates/lvu` is split so several implementers can work without colliding.

## The terminal interface

- [`dialog-system.md`](dialog-system.md) — the specification every dialog follows: classes, sizing, message row, controls, the shared rules (§8) and one section per dialog (§12).
- [`dialog-design.md`](dialog-design.md) — the earlier control rules (bounded buttons, visible caret, keyboard policy) that `dialog-system.md` reconciles and supersedes for layout.
- [`component-model.md`](component-model.md) — how the shell hosts dialogs as components with a layer stack, outcomes and view events, and the order the legacy dialogs were converted in.
- [`dialog-default-actions.md`](dialog-default-actions.md) — audit of every dialog against the default-action rule (§8.9).
- [`dialog-discoverability.md`](dialog-discoverability.md) — audit of every dialog and the base screen against the discoverability rule (§8.10).
- [`field-exploration.md`](field-exploration.md) — audit and design for nested JSON, value exploration and the field path picker (§8.11–§8.13).
- [`command-chain.md`](command-chain.md) — command steps as ordered steps of the enrichment chain: design, audit against the invariants, and the decisions (§8.14).
- [`raw-context-as-jump.md`](raw-context-as-jump.md) — design, pending decision: replace the Raw context dialog with a jump to All events and a way back.
- [`dialog-system-captures.md`](dialog-system-captures.md) — PTY captures of the dialogs, the evidence behind `dialog-system.md`.
- [`dialog-audit-captures.md`](dialog-audit-captures.md) — raw `TestBackend` captures behind the audit in `dialog-design.md`.

## Data, queries and enrichment

- [`command-enrichment.md`](command-enrichment.md) — external command steps: the JSON Lines protocol, limits, durability of attempts and results, recipes and storage.
- [`merged-view-ordering.md`](merged-view-ordering.md) — design for interleaving a multi-source view by time, with the invariants a merge must keep.
- [`text-time-basis.md`](text-time-basis.md) — reading a text enrichment column as the event-time basis with an explicit format.
- [`storage.md`](storage.md) — what lvu stores, what it will delete under pressure, and what it refuses to delete.
- [`performance.md`](performance.md) — the local capture and query baseline and how to run the benchmark.

## Releasing

- [`distribution.md`](distribution.md) — packaging: archive layout, resource resolution, Homebrew and mise, and the release sequence.
- [`portability.md`](portability.md) — audit of macOS and Windows readiness, read from source without a build on either.
- [`previews.md`](previews.md) — the development preview history and per-version compatibility notes.
