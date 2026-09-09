# Design documents

One line each. Read the `README.md` at the repository root first; it says what
lvu does for a user. These say how and why.

## Orientation

- [`architecture.md`](architecture.md) — the implementation map: crates, data flow, persistence, the expression boundary, terminal rules.
- [`development.md`](development.md) — building, validating and repository conventions; mise tasks and the PTY harness.
- [`contracts.md`](contracts.md) — the shared semantic boundaries between crates: identities, records, capture, queries, JSON shapes.
- [`implementation-plan.md`](implementation-plan.md) — the original requested scope and milestone exit gates; not evidence that a feature exists.
- [`work-ledger.md`](work-ledger.md) — verified progress by assignment, owned by the primary agent; older entries in [`work-ledger-archive.md`](work-ledger-archive.md).
- [`gardening-2026-09-08.md`](gardening-2026-09-08.md) — the inventory behind the 2026-09-08 documentation cleanup: every non-source file, its references and its verdict.

## The terminal interface

- [`dialog-system.md`](dialog-system.md) — the specification every dialog follows: classes, sizing, message row, controls, the shared rules (§8) and one section per dialog (§12).
- [`component-model.md`](component-model.md) — how the shell hosts dialogs as components with a layer stack, outcomes and view events, and the order the dialogs were converted in.
- [`dialog-default-actions.md`](dialog-default-actions.md) — audit of every dialog against the default-action rule (§8.9).
- [`dialog-discoverability.md`](dialog-discoverability.md) — audit of every dialog and the base screen against the discoverability rule (§8.10).
- [`field-exploration.md`](field-exploration.md) — audit and design for nested JSON, value exploration and the field path picker (§8.11–§8.13).
- [`raw-context-as-jump.md`](raw-context-as-jump.md) — design, being built by W21: replace the Raw context dialog with a jump to All events and a way back.
- [`larger-ask-sample.md`](larger-ask-sample.md) — design, built: let a short Ask request re-run against a wider bounded sample when the first one was demonstrably thin (§12.17).
- [`dialog-design.md`](dialog-design.md) — stub; its control rules moved into `dialog-system.md` §8.15 and its audit is in git history.

## Data, queries and enrichment

- [`command-enrichment.md`](command-enrichment.md) — external command steps in the enrichment chain: the JSON Lines protocol, limits, durability, recipes, storage, and the decisions behind the chain model (§8.14).
- [`merged-view-ordering.md`](merged-view-ordering.md) — design for interleaving a multi-source view by time, with the invariants a merge must keep.
- [`text-time-basis.md`](text-time-basis.md) — reading a text enrichment column as the event-time basis with an explicit format.
- [`storage.md`](storage.md) — what lvu stores, what it will delete under pressure, and what it refuses to delete.
- [`settings.example.toml`](settings.example.toml) — an annotated example of the global settings file.
- [`performance.md`](performance.md) — the local capture and query baseline, the 2026-09-08 filter analysis, and how to run the benchmarks.

## Releasing

- [`distribution.md`](distribution.md) — packaging: archive layout, resource resolution, Homebrew and mise, and the release sequence.
- [`release-runbook.md`](release-runbook.md) — the exact commands that publish a release and cut the next one.
- [`portability.md`](portability.md) — audit of macOS and Windows readiness, read from source without a build on either.
- [`mac-test-plan.md`](mac-test-plan.md) — a macOS acceptance checklist an agent can execute against the tap build.
- [`versions.md`](versions.md) — the single versioned release scheme and workspace schema compatibility table.
