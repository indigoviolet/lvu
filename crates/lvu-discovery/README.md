# lvu-discovery

Read-only, bounded local source discovery for lvu. `discover` coordinates Linux
`/proc`, Docker CLI, project traversal, and caller-supplied recent definitions.
It returns ordinary `lvu_core::SourceDefinition` values with merged evidence,
availability, confidence, stable identity hints, and a deterministic fingerprint.

Docker candidates use explicit executable/argument vectors and follow one container
ID each. Compose project/service/replica labels provide stable grouping and memory
hints without collapsing selectable replicas. Process discovery reads metadata,
command lines, descriptor links and access flags only. It never opens a descriptor
target or reads log payloads. Project traversal does not follow symlinks.

The crate is temporarily a standalone Cargo workspace. The repository primary
will add it to the root workspace and reconcile its lockfile during integration.
