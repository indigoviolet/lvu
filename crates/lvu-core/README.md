# lvu-core

This crate owns stable capture identities, source definitions, the raw journal,
and bounded file/command/owned-reader acquisition. Journal frame headers and bodies are
independently checksummed. `flush`
means userspace buffering was drained; `sync_data` requests durable file data.
Sequence watermarks are reserved in durable blocks before frames are appended,
so a crash or restart may create a gap but cannot reuse a sequence. Recovery
truncates only an incomplete final frame; malformed or checksum-invalid complete
frames fail as committed corruption.

A journal enforces one owning writer through an in-process registry of
claimed lock paths plus a POSIX record lock (`F_SETLK`) across processes; a
plain `flock` was replaced because a forked child inherited it through the
pre-exec window and refused its parent's restart. Recovery scans one
bounded frame at a time, and retained records are exposed through bounded pages.
Page byte limits are soft for the first record: one frame-sized record may exceed
the requested byte count so a caller can always make progress.

Acquisition applies bounded channels and read buffers. A logical line larger than
`maximum_record_bytes` becomes ordered `Start`/`Continue`/`End` records. Pending
bytes are emitted after `partial_flush_interval`; a later delimiter completes the
same fragment sequence. Rejoining fragment bytes and the final delimiter exactly
reconstructs the input.

`CaptureHandle::stop` stops new reads, drains bytes already read through the event
queue, and reaps owned commands. `abort` (and its compatibility alias `cancel`)
instead prioritizes prompt teardown and may discard bytes that could not enter a
full event queue. Neither operation journals or fsyncs by itself; that durability
boundary belongs to the ingest runtime.

File acquisition also has an additive `capture_file_from` entry point. Its resume
cursor binds a byte offset to filesystem identity, the exact trailing 4 KiB, and
a checksum of the complete acknowledged prefix. A matching regular file starts
at that offset; identity, length, or content mismatches restart at byte zero with
an explicit rotation/truncation boundary. Cursor checkpoint events never cover
bytes still buffered only inside the framer.

File capture detects gzip from the `1f 8b` magic bytes, never from the filename.
Gzip members are decoded through a bounded worker channel and feed the same raw
framer, so the journal preserves decoded invalid bytes and delimiters exactly.
Concatenated members are supported. Gzip files have static archive semantics:
archive EOF completes capture even when file follow was requested.
Gzip capture requires a regular file. Fingerprinting and decoding use the same
open handle, with compressed reads capped at 1 KiB between lifecycle checks.
Graceful stop prevents another compressed read and drains only decoded chunks
already accepted by the bounded channel; it does not decompress to archive EOF.
An individual regular-file read already executing in the kernel is not
preemptible, while abort interrupts decoder publication and owns worker joining.

`capture_reader` accepts an owned asynchronous reader for non-replayable stdin
sessions. It uses the same bounded framing and progressive partial emission as
the other acquisition paths, labels records `StreamKind::Stdin`, completes on
EOF, and drops the reader on stop or abort. It never opens process-global stdin.
