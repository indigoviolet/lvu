# lvu-core

This crate owns stable capture identities, source definitions, the raw journal,
and bounded file/command acquisition. Journal frame headers and bodies are
independently checksummed. `flush`
means userspace buffering was drained; `sync_data` requests durable file data.
Sequence watermarks are reserved in durable blocks before frames are appended,
so a crash or restart may create a gap but cannot reuse a sequence. Recovery
truncates only an incomplete final frame; malformed or checksum-invalid complete
frames fail as committed corruption.

A journal enforces one owning writer with an advisory lock. Recovery scans one
bounded frame at a time, and retained records are exposed through bounded pages.
Page byte limits are soft for the first record: one frame-sized record may exceed
the requested byte count so a caller can always make progress.

Acquisition applies bounded channels and read buffers. A logical line larger than
`maximum_record_bytes` becomes ordered `Start`/`Continue`/`End` records. Rejoining
their bytes and the final delimiter exactly reconstructs the input.
Cancellation is an abort: it promptly reaps owned work and may discard bytes that
were read but could not yet enter the bounded event queue. A future graceful stop
must explicitly drain acquisition and journal buffers before claiming losslessness.
