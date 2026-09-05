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

This package does not provide an alternate query engine. Literal search and
advanced Polars requests report that the native adapter is not connected and
leave the raw view unchanged. Discovery is linked for the eventual application
composition, but candidate browsing is intentionally deferred from this narrow
source-launch preview.
