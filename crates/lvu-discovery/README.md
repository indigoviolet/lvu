# lvu-discovery

Read-only, bounded local source discovery for lvu. `discover` coordinates Linux
`/proc`, Docker CLI, project traversal, and caller-supplied recent definitions.
It returns ordinary `lvu_core::SourceDefinition` values with merged evidence,
availability, confidence, stable identity hints, and a deterministic fingerprint.

Docker candidates use explicit executable/argument vectors. Every container
remains independently selectable by stable Compose replica or container-name
identity. When Compose's recorded working directory/config files are locally
usable, discovery also returns one stable project/service aggregate that runs
`docker compose logs --follow` for all replicas. Missing remote-host paths omit
only that aggregate. Implicit discovery leaves `DOCKER_HOST`/`DOCKER_CONTEXT`
routing to the Docker CLI; only an explicitly configured context adds
`--context`. Endpoint text is represented in identities by a stable hash, not
stored as a hint or command argument.

Process discovery reads metadata, command lines, descriptor links and access
flags only. It never opens a descriptor target or reads log payloads. Project
traversal does not follow symlinks.
