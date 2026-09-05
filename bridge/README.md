# lvu Paseo bridge

This process adapts the public `@getpaseo/client` 0.7.2 API to lvu's versioned
JSON Lines contract. It writes protocol objects only to stdout and diagnostics to
stderr. The normal entry point is `npm run build && node dist/cli.js` from
this directory.

Supported requests are `capabilities`, `start_session`, `send_prompt`, `cancel`,
`resume_session`, and `request_proposal`. Paseo 0.7.2's public high-level SDK
supports agent create/ref/refresh, `run`, snapshot and timeline subscriptions,
provider discovery, output schemas, and clean client close. It does **not** expose
remote interruption or permission resolution. For interruption, the adapter uses
the installed supported `paseo stop <session-id> --json` CLI with an argument array
and no shell. If it is disabled or fails, cancellation still settles and fences
the local observer but reports that the remote agent may still be running; the
session remains busy until its SDK run settles. Permission-bearing updates are
forwarded only while a turn is actively observed.

Environment limits:

- `LVU_PASEO_URL` (default `ws://127.0.0.1:6767/ws`)
- `LVU_PASEO_CONNECT_TIMEOUT_MS` (default 10000)
- `LVU_PASEO_TIMEOUT_MS` (default 120000)
- `LVU_PASEO_CANCEL_TIMEOUT_MS` (default 5000)
- `LVU_PASEO_MAX_SESSIONS` (default 8)
- `LVU_PASEO_MAX_LINE_BYTES`, `LVU_PASEO_MAX_PROPOSAL_BYTES`, and
  `LVU_PASEO_MAX_EVENT_BYTES` (default 262144)
- `LVU_PASEO_MAX_IN_FLIGHT` (default 16), with
  `LVU_PASEO_MAX_CANCEL_IN_FLIGHT` (default 4) reserved for cancellation
- `LVU_PASEO_MAX_QUEUED_OUTPUT_BYTES` (default 1048576)
- `LVU_PASEO_SHUTDOWN_DRAIN_TIMEOUT_MS` (default 2000)

Set `LVU_PASEO_CLI=disabled` when the supported CLI fallback must not be used.
For the default local WebSocket URL, the CLI receives an explicit matching
`--host 127.0.0.1:6767`. Set `LVU_PASEO_CLI_HOST` explicitly for another
supported CLI endpoint form. Secure/nonstandard SDK URLs do not advertise remote
cancellation unless that explicit CLI host is supplied and the executable probe
succeeds.
Session creation accepts provider-neutral `mode_id` and `thinking_option_id`; for
the configured implementation profile these are `codex/gpt-5.6-sol`,
`full-access`, and `medium` respectively.

Suggested root mise tasks call `npm --prefix bridge ci`, `npm --prefix bridge run
typecheck`, `npm --prefix bridge test`, and `npm --prefix bridge run build`.
