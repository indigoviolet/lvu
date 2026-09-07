# Storage: ownership, retention and pressure

This describes how lvu accounts for what it stores, what it will delete, and
what it refuses to delete. It covers `crates/lvu-app/src/storage/` — `ledger`,
`ownership`, `retention`, `pressure` and `governance` — and the `[storage]`
section of the settings TOML. The published Storage dialog still uses the older
`storage.rs` scan, which only reclaims verified-unused derived indexes; the
modules here are the durable-data half and are wired by the Storage screen owner.

## What is durable and what is disposable

| Class | Durability | Where |
| --- | --- | --- |
| Captured records | durable | `<capture-root>/<source-uuid>/capture.journal` and siblings |
| Investigation exports | durable | `<capture-root>/investigations/<dir>/` |
| Workspace, recipes, drafts | durable | `<capture-root>/workspace/` |
| Command-enrichment results | durable | workspace SQLite |
| Derived row indexes | disposable | `$XDG_CACHE_HOME/lvu` |
| Row cache, query membership | disposable | memory |

Command output is durable on purpose. An arbitrary external command is not
guaranteed to reproduce its result, so its output is retained derived data and
never appears in a reclaim list or a "space you can free" figure.

A durable class always reports zero reclaimable bytes. That is enforced in
`CacheUsage::new`, not by convention, so a durable total cannot be summed into
a reclaimable one by accident.

## Ownership and pins

An investigation manifest lists the sources, generations and capture
high-watermarks it was frozen against. Those references are *pins*.

* A capture with any pin cannot be deleted. The refusal names the investigation
  and the record it depends on, and suggests deleting that investigation first.
* An exported dataset covering the pinned range is reported in the preview, but
  it does not authorize deletion. Parquet is a projection; the journal holds the
  original bytes, delimiters and invalid UTF-8, and a projection is not a copy
  of the only original.
* An investigation whose manifest exists but cannot be read has *unknown*
  dependencies. It blocks every capture deletion. Unknown is never treated as
  absent. An investigation with no manifest at all never published a dataset and
  pins nothing.
* A capture whose journal lock is held, or which the application lists as open,
  is refused until the source is stopped. If activity cannot be probed at all,
  the capture is treated as active.
* Deleting an investigation releases its pins; the preview lists the captures
  that become deletable.

Path identity is verified before every removal: the target must sit directly
under its expected parent, its directory name must match its identity, and a
symbolic link is unlinked, never descended.

## Recorded deletion boundaries

`<capture-root>/deletions.jsonl` is an append-only JSON Lines ledger,
`schema_version = 1`. Every deletion writes an *intent* entry and syncs it
before the first unlink, then a *completed* or *failed* entry afterwards.

* Intent + completed → a recorded gap: "the whole capture of X was removed by
  requested/retention (records from N onward, S freed)."
* Intent + failed → the item is partially removed and reported as incomplete.
* Intent with no settlement → the process died mid-removal. The gap is reported
  as interrupted, not as complete and not as absent.

A deletion whose intent cannot be recorded is refused and removes nothing: a
gap that is not recorded would let the application imply uninterrupted capture
across data it destroyed. Reads of the ledger are bounded; a torn final line is
skipped without discarding earlier history.

The recorded range is deliberately conservative. The first sequence and capture
time come from a bounded probe of the journal head. The last sequence is left
unrecorded rather than inferred from the writer's reservation watermark, which
is an upper bound and would overstate how many records were removed.

## Retention

Retention is opt-in, configured in `[storage.retention]`, and defaults to off:
nothing captured is ever deleted automatically. Enabling it without any limit is
rejected at validation rather than presenting an active policy that does
nothing.

Rules are per source (by name or UUID) and global, by size and/or age. Age is
measured from the last capture activity. Selection is oldest-first.

Retention only ever deletes a **whole capture**. The journal is a single
append-only file whose byte offsets address every record; there is no segment
boundary to cut at, so trimming its head would either break addressing or leave
readers silently reading holes. Sub-journal trimming needs journal segmentation
in `lvu-core` and is deliberately not attempted here.

Every retention deletion runs through the same ownership check and the same
ledger as an explicit one. A capture a rule selects but ownership refuses is
reported as protected and unreclaimable, and when the configured limit still
cannot be met the assessment states the remaining shortfall. Finite storage
cannot promise unlimited lossless acquisition; the limit is stated rather than
met by deleting protected data.

## Cache pressure

Escalation is fixed and never skips a step:

1. **Reclaim** disposable caches — derived indexes first, then row cache and
   query membership.
2. **Backpressure** when reclaiming everything disposable still leaves free
   space below `storage.reserve_mib`.
3. **Stop acquisition** with a visible error when nothing disposable is left.

The stop message states plainly that records arriving now are not captured. A
capture stopped this way is a stopped capture, not a complete one. Captured
records are never evicted to make room, at any level.

## Settings keys

```toml
[storage]
reserve_mib = 256

[storage.retention]
enabled = false
maximum_total_capture_mib = 0 # 0 = no limit
maximum_age_days = 0          # 0 = no limit

[[storage.retention.source]]
match = "noisy-source"        # source name or UUID
maximum_capture_mib = 512     # 0 = no limit
maximum_age_days = 7          # 0 = no limit
```

Bounds: `reserve_mib` and the size limits are at most 1 TiB, ages at most 36500
days, at most 32 source rules, no duplicate `match`, and a source rule must set
at least one limit. A settings file written before this section existed loads
with the defaults, which keep every captured record.

## Bounds

Scans and removals are bounded and cancellable: at most 256 captures, 256
investigations, 1024 entries per directory, 8192 files, 2048 directories, depth
6, 32 reported errors, 8 MiB per manifest, 4096 ledger entries. Reaching a limit
sets `truncated`, and totals are then reported as a lower bound rather than a
figure. Cancellation is checked between filesystem operations; it cannot
interrupt a blocked kernel call.
