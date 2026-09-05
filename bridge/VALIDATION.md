# M1C validation report

## Deterministic validation

Run with repository-managed Node 26.8.1:

```text
mise exec node@26.8.1 -- npm ci --prefix bridge --no-audit --no-fund
mise exec node@26.8.1 -- npm --prefix bridge run check
mise exec node@26.8.1 -- npm --prefix bridge run build
```

The final check passed TypeScript checking and 42 Vitest tests in five files.
Coverage includes streaming/correlation; all proposal kinds and generated schemas;
malformed, empty, stale, and oversized proposals; concurrent create/resume capacity
reservations; close during create; rejected and never-resolving SDK operations;
timeout/cancel/resume; remote-busy state and post-cancel event fencing; malformed,
oversized unterminated, duplicate, and excess requests; slow stdout; and cleanup.
Round-two regressions additionally cover SDK-resolved timeout/permission with a
still-running authoritative snapshot, running-to-idle resumed sessions, duplicate
resume coalescing, connect/close races, late-create archival, cancellation timeout
generation fencing, permanently stalled stdout shutdown, and exact CLI arguments.
The final boundedness regressions verify that timed-out creates retain capacity
while the SDK promise is unresolved, release it only after late rejection or
successful archival, retain and report failed archival reconciliation, and let
bridge shutdown finish while explicitly reporting unresolved create work.

## SDK and cancellation evidence

The adapter pins the public `@getpaseo/client` 0.7.2 API: `createPaseoClient`,
`connect`, `close`, `providers.waitForReady`, `agents.create`, `agents.ref`, agent
`refresh`, agent `run` with `outputSchema`, agent snapshot `subscribe`, and timeline
`subscribe`.

The high-level public SDK has no remote interrupt method. The installed supported
Paseo 0.7.2 CLI documents `paseo stop <id> --json` and implements it through the
daemon's cancellation operation. The bridge invokes that executable with
`execFile` and an argument array, a 64 KiB output cap, and a bounded timeout. It
passes an explicit `--host` derived from the matching local SDK URL (or explicit
configuration), probes the executable before advertising remote cancellation,
and does not import private SDK paths or construct private wire messages. When
the CLI is unavailable or cancellation fails, the bridge reports the limitation,
fences the observer, and retains remote-busy state until an authoritative idle,
error, or closed snapshot arrives. A cancellation command that exceeds the bridge
response deadline continues to hold the generation lock until the bounded CLI
process itself settles, preventing a late stop from targeting a newer turn.

## Controlled live proof

On 2026-09-05, the built adapter connected to local Paseo daemon 0.7.2 and used the
configured `GPT-5.6-Sol-Implementer` settings exactly:

```text
provider: codex/gpt-5.6-sol
mode_id: full-access
thinking_option_id: medium
```

Session `549d4f82-4dd7-4aae-8911-10e5462d95c7` inspected the local synthetic
manifest and dataset, streamed events, received the concrete filter output schema,
and returned a proposal that passed the bridge's runtime validator:

```json
{"kind":"filter","definition":{"schema_version":1,"expression":"pl.col('level') == 'error'"},"explanation":"Select only events whose parsed level field equals error.","originating_revision":{"data":"high-watermark-3","definition":"view-revision-1"}}
```

The returned revisions exactly matched the request. The controlled session was
archived at `2026-09-05T06:57:48.674Z`. No other provider or authentication path
was tested during this proof.

A separate cancellation-only proof used the same configured profile. Session
`d88ebfe6-9e6f-4ecb-a577-7142cd6c0764` reached a running `sleep 30` tool call;
the bridge then executed the supported CLI stop against explicit host
`127.0.0.1:6767` and returned `remote_cancelled:true` with
`remote_agent_may_still_be_running:false`. Observation was fenced immediately.
The session was archived at `2026-09-05T07:09:49.378Z`.
