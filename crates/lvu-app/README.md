# lvu-app composition

`lvu-app` is the temporary real-source executable. It composes the accepted
capture runtime with the live journal row provider and the Ratatui UI without
duplicating their acquisition, storage, or paging logic.

```text
lvu-app --capture-dir ./captures --file ./service.log
lvu-app --capture-dir ./captures --command 'make serve'
```

`--file` and `--command` are repeatable. Commands are explicitly executed as
`sh -c` with the application's current directory recorded in the source
definition. With no source arguments, `lvu-app` opens a bounded Add source
dialog; Tab switches between file and command input.

Press Ctrl-D in the source dialog to run a bounded, asynchronous discovery scan.
The scan combines Docker, Linux `/proc`, and the current project providers;
recent-source input is empty until persisted memory is composed. Type to filter
the returned candidate list, use the arrow keys to select, and press Enter to
explicitly start it. Discovery never starts a candidate autonomously. Ctrl-R
cancels any active generation and starts a fresh scan. Provider failures,
partial limits, timeouts, cancellation, and an empty result remain visible.

## Integration boundary

The UI carries an opaque candidate fingerprint while `lvu-app` retains the full
`DiscoveryCandidate`. Selection passes its authoritative `SourceDefinition`
directly to `SourceManager`; it does not reconstruct paths or Docker commands.
Normal source admission, duplicate reuse, registration rollback, and shutdown
remain shared with manual sources. A future memory composition can populate
`ProjectConfig::recent_sources` without changing the UI contract.

This package does not provide an alternate query engine. Literal search and
advanced Polars requests report that the native adapter is not connected and
leave the raw view unchanged.
